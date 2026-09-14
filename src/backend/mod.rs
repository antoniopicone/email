//! The mail backend.
//!
//! Networking never runs on the GTK main loop. Each account gets a worker
//! thread that owns its own IMAP session; the UI sends [`Command`]s down an
//! `std::sync::mpsc` channel and receives [`Event`]s through an async channel
//! that the GLib main context drains.

pub mod imap_client;
pub mod smtp;
pub mod xoauth2;

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::demo;
use crate::model::{Account, AccountSource, Mailbox, Message, MessageSummary};
use imap_client::ImapClient;

/// Work requested by the UI.
#[derive(Debug, Clone)]
pub enum Command {
    LoadMailboxes,
    LoadMessages { mailbox: String, limit: u32, offset: u32 },
    LoadMessage { mailbox: String, uid: u32 },
    SetFlag { mailbox: String, uid: u32, flag: String, on: bool },
    MoveMessage { mailbox: String, uid: u32, target: String },
    DeleteMessage { mailbox: String, uid: u32 },
    Send { outgoing: smtp::Outgoing },
    Shutdown,
}

/// Where an account's connection stands, for the sidebar's status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Online,
    Offline,
}

/// Results pushed back to the UI.
#[derive(Debug)]
pub enum Event {
    Status { account_id: String, state: ConnectionState, detail: String },
    Mailboxes { account_id: String, mailboxes: Vec<Mailbox> },
    Messages {
        account_id: String,
        mailbox: String,
        messages: Vec<MessageSummary>,
        append: bool,
    },
    MessageLoaded { account_id: String, message: Box<Message> },
    FlagChanged { account_id: String, mailbox: String, uid: u32, flag: String, on: bool },
    MessageRemoved { account_id: String, mailbox: String, uid: u32 },
    Sent { account_id: String },
    Error { account_id: String, context: String, detail: String },
}

/// The UI-side handle: one sender per account plus the shared event stream.
pub struct Backend {
    senders: HashMap<String, mpsc::Sender<Command>>,
    event_tx: async_channel::Sender<Event>,
    events: async_channel::Receiver<Event>,
}

impl Backend {
    pub fn new() -> Self {
        let (event_tx, events) = async_channel::unbounded();
        Self { senders: HashMap::new(), event_tx, events }
    }

    /// The receiver the UI drains from the GLib main context.
    pub fn events(&self) -> async_channel::Receiver<Event> {
        self.events.clone()
    }

    /// Start a worker thread for `account`.
    pub fn add_account(&mut self, account: Account) {
        let (tx, rx) = mpsc::channel();
        let events = self.event_tx.clone();
        let id = account.id.clone();
        let name = format!("mail-{}", account.email);

        let spawned = thread::Builder::new().name(name).spawn(move || {
            if account.source == AccountSource::Demo {
                demo::run_worker(account, rx, events);
            } else {
                run_worker(account, rx, events);
            }
        });

        match spawned {
            Ok(_) => {
                self.senders.insert(id, tx);
            }
            Err(e) => {
                log::error!("could not start a worker for {id}: {e}");
                let _ = self.event_tx.send_blocking(Event::Error {
                    account_id: id,
                    context: "avvio del worker".into(),
                    detail: e.to_string(),
                });
            }
        }
    }

    /// Queue a command for one account. Unknown accounts are ignored.
    pub fn send(&self, account_id: &str, command: Command) {
        let Some(tx) = self.senders.get(account_id) else {
            log::warn!("no worker for account {account_id}");
            return;
        };
        if let Err(e) = tx.send(command) {
            log::warn!("worker for {account_id} is gone: {e}");
        }
    }

    /// Queue the same command for every account.
    pub fn broadcast(&self, command: Command) {
        for (id, tx) in &self.senders {
            if let Err(e) = tx.send(command.clone()) {
                log::warn!("worker for {id} is gone: {e}");
            }
        }
    }

    pub fn shutdown(&self) {
        self.broadcast(Command::Shutdown);
    }
}

impl Default for Backend {
    fn default() -> Self {
        Self::new()
    }
}

/// The per-account worker loop.
///
/// The IMAP session is created lazily on the first command and rebuilt when a
/// command fails, so a dropped connection recovers on the next user action
/// instead of leaving the account dead until restart.
fn run_worker(
    account: Account,
    rx: mpsc::Receiver<Command>,
    events: async_channel::Sender<Event>,
) {
    let mut client: Option<ImapClient> = None;
    let emit = |event: Event| {
        let _ = events.send_blocking(event);
    };

    while let Ok(command) = rx.recv() {
        if matches!(command, Command::Shutdown) {
            if let Some(mut c) = client.take() {
                c.logout();
            }
            break;
        }

        // (Re)connect if needed.
        if client.is_none() {
            emit(Event::Status {
                account_id: account.id.clone(),
                state: ConnectionState::Connecting,
                detail: format!("Connessione a {}…", account.imap_host),
            });

            match connect_with_retry(&account) {
                Ok(c) => {
                    client = Some(c);
                    emit(Event::Status {
                        account_id: account.id.clone(),
                        state: ConnectionState::Online,
                        detail: "Connesso".into(),
                    });
                }
                Err(e) => {
                    emit(Event::Status {
                        account_id: account.id.clone(),
                        state: ConnectionState::Offline,
                        detail: "Non connesso".into(),
                    });
                    emit(Event::Error {
                        account_id: account.id.clone(),
                        context: format!("connessione a {}", account.imap_host),
                        detail: format!("{e:#}"),
                    });
                    continue;
                }
            }
        }

        let Some(session) = client.as_mut() else { continue };

        if let Err(e) = handle_command(&account, session, &command, &emit) {
            log::warn!("command failed for {}: {e:#}", account.id);
            emit(Event::Error {
                account_id: account.id.clone(),
                context: describe(&command),
                detail: format!("{e:#}"),
            });
            // Drop the session so the next command reconnects.
            client = None;
            emit(Event::Status {
                account_id: account.id.clone(),
                state: ConnectionState::Offline,
                detail: "Riconnessione al prossimo comando".into(),
            });
        }
    }

    log::info!("worker for {} stopped", account.id);
}

/// Connect, retrying briefly: OAuth tokens occasionally need a second attempt
/// right after GOA refreshes them.
fn connect_with_retry(account: &Account) -> anyhow::Result<ImapClient> {
    let mut last_error = None;
    for attempt in 0..3 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(500 * attempt));
        }
        let credentials = match imap_client::resolve_credentials(account, crate::goa::CredentialPurpose::Imap) {
            Ok(c) => c,
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        };
        match ImapClient::connect(account, &credentials) {
            Ok(client) => return Ok(client),
            Err(e) => {
                log::debug!("connection attempt {} failed: {e:#}", attempt + 1);
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("connessione non riuscita")))
}

fn handle_command(
    account: &Account,
    session: &mut ImapClient,
    command: &Command,
    emit: &impl Fn(Event),
) -> anyhow::Result<()> {
    match command {
        Command::Shutdown => Ok(()),

        Command::LoadMailboxes => {
            let mailboxes = session.list_mailboxes()?;
            emit(Event::Mailboxes { account_id: account.id.clone(), mailboxes });
            Ok(())
        }

        Command::LoadMessages { mailbox, limit, offset } => {
            let messages = session.list_messages(mailbox, *limit, *offset)?;
            emit(Event::Messages {
                account_id: account.id.clone(),
                mailbox: mailbox.clone(),
                messages,
                append: *offset > 0,
            });
            Ok(())
        }

        Command::LoadMessage { mailbox, uid } => {
            let message = session.load_message(mailbox, *uid)?;
            emit(Event::MessageLoaded {
                account_id: account.id.clone(),
                message: Box::new(message),
            });
            Ok(())
        }

        Command::SetFlag { mailbox, uid, flag, on } => {
            session.set_flag(mailbox, *uid, flag, *on)?;
            emit(Event::FlagChanged {
                account_id: account.id.clone(),
                mailbox: mailbox.clone(),
                uid: *uid,
                flag: flag.clone(),
                on: *on,
            });
            Ok(())
        }

        Command::MoveMessage { mailbox, uid, target } => {
            session.move_message(mailbox, *uid, target)?;
            emit(Event::MessageRemoved {
                account_id: account.id.clone(),
                mailbox: mailbox.clone(),
                uid: *uid,
            });
            Ok(())
        }

        Command::Send { outgoing } => {
            let credentials =
                imap_client::resolve_credentials(account, crate::goa::CredentialPurpose::Smtp)?;
            let raw = smtp::send(account, &credentials, outgoing)?;
            // Best effort: a failed Sent copy must not look like a failed send.
            if let Err(e) = session.append_to_sent(&raw) {
                log::warn!("could not file the sent copy: {e:#}");
            }
            emit(Event::Sent { account_id: account.id.clone() });
            Ok(())
        }

        Command::DeleteMessage { mailbox, uid } => {
            // Prefer moving to Trash; only fall back to a hard delete when the
            // server has no trash folder at all.
            match session.trash_folder() {
                Some(trash) if trash != *mailbox => {
                    session.move_message(mailbox, *uid, &trash)?;
                }
                _ => {
                    session.set_flag(mailbox, *uid, "\\Deleted", true)?;
                }
            }
            emit(Event::MessageRemoved {
                account_id: account.id.clone(),
                mailbox: mailbox.clone(),
                uid: *uid,
            });
            Ok(())
        }
    }
}

fn describe(command: &Command) -> String {
    match command {
        Command::LoadMailboxes => "caricamento delle cartelle".into(),
        Command::LoadMessages { mailbox, .. } => format!("caricamento di {mailbox}"),
        Command::LoadMessage { uid, .. } => format!("apertura del messaggio {uid}"),
        Command::SetFlag { flag, .. } => format!("aggiornamento del flag {flag}"),
        Command::MoveMessage { target, .. } => format!("spostamento in {target}"),
        Command::DeleteMessage { .. } => "eliminazione del messaggio".into(),
        Command::Send { .. } => "invio del messaggio".into(),
        Command::Shutdown => "chiusura".into(),
    }
}
