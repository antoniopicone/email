//! The synchronous IMAP session used by the per-account worker threads.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{Local, TimeZone};
use mail_parser::{Address, MessageParser, MimeHeaders};
use native_tls::{TlsConnector, TlsStream};

use super::xoauth2::XOAuth2;
use crate::model::{
    Account, Attachment, Credentials, Mailaddr, Mailbox, MailboxKind, Message, MessageSummary,
};

type Session = imap::Session<TlsStream<TcpStream>>;

/// Bounds how long the initial TCP handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bounds every subsequent read/write on the socket. Without this, a
/// connection that stops answering mid-command (a dropped packet a firewall
/// or a broken IPv6 path swallows, say) blocks the worker thread forever:
/// the UI would keep showing that account as "syncing" indefinitely, with no
/// error and no way to recover short of restarting the app.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Open a TCP connection with both a connect and an I/O deadline, so a
/// server or network that stops responding fails loudly instead of hanging
/// the worker thread.
fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let addr = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}:{port}"))?
        .next()
        .ok_or_else(|| anyhow!("no address found for {host}:{port}"))?;
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .with_context(|| format!("connecting to {host}:{port}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).context("setting the socket read timeout")?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).context("setting the socket write timeout")?;
    Ok(stream)
}

/// Fields we ask for when building the message list. Everything here is cheap
/// except the leading kilobyte of body text, which gives us the list preview
/// without a second round trip.
const LIST_QUERY: &str =
    "(UID FLAGS ENVELOPE INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[TEXT]<0.1024>)";

pub struct ImapClient {
    account: Account,
    session: Session,
    delimiter: char,
}

impl ImapClient {
    /// Open a TLS connection and authenticate.
    pub fn connect(account: &Account, credentials: &Credentials) -> Result<Self> {
        let tls = TlsConnector::builder()
            .build()
            .context("initialising TLS")?;

        let host = account.imap_host.as_str();
        let port = account.imap_port;

        log::info!(
            "connecting to {host}:{port} as {} using {}",
            account.imap_user,
            credentials.mechanism()
        );

        let tcp = connect_tcp(host, port)?;

        let client = if account.use_starttls {
            let mut plain = imap::Client::new(tcp);
            plain
                .read_greeting()
                .with_context(|| format!("reading the greeting from {host}:{port}"))?;
            plain
                .secure(host, &tls)
                .map_err(|e| anyhow!("STARTTLS negotiation with {host}:{port} failed: {e}"))?
        } else {
            let tls_stream = tls
                .connect(host, tcp)
                .with_context(|| format!("TLS handshake with {host}:{port}"))?;
            let mut socket = imap::Client::new(tls_stream);
            socket
                .read_greeting()
                .with_context(|| format!("reading the greeting from {host}:{port}"))?;
            socket
        };

        let session = match credentials {
            Credentials::OAuth2(token) => {
                let auth = XOAuth2::new(&account.imap_user, token);
                client.authenticate("XOAUTH2", &auth).map_err(|(e, _)| {
                    anyhow!("XOAUTH2 authentication failed for {}: {e}", account.email)
                })?
            }
            Credentials::Password(password) => {
                client.login(&account.imap_user, password).map_err(|(e, _)| {
                    anyhow!("login failed for {}: {e}", account.email)
                })?
            }
        };

        log::info!("authenticated as {}", account.email);

        Ok(Self {
            account: account.clone(),
            session,
            delimiter: '/',
        })
    }

    /// List the folders on the server, with unread counts.
    pub fn list_mailboxes(&mut self) -> Result<Vec<Mailbox>> {
        let names = self.session.list(Some(""), Some("*")).context("listing mailboxes")?;

        let mut raw: Vec<(String, Vec<String>)> = Vec::new();
        for name in names.iter() {
            if let Some(delim) = name.delimiter().and_then(|d| d.chars().next()) {
                self.delimiter = delim;
            }
            let attributes: Vec<String> =
                name.attributes().iter().map(|a| format!("{a:?}")).collect();

            // `\Noselect` folders are pure containers, e.g. Gmail's `[Gmail]`.
            let selectable = !attributes.iter().any(|a| a.contains("NoSelect"));
            if !selectable {
                continue;
            }
            raw.push((name.name().to_string(), attributes));
        }

        let delimiter = self.delimiter;
        let mut mailboxes = Vec::new();

        for (path, attributes) in raw {
            // `format!("{:?}")` on NameAttribute renders `Custom("\\Sent")`, so
            // pull the inner token back out for classification.
            let cleaned: Vec<String> = attributes
                .iter()
                .map(|a| {
                    a.trim_start_matches("Custom(")
                        .trim_end_matches(')')
                        .trim_matches('"')
                        .replace("\\\\", "\\")
                        .to_string()
                })
                .collect();

            let kind = MailboxKind::classify(&path, &cleaned);
            let depth = path.matches(delimiter).count();
            let leaf = path.rsplit(delimiter).next().unwrap_or(&path).to_string();

            let (unread, total) = self.folder_counts(&path);

            mailboxes.push(Mailbox {
                account_id: self.account.id.clone(),
                path: path.clone(),
                name: leaf,
                kind,
                unread,
                total,
                depth,
            });
        }

        // Standard mailboxes first in Apple Mail's order, then the rest
        // alphabetically.
        mailboxes.sort_by(|a, b| {
            a.kind
                .order()
                .cmp(&b.kind.order())
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });

        Ok(mailboxes)
    }

    /// STATUS is cheap and does not disturb the selected mailbox.
    fn folder_counts(&mut self, path: &str) -> (u32, u32) {
        match self.session.status(path, "(MESSAGES UNSEEN)") {
            Ok(status) => (status.unseen.unwrap_or(0), status.exists),
            Err(e) => {
                log::debug!("STATUS failed for {path}: {e}");
                (0, 0)
            }
        }
    }

    /// Always re-issues SELECT, even when `mailbox` is already open.
    ///
    /// An earlier version skipped straight to STATUS when the mailbox was
    /// already selected, to save a round trip. STATUS on the *currently
    /// selected* mailbox is explicitly discouraged by RFC 3501 §6.3.10, and
    /// some servers answer it with a stale or zero EXISTS count instead of
    /// the real one — which made the message list "lose" every message in
    /// the folder on a refresh. SELECT always gives an accurate count.
    fn select(&mut self, mailbox: &str) -> Result<u32> {
        let meta = self
            .session
            .select(mailbox)
            .with_context(|| format!("selecting {mailbox}"))?;
        Ok(meta.exists)
    }

    /// Fetch the newest `limit` messages of `mailbox`, skipping the newest
    /// `offset` (used for paging further back in history).
    pub fn list_messages(
        &mut self,
        mailbox: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageSummary>> {
        let exists = self.select(mailbox)?;
        if exists == 0 {
            return Ok(Vec::new());
        }

        let end = exists.saturating_sub(offset);
        if end == 0 {
            return Ok(Vec::new());
        }
        let start = end.saturating_sub(limit.saturating_sub(1)).max(1);
        let sequence = format!("{start}:{end}");

        let fetches = self
            .session
            .fetch(&sequence, LIST_QUERY)
            .with_context(|| format!("fetching {sequence} from {mailbox}"))?;

        let mut summaries: Vec<MessageSummary> = fetches
            .iter()
            .filter_map(|f| self.summary_from_fetch(mailbox, f))
            .collect();

        // Newest first, the way a mail client shows a folder.
        summaries.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(summaries)
    }

    fn summary_from_fetch(&self, mailbox: &str, fetch: &imap::types::Fetch) -> Option<MessageSummary> {
        let uid = fetch.uid?;
        let envelope = fetch.envelope();

        let subject = envelope
            .and_then(|e| e.subject.as_ref())
            .map(|s| decode_header_bytes(s))
            .unwrap_or_default();

        let from = envelope
            .and_then(|e| e.from.as_ref())
            .and_then(|list| list.first())
            .map(address_from_imap)
            .unwrap_or_default();

        let to = envelope
            .and_then(|e| e.to.as_ref())
            .map(|list| list.iter().map(address_from_imap).collect())
            .unwrap_or_default();

        let date = fetch
            .internal_date()
            .map(|d| d.with_timezone(&Local))
            .or_else(|| {
                envelope
                    .and_then(|e| e.date.as_ref())
                    .and_then(|d| parse_rfc2822(&String::from_utf8_lossy(d)))
            })
            .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap_or_else(Local::now));

        let flags: Vec<String> = fetch.flags().iter().map(|f| format!("{f:?}")).collect();
        let has = |needle: &str| flags.iter().any(|f| f.contains(needle));

        let snippet = fetch
            .text()
            .map(|raw| snippet_from_partial_body(raw))
            .unwrap_or_default();

        let has_attachments = fetch
            .bodystructure()
            .map(bodystructure_has_attachment)
            .unwrap_or(false);

        Some(MessageSummary {
            account_id: self.account.id.clone(),
            mailbox: mailbox.to_string(),
            uid,
            from,
            to,
            subject,
            snippet,
            date,
            seen: has("Seen"),
            flagged: has("Flagged"),
            answered: has("Answered"),
            has_attachments,
        })
    }

    /// Fetch and parse one full message.
    pub fn load_message(&mut self, mailbox: &str, uid: u32) -> Result<Message> {
        self.select(mailbox)?;

        let fetches = self
            .session
            .uid_fetch(uid.to_string(), "(UID FLAGS INTERNALDATE RFC822)")
            .with_context(|| format!("fetching message {uid} from {mailbox}"))?;

        let fetch = fetches
            .iter()
            .next()
            .ok_or_else(|| anyhow!("message {uid} not found in {mailbox}"))?;

        let raw = fetch
            .body()
            .ok_or_else(|| anyhow!("message {uid} came back without a body"))?;

        let parsed = MessageParser::default()
            .parse(raw)
            .ok_or_else(|| anyhow!("could not parse message {uid}"))?;

        let from = parsed
            .from()
            .and_then(first_address)
            .unwrap_or_default();
        let to = parsed.to().map(all_addresses).unwrap_or_default();
        let cc = parsed.cc().map(all_addresses).unwrap_or_default();

        let text = parsed.body_text(0).map(|t| t.into_owned()).unwrap_or_default();
        let html_raw = parsed.body_html(0).map(|h| h.into_owned());

        let attachments: Vec<Attachment> = parsed
            .attachments()
            .map(|part| {
                let data = part.contents().to_vec();
                Attachment {
                    filename: part
                        .attachment_name()
                        .unwrap_or("allegato")
                        .to_string(),
                    mime_type: part
                        .content_type()
                        .map(|ct| match ct.subtype() {
                            Some(sub) => format!("{}/{}", ct.ctype(), sub),
                            None => ct.ctype().to_string(),
                        })
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    size: data.len(),
                    data,
                }
            })
            .collect();

        let date = parsed
            .date()
            .and_then(|d| {
                Local
                    .timestamp_opt(d.to_timestamp(), 0)
                    .single()
            })
            .or_else(|| fetch.internal_date().map(|d| d.with_timezone(&Local)))
            .unwrap_or_else(Local::now);

        let flags: Vec<String> = fetch.flags().iter().map(|f| format!("{f:?}")).collect();
        let has = |needle: &str| flags.iter().any(|f| f.contains(needle));

        let subject = parsed.subject().unwrap_or_default().to_string();
        let snippet = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();

        let summary = MessageSummary {
            account_id: self.account.id.clone(),
            mailbox: mailbox.to_string(),
            uid,
            from,
            to,
            subject,
            snippet,
            date,
            seen: has("Seen"),
            flagged: has("Flagged"),
            answered: has("Answered"),
            has_attachments: !attachments.is_empty(),
        };

        Ok(Message {
            summary,
            cc,
            html: html_raw.map(|h| crate::html::sanitize(&h)),
            text,
            attachments,
        })
    }

    /// Add or remove a flag on a message.
    pub fn set_flag(&mut self, mailbox: &str, uid: u32, flag: &str, on: bool) -> Result<()> {
        self.select(mailbox)?;
        let op = if on { "+FLAGS" } else { "-FLAGS" };
        self.session
            .uid_store(uid.to_string(), format!("{op} ({flag})"))
            .with_context(|| format!("setting {flag} on message {uid}"))?;
        Ok(())
    }

    /// Move a message to another folder, falling back to copy+delete on
    /// servers without the MOVE extension.
    pub fn move_message(&mut self, mailbox: &str, uid: u32, target: &str) -> Result<()> {
        self.select(mailbox)?;
        match self.session.uid_mv(uid.to_string(), target) {
            Ok(()) => Ok(()),
            Err(e) => {
                log::debug!("UID MOVE unsupported ({e}), falling back to COPY+STORE");
                self.session
                    .uid_copy(uid.to_string(), target)
                    .with_context(|| format!("copying message {uid} to {target}"))?;
                self.session
                    .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
                    .with_context(|| format!("marking message {uid} deleted"))?;
                self.session.expunge().context("expunging after a copy-move")?;
                Ok(())
            }
        }
    }

    /// File a copy of a just-sent message in the Sent folder.
    ///
    /// Servers do not do this for us, and a sent message that only exists in
    /// the recipient's mailbox is a surprising thing to hand a user.
    pub fn append_to_sent(&mut self, raw: &[u8]) -> Result<()> {
        let sent = self
            .list_mailboxes()?
            .into_iter()
            .find(|m| m.kind == MailboxKind::Sent)
            .map(|m| m.path);

        let Some(sent) = sent else {
            log::info!("no Sent folder on this account, skipping the copy");
            return Ok(());
        };

        self.session
            .append_with_flags(&sent, raw, &[imap::types::Flag::Seen])
            .with_context(|| format!("appending the sent copy to {sent}"))?;

        Ok(())
    }

    /// The folder a "delete" should move to, if there is one.
    pub fn trash_folder(&mut self) -> Option<String> {
        self.list_mailboxes()
            .ok()?
            .into_iter()
            .find(|m| m.kind == MailboxKind::Trash)
            .map(|m| m.path)
    }

    pub fn logout(&mut self) {
        let _ = self.session.logout();
    }
}

fn address_from_imap(addr: &imap_proto::Address) -> Mailaddr {
    let name = addr.name.as_ref().map(|n| decode_header_bytes(n)).unwrap_or_default();
    let mailbox = addr.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).into_owned());
    let host = addr.host.as_ref().map(|h| String::from_utf8_lossy(h).into_owned());
    let address = match (mailbox, host) {
        (Some(m), Some(h)) => format!("{m}@{h}"),
        (Some(m), None) => m,
        _ => String::new(),
    };
    Mailaddr { name, address }
}

fn first_address(address: &Address) -> Option<Mailaddr> {
    all_addresses(address).into_iter().next()
}

fn all_addresses(address: &Address) -> Vec<Mailaddr> {
    match address {
        Address::List(list) => list
            .iter()
            .map(|a| Mailaddr {
                name: a.name.as_deref().unwrap_or_default().to_string(),
                address: a.address.as_deref().unwrap_or_default().to_string(),
            })
            .collect(),
        Address::Group(groups) => groups
            .iter()
            .flat_map(|g| g.addresses.iter())
            .map(|a| Mailaddr {
                name: a.name.as_deref().unwrap_or_default().to_string(),
                address: a.address.as_deref().unwrap_or_default().to_string(),
            })
            .collect(),
    }
}

/// Decode an RFC 2047 encoded-word header, e.g. `=?UTF-8?B?...?=`.
///
/// mail-parser handles this for full messages; ENVELOPE gives us raw bytes, so
/// we run them back through the parser as a synthetic Subject header.
fn decode_header_bytes(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    if !raw.contains("=?") {
        return raw.into_owned();
    }
    let synthetic = format!("Subject: {raw}\r\n\r\n");
    MessageParser::default()
        .parse(synthetic.as_bytes())
        .and_then(|m| m.subject().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.into_owned())
}

fn parse_rfc2822(value: &str) -> Option<chrono::DateTime<Local>> {
    chrono::DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|d| d.with_timezone(&Local))
}

/// Turn the first kilobyte of a raw body into a one-line list preview.
fn snippet_from_partial_body(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);

    // The partial fetch starts at the body of the first MIME part, which may
    // still carry part headers; drop everything up to the first blank line.
    let body = match text.find("\r\n\r\n") {
        Some(idx) => &text[idx + 4..],
        None => match text.find("\n\n") {
            Some(idx) => &text[idx + 2..],
            None => text.as_ref(),
        },
    };

    let decoded = decode_quoted_printable(body);
    let stripped = crate::html::to_plain_text(&decoded);

    stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// A forgiving quoted-printable decoder for preview text only.
fn decode_quoted_printable(input: &str) -> String {
    if !input.contains('=') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            let hex = &input[i + 1..i + 3];
            if hex == "\r\n" || hex.starts_with('\n') {
                i += if hex.starts_with('\n') { 2 } else { 3 };
                continue;
            }
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Walk a BODYSTRUCTURE looking for a part with a filename or an
/// `attachment` disposition.
fn bodystructure_has_attachment(body: &imap_proto::BodyStructure) -> bool {
    use imap_proto::BodyStructure;

    fn disposition_is_attachment(disposition: &Option<imap_proto::ContentDisposition>) -> bool {
        let Some(d) = disposition else { return false };
        if d.ty.to_ascii_lowercase().contains("attachment") {
            return true;
        }
        // Some servers only signal an attachment through a filename parameter.
        d.params
            .as_ref()
            .map(|params| params.iter().any(|(key, _)| key.eq_ignore_ascii_case("filename")))
            .unwrap_or(false)
    }

    match body {
        BodyStructure::Multipart { bodies, .. } => bodies.iter().any(bodystructure_has_attachment),
        BodyStructure::Basic { common, .. }
        | BodyStructure::Message { common, .. }
        | BodyStructure::Text { common, .. } => disposition_is_attachment(&common.disposition),
    }
}

/// Resolve credentials for an account, from GOA or the keyring.
pub fn resolve_credentials(account: &Account) -> Result<Credentials> {
    use crate::model::AccountSource;
    match account.source {
        AccountSource::Gnome => crate::goa::credentials_blocking(account),
        AccountSource::Manual => {
            let password = crate::secrets::lookup_password_blocking(&account.id)
                .context("reading the password from the keyring")?
                .ok_or_else(|| {
                    anyhow!("no password saved for {}; add it in the account settings", account.email)
                })?;
            Ok(Credentials::Password(password))
        }
        AccountSource::Demo => Err(anyhow!("demo accounts do not connect to a server")),
    }
}

