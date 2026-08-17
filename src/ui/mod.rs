//! The application window.

pub mod accounts;
pub mod compose;
pub mod message_view;
pub mod rows;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::backend::{Backend, Command, ConnectionState, Event};
use compose::ComposeKind;
use crate::config::{Config, ThemePreference};
use crate::model::{Account, Mailbox, MailboxKind, Message, MessageSummary};
use message_view::MessageView;

const APP_ID: &str = "it.antoniopicone.MailView";

/// Everything the UI needs to know that is not held by a widget.
#[derive(Default)]
struct State {
    accounts: Vec<Account>,
    mailboxes: HashMap<String, Vec<Mailbox>>,
    status: HashMap<String, (ConnectionState, String)>,
    /// Parallel to the sidebar's rows: which mailbox each row selects.
    sidebar_index: Vec<Option<(String, String)>>,
    current_folder: Option<(String, String)>,
    /// Every message loaded for the current folder, newest first.
    messages: Vec<MessageSummary>,
    /// Indices into `messages` that survive the current search.
    visible: Vec<usize>,
    current_message: Option<Message>,
    search: String,
    dark: bool,
    /// Set once the user picks a mailbox themselves, so later arrivals from
    /// slower accounts stop moving them around.
    user_picked_folder: bool,
    /// True while we are driving a selection ourselves.
    selecting_programmatically: bool,
}

impl State {
    fn selected_summary(&self, row_index: usize) -> Option<&MessageSummary> {
        self.visible.get(row_index).and_then(|i| self.messages.get(*i))
    }

    fn mailbox(&self, account_id: &str, path: &str) -> Option<&Mailbox> {
        self.mailboxes.get(account_id)?.iter().find(|m| m.path == path)
    }
}

struct Widgets {
    window: adw::ApplicationWindow,
    sidebar_list: gtk::ListBox,
    message_list: gtk::ListBox,
    message_view: MessageView,
    folder_title: adw::WindowTitle,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    list_stack: gtk::Stack,
    banner: adw::Banner,
    toast_overlay: adw::ToastOverlay,
    flag_button: gtk::ToggleButton,
    action_buttons: Vec<gtk::Widget>,
    spinner: gtk::Spinner,
}

/// The main window and its wiring.
pub struct App {
    widgets: Rc<Widgets>,
    state: Rc<RefCell<State>>,
    backend: Rc<RefCell<Backend>>,
    config: Rc<RefCell<Config>>,
}

impl App {
    pub fn build(
        application: &adw::Application,
        accounts: Vec<Account>,
        config: Config,
    ) -> Rc<Self> {
        let state = Rc::new(RefCell::new(State {
            dark: adw::StyleManager::default().is_dark(),
            accounts: accounts.clone(),
            ..Default::default()
        }));
        let config = Rc::new(RefCell::new(config));

        let mut backend = Backend::new();
        for account in &accounts {
            backend.add_account(account.clone());
        }
        let events = backend.events();
        let backend = Rc::new(RefCell::new(backend));

        let widgets = Rc::new(build_widgets(application));
        let app = Rc::new(Self {
            widgets: widgets.clone(),
            state: state.clone(),
            backend: backend.clone(),
            config: config.clone(),
        });

        app.connect_signals();
        app.install_actions(application);
        app.rebuild_sidebar();

        // Ask every account for its folder list; the first inbox that arrives
        // is selected automatically.
        backend.borrow().broadcast(Command::LoadMailboxes);

        // Drain backend events on the main loop.
        {
            let app = app.clone();
            glib::spawn_future_local(async move {
                while let Ok(event) = events.recv().await {
                    app.handle_event(event);
                }
            });
        }

        // Follow the system light/dark preference.
        {
            let app = app.clone();
            adw::StyleManager::default().connect_dark_notify(move |manager| {
                app.state.borrow_mut().dark = manager.is_dark();
                app.rerender_body();
            });
        }

        if accounts.is_empty() {
            widgets.banner.set_title(
                "Nessun account configurato. Aggiungine uno in Impostazioni → Account online, \
                 oppure avvia con --demo per esplorare l'interfaccia.",
            );
            widgets.banner.set_revealed(true);
        }

        app
    }

    pub fn present(&self) {
        self.widgets.window.present();
    }

    // ------------------------------------------------------------- signals

    fn connect_signals(self: &Rc<Self>) {
        // Sidebar: pick a mailbox.
        {
            let app = self.clone();
            self.widgets.sidebar_list.connect_row_selected(move |_, row| {
                let Some(row) = row else { return };
                let index = row.index() as usize;
                let (target, programmatic) = {
                    let state = app.state.borrow();
                    (
                        state.sidebar_index.get(index).cloned().flatten(),
                        state.selecting_programmatically,
                    )
                };
                if !programmatic {
                    app.state.borrow_mut().user_picked_folder = true;
                }
                if let Some((account_id, path)) = target {
                    app.open_folder(&account_id, &path);
                }
            });
        }

        // Message list: open a message.
        {
            let app = self.clone();
            self.widgets.message_list.connect_row_selected(move |_, row| {
                let Some(row) = row else {
                    app.widgets.message_view.show_empty();
                    app.set_actions_enabled(false);
                    return;
                };
                app.open_message(row.index() as usize);
            });
        }

        // Search.
        {
            let app = self.clone();
            self.widgets.search_entry.connect_search_changed(move |entry| {
                app.state.borrow_mut().search = entry.text().to_string();
                app.refresh_message_list();
            });
        }

        // Flag toggle.
        {
            let app = self.clone();
            self.widgets.flag_button.connect_toggled(move |button| {
                // Ignore the programmatic updates we make when selecting a row.
                if button.is_sensitive() {
                    app.set_flag_on_selection("\\Flagged", button.is_active());
                }
            });
        }

        let window = self.widgets.window.clone();
        let backend = self.backend.clone();
        window.connect_close_request(move |_| {
            backend.borrow().shutdown();
            glib::Propagation::Proceed
        });
    }

    fn install_actions(self: &Rc<Self>, application: &adw::Application) {
        let group = gio::SimpleActionGroup::new();

        let add = |name: &str, callback: Box<dyn Fn()>| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| callback());
            group.add_action(&action);
        };

        {
            let app = self.clone();
            add("refresh", Box::new(move || app.refresh_current_folder()));
        }
        {
            let app = self.clone();
            add("archive", Box::new(move || app.move_selection_to(MailboxKind::Archive)));
        }
        {
            let app = self.clone();
            add("delete", Box::new(move || app.delete_selection()));
        }
        {
            let app = self.clone();
            add("junk", Box::new(move || app.move_selection_to(MailboxKind::Junk)));
        }
        {
            let app = self.clone();
            add("toggle-read", Box::new(move || app.toggle_read_on_selection()));
        }
        {
            let app = self.clone();
            add("compose", Box::new(move || app.open_compose(ComposeKind::New)));
        }
        {
            let app = self.clone();
            add("add-account", Box::new(move || app.open_account_dialog()));
        }
        {
            let app = self.clone();
            add("reply", Box::new(move || app.open_compose(ComposeKind::Reply)));
        }
        {
            let app = self.clone();
            add("reply-all", Box::new(move || app.open_compose(ComposeKind::ReplyAll)));
        }
        {
            let app = self.clone();
            add("forward", Box::new(move || app.open_compose(ComposeKind::Forward)));
        }
        {
            let app = self.clone();
            add("search", Box::new(move || {
                let bar = &app.widgets.search_bar;
                bar.set_search_mode(!bar.is_search_mode());
                if bar.is_search_mode() {
                    app.widgets.search_entry.grab_focus();
                }
            }));
        }

        // Appearance: system / light / dark.
        let theme_action = gio::SimpleAction::new_stateful(
            "theme",
            Some(glib::VariantTy::STRING),
            &self.config.borrow().theme.as_str().to_variant(),
        );
        {
            let app = self.clone();
            theme_action.connect_activate(move |action, parameter| {
                let Some(value) = parameter.and_then(|p| p.str().map(str::to_string)) else {
                    return;
                };
                let preference = ThemePreference::from_str_lossy(&value);
                apply_theme(preference);
                action.set_state(&value.to_variant());
                let mut config = app.config.borrow_mut();
                config.theme = preference;
                if let Err(e) = config.save() {
                    log::warn!("could not save the theme preference: {e:#}");
                }
            });
        }
        group.add_action(&theme_action);

        self.widgets.window.insert_action_group("win", Some(&group));

        application.set_accels_for_action("win.refresh", &["<Control>r"]);
        application.set_accels_for_action("win.search", &["<Control>f"]);
        application.set_accels_for_action("win.delete", &["Delete"]);
        application.set_accels_for_action("win.archive", &["<Control>e"]);
        application.set_accels_for_action("win.compose", &["<Control>n"]);
        application.set_accels_for_action("win.reply", &["<Control>Return"]);
        application.set_accels_for_action("win.reply-all", &["<Control><Shift>Return"]);
    }

    // -------------------------------------------------------------- events

    fn handle_event(self: &Rc<Self>, event: Event) {
        match event {
            Event::Status { account_id, state, detail } => {
                self.state.borrow_mut().status.insert(account_id, (state, detail));
                self.rebuild_sidebar();
            }

            Event::Mailboxes { account_id, mailboxes } => {
                {
                    let mut state = self.state.borrow_mut();
                    state.mailboxes.insert(account_id.clone(), mailboxes);
                }
                self.rebuild_sidebar();
                self.select_first_inbox_if_idle();
            }

            Event::Messages { account_id, mailbox, messages, append } => {
                let is_current = self
                    .state
                    .borrow()
                    .current_folder
                    .as_ref()
                    .map(|(a, m)| a == &account_id && m == &mailbox)
                    .unwrap_or(false);
                if !is_current {
                    return;
                }
                {
                    let mut state = self.state.borrow_mut();
                    if append {
                        state.messages.extend(messages);
                    } else {
                        state.messages = messages;
                        state.current_message = None;
                    }
                }
                self.widgets.spinner.set_visible(false);
                self.refresh_message_list();
                self.apply_preselection();
            }

            Event::MessageLoaded { account_id, message } => {
                // A slow fetch can land after the user has moved on.
                let still_relevant = self
                    .state
                    .borrow()
                    .current_folder
                    .as_ref()
                    .map(|(current, _)| current == &account_id)
                    .unwrap_or(false);
                if !still_relevant {
                    return;
                }
                let dark = self.state.borrow().dark;
                self.widgets.message_view.show_message(&message, dark);
                self.state.borrow_mut().current_message = Some(*message);
                self.set_actions_enabled(true);
            }

            Event::FlagChanged { account_id, mailbox, uid, flag, on } => {
                let mut sidebar_needs_refresh = false;
                {
                    let mut state = self.state.borrow_mut();
                    let previously_seen = state
                        .messages
                        .iter()
                        .find(|m| m.uid == uid && m.mailbox == mailbox)
                        .map(|m| m.seen);

                    if let Some(summary) =
                        state.messages.iter_mut().find(|m| m.uid == uid && m.mailbox == mailbox)
                    {
                        match flag.as_str() {
                            "\\Seen" => summary.seen = on,
                            "\\Flagged" => summary.flagged = on,
                            _ => {}
                        }
                    }

                    // Keep the sidebar's unread badge in step with the list.
                    if flag == "\\Seen" && previously_seen == Some(!on) {
                        if let Some(folder) = state
                            .mailboxes
                            .get_mut(&account_id)
                            .and_then(|list| list.iter_mut().find(|m| m.path == mailbox))
                        {
                            folder.unread = if on {
                                folder.unread.saturating_sub(1)
                            } else {
                                folder.unread + 1
                            };
                            sidebar_needs_refresh = true;
                        }
                    }
                }
                self.refresh_message_list_preserving_selection();
                if sidebar_needs_refresh {
                    self.rebuild_sidebar();
                }
            }

            Event::MessageRemoved { account_id, mailbox, uid } => {
                {
                    let mut state = self.state.borrow_mut();
                    let was_unread = state
                        .messages
                        .iter()
                        .find(|m| m.uid == uid && m.mailbox == mailbox)
                        .map(|m| !m.seen)
                        .unwrap_or(false);

                    state.messages.retain(|m| !(m.uid == uid && m.mailbox == mailbox));
                    state.current_message = None;

                    if was_unread {
                        if let Some(folder) = state
                            .mailboxes
                            .get_mut(&account_id)
                            .and_then(|list| list.iter_mut().find(|m| m.path == mailbox))
                        {
                            folder.unread = folder.unread.saturating_sub(1);
                        }
                    }
                }
                self.widgets.message_view.show_empty();
                self.refresh_message_list();
                self.rebuild_sidebar();
                self.toast("Messaggio spostato");
            }

            Event::Sent { account_id } => {
                log::info!("message sent from {account_id}");
                self.toast("Messaggio inviato");
                // The copy we filed in Sent changes that folder's counts.
                self.backend.borrow().send(&account_id, Command::LoadMailboxes);
            }

            Event::Error { context, detail, account_id } => {
                log::warn!("[{account_id}] {context}: {detail}");
                self.widgets.spinner.set_visible(false);

                // A message that failed to open belongs in the reading pane;
                // everything else is an account-level problem for the banner.
                if context.starts_with("apertura del messaggio") {
                    self.widgets.message_view.show_error(&context, &detail);
                } else {
                    self.widgets.banner.set_title(&format!("{context}: {detail}"));
                    self.widgets.banner.set_revealed(true);
                }
            }
        }
    }

    // ------------------------------------------------------------- sidebar

    fn rebuild_sidebar(self: &Rc<Self>) {
        let widgets = &self.widgets;
        // Remember the selection by identity: row indices shift as other
        // accounts finish loading their folder lists.
        let previous = self.state.borrow().current_folder.clone();

        while let Some(child) = widgets.sidebar_list.first_child() {
            widgets.sidebar_list.remove(&child);
        }

        let mut index_map: Vec<Option<(String, String)>> = Vec::new();
        let state = self.state.borrow();

        for account in &state.accounts {
            let status = state
                .status
                .get(&account.id)
                .map(|(_, detail)| detail.clone())
                .unwrap_or_else(|| "In attesa…".to_string());

            let header = rows::section_header(&account.short_label(), &status);
            widgets.sidebar_list.append(&header);
            index_map.push(None);

            if let Some(mailboxes) = state.mailboxes.get(&account.id) {
                for mailbox in mailboxes {
                    let row = rows::mailbox_row(mailbox);
                    widgets.sidebar_list.append(&row);
                    index_map.push(Some((mailbox.account_id.clone(), mailbox.path.clone())));
                }
            }
        }
        drop(state);

        // The map has to be in place before selecting: the selection handler
        // resolves the row index through it.
        let restore = previous.and_then(|target| {
            index_map.iter().position(|entry| entry.as_ref() == Some(&target))
        });
        self.state.borrow_mut().sidebar_index = index_map;

        if let Some(index) = restore {
            if let Some(row) = widgets.sidebar_list.row_at_index(index as i32) {
                self.state.borrow_mut().selecting_programmatically = true;
                widgets.sidebar_list.select_row(Some(&row));
                self.state.borrow_mut().selecting_programmatically = false;
            }
        }
    }

    /// Open on an inbox rather than on an empty pane.
    ///
    /// Accounts answer at their own pace, so this keeps re-targeting to the
    /// earliest account that has reported an inbox until the user makes their
    /// own choice. Otherwise whichever server replied first would win.
    fn select_first_inbox_if_idle(self: &Rc<Self>) {
        if self.state.borrow().user_picked_folder {
            return;
        }

        let target = {
            let state = self.state.borrow();
            state.accounts.iter().find_map(|account| {
                let inbox = state
                    .mailboxes
                    .get(&account.id)?
                    .iter()
                    .find(|m| m.kind == MailboxKind::Inbox)?;
                state.sidebar_index.iter().position(|entry| {
                    entry.as_ref() == Some(&(account.id.clone(), inbox.path.clone()))
                })
            })
        };

        let Some(index) = target else { return };
        let Some(row) = self.widgets.sidebar_list.row_at_index(index as i32) else { return };
        if self.widgets.sidebar_list.selected_row().map(|r| r.index()) == Some(index as i32) {
            return;
        }

        self.state.borrow_mut().selecting_programmatically = true;
        self.widgets.sidebar_list.select_row(Some(&row));
        self.state.borrow_mut().selecting_programmatically = false;
    }

    fn open_folder(self: &Rc<Self>, account_id: &str, path: &str) {
        {
            let mut state = self.state.borrow_mut();
            if state.current_folder.as_ref().map(|(a, m)| (a.as_str(), m.as_str()))
                == Some((account_id, path))
            {
                return;
            }
            state.current_folder = Some((account_id.to_string(), path.to_string()));
            state.messages.clear();
            state.visible.clear();
            state.current_message = None;
        }

        let title = self
            .state
            .borrow()
            .mailbox(account_id, path)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| path.to_string());
        self.widgets.folder_title.set_title(&title);
        self.widgets.folder_title.set_subtitle("Caricamento…");
        self.widgets.message_view.show_empty();
        self.widgets.spinner.set_visible(true);
        self.set_actions_enabled(false);
        self.refresh_message_list();

        let page_size = self.config.borrow().page_size;
        self.backend.borrow().send(
            account_id,
            Command::LoadMessages { mailbox: path.to_string(), limit: page_size, offset: 0 },
        );
    }

    fn refresh_current_folder(self: &Rc<Self>) {
        let Some((account_id, path)) = self.state.borrow().current_folder.clone() else {
            return;
        };
        self.widgets.spinner.set_visible(true);
        let page_size = self.config.borrow().page_size;
        let backend = self.backend.borrow();
        backend.send(&account_id, Command::LoadMailboxes);
        backend.send(
            &account_id,
            Command::LoadMessages { mailbox: path, limit: page_size, offset: 0 },
        );
    }

    // -------------------------------------------------------- message list

    fn refresh_message_list(self: &Rc<Self>) {
        let widgets = &self.widgets;

        while let Some(child) = widgets.message_list.first_child() {
            widgets.message_list.remove(&child);
        }

        let mut state = self.state.borrow_mut();
        let needle = state.search.trim().to_lowercase();
        let visible: Vec<usize> = state
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| matches_search(message, &needle))
            .map(|(index, _)| index)
            .collect();
        state.visible = visible.clone();

        let total = state.messages.len();
        let unread = state.messages.iter().filter(|m| !m.seen).count();
        let rows_to_add: Vec<gtk::ListBoxRow> =
            visible.iter().filter_map(|i| state.messages.get(*i)).map(rows::message_row).collect();
        drop(state);

        for row in rows_to_add {
            widgets.message_list.append(&row);
        }

        widgets.list_stack.set_visible_child_name(if visible.is_empty() {
            "empty"
        } else {
            "list"
        });

        let subtitle = if needle.is_empty() {
            match (total, unread) {
                (0, _) => "Nessun messaggio".to_string(),
                (t, 0) => format!("{t} {}", plural(t, "messaggio", "messaggi")),
                (t, u) => {
                    format!("{t} {} · {u} da leggere", plural(t, "messaggio", "messaggi"))
                }
            }
        } else {
            let found = visible.len();
            format!("{found} {}", plural(found, "risultato", "risultati"))
        };
        widgets.folder_title.set_subtitle(&subtitle);
    }

    /// Rebuild the list but keep the user's place, used after a flag change.
    fn refresh_message_list_preserving_selection(self: &Rc<Self>) {
        let selected = self.widgets.message_list.selected_row().map(|r| r.index());
        self.refresh_message_list();
        if let Some(index) = selected {
            if let Some(row) = self.widgets.message_list.row_at_index(index) {
                self.widgets.message_list.select_row(Some(&row));
            }
        }
    }

    fn open_message(self: &Rc<Self>, row_index: usize) {
        let Some((account_id, mailbox, uid, subject, seen, flagged)) = ({
            let state = self.state.borrow();
            state.selected_summary(row_index).map(|s| {
                (
                    s.account_id.clone(),
                    s.mailbox.clone(),
                    s.uid,
                    s.subject_or_placeholder().to_string(),
                    s.seen,
                    s.flagged,
                )
            })
        }) else {
            return;
        };

        self.widgets.message_view.show_loading(&subject);

        // Reflect the message's flag state without re-triggering the handler.
        let button = &self.widgets.flag_button;
        button.set_sensitive(false);
        button.set_active(flagged);
        button.set_sensitive(true);

        let backend = self.backend.borrow();
        backend.send(
            &account_id,
            Command::LoadMessage { mailbox: mailbox.clone(), uid },
        );

        // Opening a message marks it read, the way every mail client does.
        if !seen {
            backend.send(
                &account_id,
                Command::SetFlag { mailbox, uid, flag: "\\Seen".into(), on: true },
            );
        }
    }

    /// Testing hook: `MAILVIEW_SELECT_MESSAGE=<n>` opens the nth message of
    /// the folder as soon as it loads. The screenshot tooling uses it so the
    /// reading pane is populated without a real click.
    fn apply_preselection(self: &Rc<Self>) {
        let Ok(value) = std::env::var("MAILVIEW_SELECT_MESSAGE") else { return };
        let Ok(index) = value.trim().parse::<i32>() else { return };
        if self.widgets.message_list.selected_row().is_some() {
            return;
        }
        if let Some(row) = self.widgets.message_list.row_at_index(index) {
            self.widgets.message_list.select_row(Some(&row));
        }
    }

    fn rerender_body(self: &Rc<Self>) {
        let state = self.state.borrow();
        if let Some(message) = &state.current_message {
            self.widgets.message_view.render_body(message, state.dark);
        }
    }

    // ------------------------------------------------------------- actions

    fn selected_message(&self) -> Option<(String, String, u32)> {
        let row = self.widgets.message_list.selected_row()?;
        let state = self.state.borrow();
        let summary = state.selected_summary(row.index() as usize)?;
        Some((summary.account_id.clone(), summary.mailbox.clone(), summary.uid))
    }

    fn set_flag_on_selection(self: &Rc<Self>, flag: &str, on: bool) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        self.backend.borrow().send(
            &account_id,
            Command::SetFlag { mailbox, uid, flag: flag.to_string(), on },
        );
    }

    fn toggle_read_on_selection(self: &Rc<Self>) {
        let Some(row) = self.widgets.message_list.selected_row() else { return };
        let seen = {
            let state = self.state.borrow();
            state.selected_summary(row.index() as usize).map(|s| s.seen)
        };
        let Some(seen) = seen else { return };
        self.set_flag_on_selection("\\Seen", !seen);
    }

    fn move_selection_to(self: &Rc<Self>, kind: MailboxKind) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        let target = {
            let state = self.state.borrow();
            state
                .mailboxes
                .get(&account_id)
                .and_then(|list| list.iter().find(|m| m.kind == kind))
                .map(|m| m.path.clone())
        };
        let Some(target) = target else {
            self.toast(&format!("Nessuna cartella {} su questo account", label_for(kind)));
            return;
        };
        if target == mailbox {
            return;
        }
        self.backend
            .borrow()
            .send(&account_id, Command::MoveMessage { mailbox, uid, target });
    }

    fn delete_selection(self: &Rc<Self>) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        self.backend.borrow().send(&account_id, Command::DeleteMessage { mailbox, uid });
    }

    /// The account whose mailbox is currently open, falling back to the first
    /// configured one so "new message" works before anything is selected.
    fn active_account(&self) -> Option<Account> {
        let state = self.state.borrow();
        let id = state.current_folder.as_ref().map(|(account_id, _)| account_id.clone());
        match id {
            Some(id) => state.accounts.iter().find(|a| a.id == id).cloned(),
            None => state.accounts.first().cloned(),
        }
    }

    /// Add an IMAP account by hand and start using it without a restart.
    fn open_account_dialog(self: &Rc<Self>) {
        let app = self.clone();
        accounts::open(&self.widgets.window, move |manual, password| {
            // The password goes to the keyring, never to the config file.
            let label = format!("MailView — {}", manual.email);
            if let Err(e) = crate::secrets::store_password_blocking(&manual.id, &label, &password) {
                app.widgets
                    .banner
                    .set_title(&format!("Impossibile salvare la password nel portachiavi: {e:#}"));
                app.widgets.banner.set_revealed(true);
                return;
            }

            {
                let mut config = app.config.borrow_mut();
                config.accounts.retain(|existing| existing.id != manual.id);
                config.accounts.push(manual.clone());
                if let Err(e) = config.save() {
                    log::warn!("could not save the config: {e:#}");
                }
            }

            let account = manual.to_account();
            app.state.borrow_mut().accounts.push(account.clone());
            app.backend.borrow_mut().add_account(account.clone());
            app.rebuild_sidebar();
            app.backend.borrow().send(&account.id, Command::LoadMailboxes);
            app.widgets.banner.set_revealed(false);
            app.toast(&format!("Account {} aggiunto", account.email));
        });
    }

    fn open_compose(self: &Rc<Self>, kind: ComposeKind) {
        let Some(account) = self.active_account() else {
            self.toast("Nessun account configurato");
            return;
        };

        // Replies need the open message; a new message does not.
        let state = self.state.borrow();
        let message = state.current_message.as_ref();
        if kind != ComposeKind::New && message.is_none() {
            drop(state);
            self.toast("Apri prima un messaggio");
            return;
        }
        let prefilled = compose::prefill(kind, &account, message);
        drop(state);

        let app = self.clone();
        let account_id = account.id.clone();
        compose::open(&self.widgets.window, kind, &account, prefilled, move |outgoing| {
            app.toast("Invio in corso…");
            app.backend.borrow().send(&account_id, Command::Send { outgoing });
        });
    }

    fn set_actions_enabled(&self, enabled: bool) {
        for widget in &self.widgets.action_buttons {
            widget.set_sensitive(enabled);
        }
        self.widgets.flag_button.set_sensitive(enabled);
    }

    fn toast(&self, text: &str) {
        self.widgets.toast_overlay.add_toast(adw::Toast::new(text));
    }
}

fn label_for(kind: MailboxKind) -> &'static str {
    match kind {
        MailboxKind::Archive => "Archivio",
        MailboxKind::Junk => "Indesiderata",
        MailboxKind::Trash => "Cestino",
        MailboxKind::Inbox => "In arrivo",
        MailboxKind::Sent => "Inviata",
        MailboxKind::Drafts => "Bozze",
        MailboxKind::Flagged => "Speciali",
        MailboxKind::Other => "Cartella",
    }
}

/// Italian agreement for the counters in the pane subtitles.
fn plural(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 {
        singular
    } else {
        plural
    }
}

fn matches_search(message: &MessageSummary, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    message.subject.to_lowercase().contains(needle)
        || message.from.name.to_lowercase().contains(needle)
        || message.from.address.to_lowercase().contains(needle)
        || message.snippet.to_lowercase().contains(needle)
}

/// Point libadwaita at the requested colour scheme.
///
/// `Default` means "follow the desktop": libadwaita watches the
/// `org.freedesktop.appearance color-scheme` portal setting and GNOME's own
/// `org.gnome.desktop.interface color-scheme` key, so day/night switching is
/// automatic and needs nothing further from us.
pub fn apply_theme(preference: ThemePreference) {
    let manager = adw::StyleManager::default();
    manager.set_color_scheme(match preference {
        ThemePreference::System => adw::ColorScheme::Default,
        ThemePreference::Light => adw::ColorScheme::ForceLight,
        ThemePreference::Dark => adw::ColorScheme::ForceDark,
    });
}

// ----------------------------------------------------------------- widgets

fn build_widgets(application: &adw::Application) -> Widgets {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Posta")
        .default_width(1440)
        .default_height(900)
        .width_request(600)
        .height_request(450)
        .build();

    // ---- pane 1: mailboxes ------------------------------------------------
    let sidebar_list = gtk::ListBox::new();
    sidebar_list.set_selection_mode(gtk::SelectionMode::Single);
    sidebar_list.add_css_class("navigation-sidebar");
    sidebar_list.add_css_class("mail-sidebar");

    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&sidebar_list)
        .build();

    let menu = gio::Menu::new();
    let appearance = gio::Menu::new();
    appearance.append(Some("Automatico (sistema)"), Some("win.theme('system')"));
    appearance.append(Some("Chiaro"), Some("win.theme('light')"));
    appearance.append(Some("Scuro"), Some("win.theme('dark')"));
    menu.append_submenu(Some("Aspetto"), &appearance);

    let tools = gio::Menu::new();
    tools.append(Some("Aggiungi account IMAP…"), Some("win.add-account"));
    tools.append(Some("Aggiorna"), Some("win.refresh"));
    tools.append(Some("Cerca"), Some("win.search"));
    menu.append_section(None, &tools);

    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Menu principale")
        .build();

    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Caselle", "")));
    sidebar_header.pack_end(&menu_button);

    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(&sidebar_scroll));

    // ---- pane 2: message list --------------------------------------------
    let message_list = gtk::ListBox::new();
    message_list.set_selection_mode(gtk::SelectionMode::Single);
    message_list.add_css_class("message-list");

    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&message_list)
        .build();

    let empty_list = adw::StatusPage::builder()
        .icon_name("mail-mark-important-symbolic")
        .title("Nessun messaggio")
        .description("Questa cartella è vuota.")
        .build();

    let list_stack = gtk::Stack::new();
    list_stack.add_named(&list_scroll, Some("list"));
    list_stack.add_named(&empty_list, Some("empty"));
    list_stack.set_visible_child_name("empty");
    list_stack.set_vexpand(true);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Cerca nei messaggi"));
    search_entry.set_hexpand(true);

    let search_bar = gtk::SearchBar::builder().child(&search_entry).build();
    search_bar.set_key_capture_widget(Some(&window));
    search_bar.connect_entry(&search_entry);

    let folder_title = adw::WindowTitle::new("Posta", "");

    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_visible(false);

    let search_button = gtk::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text("Cerca (Ctrl+F)")
        .build();
    search_button
        .bind_property("active", &search_bar, "search-mode-enabled")
        .bidirectional()
        .sync_create()
        .build();

    let refresh_button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Aggiorna (Ctrl+R)")
        .action_name("win.refresh")
        .build();

    let compose_button = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text("Nuovo messaggio (Ctrl+N)")
        .action_name("win.compose")
        .build();

    let list_header = adw::HeaderBar::new();
    list_header.set_title_widget(Some(&folder_title));
    list_header.pack_start(&refresh_button);
    list_header.pack_start(&compose_button);
    list_header.pack_end(&search_button);
    list_header.pack_end(&spinner);

    let list_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_body.append(&search_bar);
    list_body.append(&list_stack);

    let list_view = adw::ToolbarView::new();
    list_view.add_top_bar(&list_header);
    list_view.set_content(Some(&list_body));

    // ---- pane 3: the message ---------------------------------------------
    let message_view = MessageView::new();

    let reply_button = gtk::Button::builder()
        .icon_name("mail-reply-sender-symbolic")
        .tooltip_text("Rispondi (Ctrl+Invio)")
        .action_name("win.reply")
        .build();
    let reply_all_button = gtk::Button::builder()
        .icon_name("mail-reply-all-symbolic")
        .tooltip_text("Rispondi a tutti (Ctrl+Maiusc+Invio)")
        .action_name("win.reply-all")
        .build();
    let forward_button = gtk::Button::builder()
        .icon_name("mail-forward-symbolic")
        .tooltip_text("Inoltra")
        .action_name("win.forward")
        .build();

    let archive_button = gtk::Button::builder()
        .icon_name("mail-archive-symbolic")
        .tooltip_text("Archivia (Ctrl+E)")
        .action_name("win.archive")
        .build();
    let junk_button = gtk::Button::builder()
        .icon_name("dialog-warning-symbolic")
        .tooltip_text("Segna come indesiderata")
        .action_name("win.junk")
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Elimina (Canc)")
        .action_name("win.delete")
        .build();
    let unread_button = gtk::Button::builder()
        .icon_name("mail-unread-symbolic")
        .tooltip_text("Segna come da leggere / letto")
        .action_name("win.toggle-read")
        .build();
    let flag_button = gtk::ToggleButton::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Contrassegna")
        .build();

    // Apple Mail groups its message actions; linked buttons give the same read.
    let filing_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    filing_group.add_css_class("linked");
    filing_group.append(&archive_button);
    filing_group.append(&junk_button);
    filing_group.append(&delete_button);

    let reply_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    reply_group.add_css_class("linked");
    reply_group.append(&reply_button);
    reply_group.append(&reply_all_button);
    reply_group.append(&forward_button);

    let reader_header = adw::HeaderBar::new();
    reader_header.set_title_widget(Some(&adw::WindowTitle::new("Messaggio", "")));
    reader_header.pack_start(&filing_group);
    reader_header.pack_start(&unread_button);
    reader_header.pack_start(&flag_button);
    reader_header.pack_end(&reply_group);

    let reader_view = adw::ToolbarView::new();
    reader_view.add_top_bar(&reader_header);
    reader_view.set_content(Some(&message_view.root));

    // ---- assemble the three panes ----------------------------------------
    let inner_split = adw::NavigationSplitView::new();
    inner_split.set_sidebar(Some(&adw::NavigationPage::new(&list_view, "Messaggi")));
    inner_split.set_content(Some(&adw::NavigationPage::new(&reader_view, "Messaggio")));
    inner_split.set_min_sidebar_width(330.0);
    inner_split.set_max_sidebar_width(460.0);
    inner_split.set_sidebar_width_fraction(0.34);

    let outer_split = adw::NavigationSplitView::new();
    outer_split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar_view, "Caselle")));
    outer_split.set_content(Some(&adw::NavigationPage::new(&inner_split, "Posta")));
    outer_split.set_min_sidebar_width(220.0);
    outer_split.set_max_sidebar_width(300.0);
    outer_split.set_sidebar_width_fraction(0.17);

    let banner = adw::Banner::new("");
    banner.set_revealed(false);
    banner.set_button_label(Some("Chiudi"));
    {
        let banner_clone = banner.clone();
        banner.connect_button_clicked(move |_| banner_clone.set_revealed(false));
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&banner);
    content.append(&outer_split);
    outer_split.set_vexpand(true);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&content));

    window.set_content(Some(&toast_overlay));

    let mut action_buttons: Vec<gtk::Widget> = Vec::new();
    action_buttons.push(reply_button.upcast());
    action_buttons.push(reply_all_button.upcast());
    action_buttons.push(forward_button.upcast());
    action_buttons.push(archive_button.upcast());
    action_buttons.push(junk_button.upcast());
    action_buttons.push(delete_button.upcast());
    action_buttons.push(unread_button.upcast());
    for widget in &action_buttons {
        widget.set_sensitive(false);
    }
    flag_button.set_sensitive(false);

    Widgets {
        window,
        sidebar_list,
        message_list,
        message_view,
        folder_title,
        search_bar,
        search_entry,
        list_stack,
        banner,
        toast_overlay,
        flag_button,
        action_buttons,
        spinner,
    }
}

/// Load the bundled stylesheet.
pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

pub fn app_id() -> &'static str {
    APP_ID
}

/// Unused import guard: `Message` is part of the public event payloads.
#[allow(dead_code)]
fn _type_check(_: Option<Message>) {}
