//! The application window.

pub mod accounts;
pub mod compose;
pub mod message_view;
pub mod preferences;
pub mod rows;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::backend::smtp::Outgoing;
use crate::backend::{Backend, Command, ConnectionState, Event};
use compose::ComposeKind;
use crate::config::{Config, ThemePreference};
use crate::i18n::{plural, t, t1};
use crate::model::{Account, Mailaddr, Mailbox, MailboxKind, Message, MessageSummary};
use message_view::{MessageView, RenderPrefs};

const APP_ID: &str = "it.antoniopicone.MailView";

/// A mailbox that spans every configured account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartMailbox {
    /// Every account's inbox, merged and sorted by date.
    AllInboxes,
    /// Flagged messages across those inboxes.
    Flagged,
    /// Unread messages across those inboxes.
    Unread,
}

impl SmartMailbox {
    fn title(&self) -> &'static str {
        match self {
            SmartMailbox::AllInboxes => t("In entrata (tutte)"),
            SmartMailbox::Flagged => t("Contrassegnati"),
            SmartMailbox::Unread => t("Non letti"),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            SmartMailbox::AllInboxes => "mailview-inbox-all-symbolic",
            SmartMailbox::Flagged => "mailview-flagged-symbolic",
            SmartMailbox::Unread => "mailview-unread-symbolic",
        }
    }

    /// Whether a message belongs in this mailbox.
    fn accepts(&self, message: &MessageSummary) -> bool {
        match self {
            SmartMailbox::AllInboxes => true,
            SmartMailbox::Flagged => message.flagged,
            SmartMailbox::Unread => !message.seen,
        }
    }
}

/// What the sidebar selection currently points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderTarget {
    Smart(SmartMailbox),
    Mailbox { account_id: String, path: String },
}

impl FolderTarget {
    fn spans_accounts(&self) -> bool {
        matches!(self, FolderTarget::Smart(_))
    }
}

/// One selectable sidebar row. The same mailbox can appear twice — once as a
/// shortcut in the unified section and once inside its account's own section —
/// so `shortcut` distinguishes the two for selection tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarEntry {
    target: FolderTarget,
    shortcut: bool,
}

/// Where a given account stands in a lazily-paged mailbox: how many messages
/// have been requested so far, whether the server might still have older
/// ones, and whether a page request is already in flight.
#[derive(Debug, Clone, Copy)]
struct PageState {
    next_offset: u32,
    has_more: bool,
    loading: bool,
}

impl Default for PageState {
    fn default() -> Self {
        Self { next_offset: 0, has_more: true, loading: false }
    }
}

/// Everything the UI needs to know that is not held by a widget.
#[derive(Default)]
struct State {
    accounts: Vec<Account>,
    mailboxes: HashMap<String, Vec<Mailbox>>,
    status: HashMap<String, (ConnectionState, String)>,
    /// Parallel to the sidebar's rows: what each row selects.
    sidebar_index: Vec<Option<SidebarEntry>>,
    current_target: Option<FolderTarget>,
    /// The row that is selected, so a rebuild can restore it even though row
    /// indices shift as other accounts finish loading.
    selected_entry: Option<SidebarEntry>,
    /// Messages per account. A single mailbox fills one bucket; a unified view
    /// fills one per account and merges them for display.
    buckets: HashMap<String, Vec<MessageSummary>>,
    /// The merged, date-sorted view of `buckets`.
    messages: Vec<MessageSummary>,
    /// Indices into `messages` that survive the current search and target.
    visible: Vec<usize>,
    /// Lazy-paging progress per account, for the currently open target.
    pagination: HashMap<String, PageState>,
    /// account_id -> human description of the sync currently in flight for
    /// it, shown in the bottom status bar. Removed once that operation's
    /// event (or error) arrives.
    activity: HashMap<String, String>,
    current_message: Option<Message>,
    search: String,
    /// Filter toggle: when set, the list shows only unread messages.
    unread_only: bool,
    dark: bool,
    /// Set once the user picks a mailbox themselves, so later arrivals from
    /// slower accounts stop moving them around.
    user_picked_folder: bool,
    /// True while we are driving a sidebar selection ourselves.
    selecting_programmatically: bool,
    /// True while we are re-selecting the open message after a list rebuild.
    restoring_message: bool,
    /// The screenshot hook must only fire once, not on every batch.
    preselection_done: bool,
    /// When every in-flight sync last finished, for the status bar's
    /// "10 min fa" style display.
    last_sync: Option<chrono::DateTime<chrono::Local>>,
    /// Handles into the currently-built message rows, keyed by identity, so
    /// a flag change can restyle the row already on screen instead of
    /// rebuilding the whole list. Repopulated on every `refresh_message_list`.
    row_refs: HashMap<(String, String, u32), rows::MessageRowRefs>,
}

impl State {
    fn selected_summary(&self, row_index: usize) -> Option<&MessageSummary> {
        self.visible.get(row_index).and_then(|i| self.messages.get(*i))
    }

    fn mailbox(&self, account_id: &str, path: &str) -> Option<&Mailbox> {
        self.mailboxes.get(account_id)?.iter().find(|m| m.path == path)
    }

    /// The IMAP path of an account's inbox, once its folder list has arrived.
    fn inbox_path(&self, account_id: &str) -> Option<&str> {
        self.mailboxes
            .get(account_id)?
            .iter()
            .find(|m| m.kind == MailboxKind::Inbox)
            .map(|m| m.path.as_str())
    }

    fn inbox_unread(&self, account_id: &str) -> u32 {
        self.mailboxes
            .get(account_id)
            .and_then(|list| list.iter().find(|m| m.kind == MailboxKind::Inbox))
            .map(|m| m.unread)
            .unwrap_or(0)
    }

    fn total_inbox_unread(&self) -> u32 {
        self.accounts.iter().map(|a| self.inbox_unread(&a.id)).sum()
    }

    fn account_label(&self, account_id: &str) -> Option<String> {
        self.accounts.iter().find(|a| a.id == account_id).map(|a| a.short_label())
    }

    /// Does an incoming batch of messages belong to what is on screen?
    fn batch_is_relevant(&self, account_id: &str, mailbox: &str) -> bool {
        match &self.current_target {
            Some(FolderTarget::Mailbox { account_id: a, path }) => {
                a == account_id && path == mailbox
            }
            Some(FolderTarget::Smart(_)) => self.inbox_path(account_id) == Some(mailbox),
            None => false,
        }
    }

    /// Rebuild the merged message list from the per-account buckets.
    fn remerge(&mut self) {
        let mut merged: Vec<MessageSummary> =
            self.buckets.values().flat_map(|list| list.iter().cloned()).collect();
        merged.sort_by(|a, b| b.date.cmp(&a.date));
        self.messages = merged;
    }
}

/// The natural (default) order of the sidebar's top shortcut rows: the
/// unified inbox, one per account, then Flagged and Unread — only shown at
/// all once there is more than one account to unify.
fn natural_shortcut_ids(state: &State) -> Vec<String> {
    if state.accounts.len() <= 1 {
        return Vec::new();
    }
    let mut ids = vec!["unified".to_string()];
    for account in &state.accounts {
        ids.push(format!("account:{}", account.id));
    }
    ids.push("flagged".to_string());
    ids.push("unread".to_string());
    ids
}

/// The natural order, with the user's saved drag-and-drop order applied:
/// customised items keep their saved position, anything new (a just-added
/// account, say) is appended in its natural place.
fn ordered_shortcut_ids(state: &State, config: &Config) -> Vec<String> {
    let natural = natural_shortcut_ids(state);
    if config.sidebar_order.is_empty() {
        return natural;
    }
    let natural_set: std::collections::HashSet<&str> =
        natural.iter().map(String::as_str).collect();
    let mut ordered: Vec<String> = config
        .sidebar_order
        .iter()
        .filter(|id| natural_set.contains(id.as_str()))
        .cloned()
        .collect();
    for id in &natural {
        if !ordered.contains(id) {
            ordered.push(id.clone());
        }
    }
    ordered
}

/// Build one shortcut row from its stable id, or `None` when the id no
/// longer resolves to anything (an account that was removed, or whose
/// inbox has not been discovered yet).
fn build_shortcut_row(id: &str, state: &State) -> Option<(gtk::ListBoxRow, SidebarEntry)> {
    match id {
        "unified" => Some((
            rows::smart_row(
                SmartMailbox::AllInboxes.title(),
                SmartMailbox::AllInboxes.icon(),
                state.total_inbox_unread(),
            ),
            SidebarEntry { target: FolderTarget::Smart(SmartMailbox::AllInboxes), shortcut: true },
        )),
        "flagged" => Some((
            rows::smart_row(SmartMailbox::Flagged.title(), SmartMailbox::Flagged.icon(), 0),
            SidebarEntry { target: FolderTarget::Smart(SmartMailbox::Flagged), shortcut: true },
        )),
        "unread" => Some((
            rows::smart_row(
                SmartMailbox::Unread.title(),
                SmartMailbox::Unread.icon(),
                state.total_inbox_unread(),
            ),
            SidebarEntry { target: FolderTarget::Smart(SmartMailbox::Unread), shortcut: true },
        )),
        _ => {
            let account_id = id.strip_prefix("account:")?;
            let account = state.accounts.iter().find(|a| a.id == account_id)?;
            let path = state.inbox_path(&account.id)?.to_string();
            Some((
                rows::smart_row(
                    &account.short_label(),
                    MailboxKind::Inbox.icon(),
                    state.inbox_unread(&account.id),
                ),
                SidebarEntry {
                    target: FolderTarget::Mailbox { account_id: account.id.clone(), path },
                    shortcut: true,
                },
            ))
        }
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
    list_scroll: gtk::ScrolledWindow,
    banner: adw::Banner,
    toast_overlay: adw::ToastOverlay,
    flag_button: gtk::ToggleButton,
    unread_filter_button: gtk::ToggleButton,
    action_buttons: Vec<gtk::Widget>,
    /// Bottom status bar: sync spinner and a description of what is
    /// currently being synced.
    status_spinner: gtk::Spinner,
    status_label: gtk::Label,
    /// Mailboxes ↔ (messages | reader). Their widths are user-resizable and
    /// persisted to config on close.
    outer_paned: gtk::Paned,
    inner_paned: gtk::Paned,
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
            // Enforce the offline retention window against whatever is
            // already on disk from a previous run.
            let cutoff = config.borrow().offline_window(&account.id).cutoff();
            crate::cache::prune_messages_older_than(&account.id, cutoff);
        }
        let events = backend.events();
        let backend = Rc::new(RefCell::new(backend));

        let widgets = Rc::new(build_widgets(application, &config.borrow()));
        let app = Rc::new(Self {
            widgets: widgets.clone(),
            state: state.clone(),
            backend: backend.clone(),
            config: config.clone(),
        });

        app.connect_signals();
        app.install_actions(application);

        // Restore each account's folder list from the last session before
        // anything has connected. With it in hand, the sidebar can populate
        // and land on a target immediately — including sending the real
        // LoadMessages request below — instead of waiting for the network.
        app.preload_mailboxes_from_cache();
        app.rebuild_sidebar();
        app.select_first_inbox_if_idle();

        // Fallback for accounts with no cached folder list yet (first ever
        // launch): still show their last known inbox, guessing "INBOX" since
        // the real path isn't known before the folder list arrives.
        app.preload_inboxes_from_cache();

        // Ask every account for its folder list; the first inbox that arrives
        // is selected automatically.
        backend.borrow().broadcast(Command::LoadMailboxes);
        for account in &accounts {
            app.set_activity(&account.id, t("Aggiornamento delle cartelle…").to_string());
        }

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

        // Keep the "last sync: N min fa" status text current even when
        // nothing else happens to trigger a refresh.
        {
            let app = app.clone();
            glib::timeout_add_seconds_local(30, move || {
                app.refresh_status_bar();
                glib::ControlFlow::Continue
            });
        }

        if accounts.is_empty() {
            widgets.banner.set_title(t(
                "Nessun account configurato. Aggiungine uno in Impostazioni → Account online, \
                 oppure avvia con --demo per esplorare l'interfaccia.",
            ));
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
                let (entry, programmatic) = {
                    let state = app.state.borrow();
                    (
                        state.sidebar_index.get(index).cloned().flatten(),
                        state.selecting_programmatically,
                    )
                };
                if !programmatic {
                    app.state.borrow_mut().user_picked_folder = true;
                }
                if let Some(entry) = entry {
                    app.state.borrow_mut().selected_entry = Some(entry.clone());
                    app.open_target(entry.target);
                }
            });
        }

        // Message list: open a message.
        {
            let app = self.clone();
            self.widgets.message_list.connect_row_selected(move |_, row| {
                // Clearing the list during a rebuild fires this with `None`;
                // ignore both halves of a restore so the reading pane holds.
                if app.state.borrow().restoring_message {
                    return;
                }
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

        // Unread-only filter.
        {
            let app = self.clone();
            self.widgets.unread_filter_button.connect_toggled(move |button| {
                app.state.borrow_mut().unread_only = button.is_active();
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

        // Lazy-load: pull in the next page once the user scrolls to the
        // bottom of the currently loaded messages.
        {
            let app = self.clone();
            self.widgets.list_scroll.connect_edge_reached(move |_, pos| {
                if pos == gtk::PositionType::Bottom {
                    app.load_more();
                }
            });
        }

        let window = self.widgets.window.clone();
        let backend = self.backend.clone();
        let config = self.config.clone();
        let outer_paned = self.widgets.outer_paned.clone();
        let inner_paned = self.widgets.inner_paned.clone();
        window.connect_close_request(move |_| {
            backend.borrow().shutdown();
            {
                let mut config = config.borrow_mut();
                config.sidebar_width = outer_paned.position();
                config.message_list_width = inner_paned.position();
                if let Err(e) = config.save() {
                    log::warn!("could not save the window layout: {e:#}");
                }
            }
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
            add("preferences", Box::new(move || app.open_preferences()));
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
                crate::cache::store_mailboxes(&account_id, &mailboxes);
                {
                    let mut state = self.state.borrow_mut();
                    state.mailboxes.insert(account_id.clone(), mailboxes);
                }
                self.clear_activity(&account_id);
                self.rebuild_sidebar();
                self.select_first_inbox_if_idle();

                // A unified view is opened as soon as the first account
                // answers, so accounts that report their folders later still
                // need their inbox pulled in.
                let pending = {
                    let state = self.state.borrow();
                    let spans = state
                        .current_target
                        .as_ref()
                        .map(FolderTarget::spans_accounts)
                        .unwrap_or(false);
                    if spans && !state.buckets.contains_key(&account_id) {
                        state.inbox_path(&account_id).map(str::to_string)
                    } else {
                        None
                    }
                };

                if let Some(path) = pending {
                    let page_size = self.config.borrow().page_size;
                    self.state.borrow_mut().pagination.insert(
                        account_id.clone(),
                        PageState { next_offset: 0, has_more: true, loading: true },
                    );
                    self.backend.borrow().send(
                        &account_id,
                        Command::LoadMessages { mailbox: path.clone(), limit: page_size, offset: 0 },
                    );
                    self.set_activity(
                        &account_id,
                        t1("Sincronizzazione — {}", &self.describe_folder(&account_id, &path)),
                    );
                }
            }

            Event::Messages { account_id, mailbox, messages, append } => {
                let page_size = self.config.borrow().page_size;
                let count = messages.len() as u32;
                {
                    let mut state = self.state.borrow_mut();
                    if !state.batch_is_relevant(&account_id, &mailbox) {
                        return;
                    }
                    {
                        let bucket = state.buckets.entry(account_id.clone()).or_default();
                        if append {
                            bucket.extend(messages);
                        } else {
                            *bucket = messages;
                        }
                    }

                    // A full (non-paged) reload can legitimately drop the
                    // message that is open, e.g. it was deleted elsewhere.
                    // But it must only close the reading pane for the
                    // account/mailbox that was actually reloaded — a
                    // background refresh of a different account in the
                    // unified view must not blank out what the user is
                    // reading.
                    if !append {
                        let mut clear_current = false;
                        if let Some(current) = &state.current_message {
                            if current.summary.account_id == account_id
                                && current.summary.mailbox == mailbox
                            {
                                let uid = current.summary.uid;
                                let present = state
                                    .buckets
                                    .get(&account_id)
                                    .map(|b| b.iter().any(|m| m.uid == uid))
                                    .unwrap_or(false);
                                if !present {
                                    clear_current = true;
                                }
                            }
                        }
                        if clear_current {
                            state.current_message = None;
                        }
                    }

                    state.remerge();

                    let page = state.pagination.entry(account_id.clone()).or_default();
                    page.loading = false;
                    page.has_more = page_size > 0 && count >= page_size;
                    page.next_offset = if append { page.next_offset + count } else { count };
                }
                if !append {
                    if let Some(bucket) = self.state.borrow().buckets.get(&account_id) {
                        crate::cache::store_summaries(&account_id, &mailbox, bucket);
                    }
                }
                self.clear_activity(&account_id);
                self.refresh_message_list();
                self.apply_preselection();
            }

            Event::MessageLoaded { account_id, message } => {
                // A slow fetch can land after the user has moved on. In a
                // unified view every account is in scope, so only a
                // single-mailbox target can rule the message out.
                let still_relevant = match &self.state.borrow().current_target {
                    Some(FolderTarget::Mailbox { account_id: current, .. }) => {
                        current == &account_id
                    }
                    Some(FolderTarget::Smart(_)) => true,
                    None => false,
                };
                if !still_relevant {
                    return;
                }
                let prefs = self.render_prefs();
                self.widgets.message_view.show_message(&message, prefs);
                crate::cache::store_message(
                    &account_id,
                    &message.summary.mailbox,
                    message.summary.uid,
                    &message,
                );
                self.state.borrow_mut().current_message = Some(*message);
                self.set_actions_enabled(true);
            }

            Event::FlagChanged { account_id, mailbox, uid, flag, on } => {
                let mut sidebar_needs_refresh = false;
                let needs_full_refresh;
                {
                    let mut state = self.state.borrow_mut();
                    let bucket = state.buckets.entry(account_id.clone()).or_default();
                    let previously_seen = bucket
                        .iter()
                        .find(|m| m.uid == uid && m.mailbox == mailbox)
                        .map(|m| m.seen);

                    if let Some(summary) =
                        bucket.iter_mut().find(|m| m.uid == uid && m.mailbox == mailbox)
                    {
                        match flag.as_str() {
                            "\\Seen" => summary.seen = on,
                            "\\Flagged" => summary.flagged = on,
                            _ => {}
                        }
                    }
                    state.remerge();

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

                    // A filtered view can gain or lose this row outright,
                    // which genuinely needs a rebuild; otherwise the row
                    // already on screen can just be restyled in place,
                    // without touching the list itself — its selection, any
                    // popover a user has open on another row, scroll
                    // position, all stay exactly as they were.
                    needs_full_refresh = match flag.as_str() {
                        "\\Seen" => state.unread_only,
                        "\\Flagged" => {
                            matches!(state.current_target, Some(FolderTarget::Smart(SmartMailbox::Flagged)))
                        }
                        _ => true,
                    };
                }

                if needs_full_refresh {
                    self.refresh_message_list();
                } else {
                    let key = (account_id.clone(), mailbox.clone(), uid);
                    let state = self.state.borrow();
                    if let Some(refs) = state.row_refs.get(&key) {
                        match flag.as_str() {
                            "\\Seen" => rows::set_row_seen(refs, on),
                            "\\Flagged" => rows::set_row_flagged(refs, on),
                            _ => {}
                        }
                    }
                    drop(state);
                    self.update_message_list_subtitle();
                }
                if sidebar_needs_refresh {
                    self.rebuild_sidebar();
                }
            }

            Event::MessageRemoved { account_id, mailbox, uid } => {
                {
                    let mut state = self.state.borrow_mut();
                    let bucket = state.buckets.entry(account_id.clone()).or_default();
                    let was_unread = bucket
                        .iter()
                        .find(|m| m.uid == uid && m.mailbox == mailbox)
                        .map(|m| !m.seen)
                        .unwrap_or(false);

                    bucket.retain(|m| !(m.uid == uid && m.mailbox == mailbox));
                    state.remerge();
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
                self.toast(t("Messaggio spostato"));
            }

            Event::Sent { account_id } => {
                log::info!("message sent from {account_id}");
                self.toast(t("Messaggio inviato"));
                // The copy we filed in Sent changes that folder's counts.
                self.backend.borrow().send(&account_id, Command::LoadMailboxes);
                self.set_activity(&account_id, t("Aggiornamento delle cartelle…").to_string());
            }

            Event::DraftSaved { account_id } => {
                log::info!("draft saved for {account_id}");
                self.toast(t("Bozza salvata"));
                self.backend.borrow().send(&account_id, Command::LoadMailboxes);
                self.set_activity(&account_id, t("Aggiornamento delle cartelle…").to_string());
            }

            Event::Error { context, detail, account_id } => {
                log::warn!("[{account_id}] {context}: {detail}");
                self.clear_activity(&account_id);

                // A message that failed to open belongs in the reading pane;
                // everything else is an account-level problem, worth a
                // dialog the user has to actually acknowledge.
                if context.starts_with("apertura del messaggio") {
                    self.widgets.message_view.show_error(&context, &detail);
                } else {
                    self.show_error_dialog(&context, &detail);
                }
            }
        }
    }

    // ------------------------------------------------------------- sidebar

    /// Rebuild the sidebar: a unified section on top, then one section per
    /// account, the way Mail on iOS arranges it.
    fn rebuild_sidebar(self: &Rc<Self>) {
        let widgets = &self.widgets;
        // Remember the selection by identity: row indices shift as other
        // accounts finish loading their folder lists.
        let previous = self.state.borrow().selected_entry.clone();

        while let Some(child) = widgets.sidebar_list.first_child() {
            widgets.sidebar_list.remove(&child);
        }

        let mut index_map: Vec<Option<SidebarEntry>> = Vec::new();
        let state = self.state.borrow();

        let mut push = |row: gtk::ListBoxRow, entry: Option<SidebarEntry>| {
            widgets.sidebar_list.append(&row);
            index_map.push(entry);
        };

        // ---- unified section ------------------------------------------
        // No section header here: the pane is already titled "Caselle", and
        // iOS leaves this first group unlabelled too. Only worth showing
        // once there is more than one account to unify — `ordered_shortcut_ids`
        // already returns nothing otherwise.
        let ids = ordered_shortcut_ids(&state, &self.config.borrow());
        for id in &ids {
            if let Some((row, entry)) = build_shortcut_row(id, &state) {
                self.attach_shortcut_dnd(&row, id);
                push(row, Some(entry));
            }
        }

        // ---- one section per account ----------------------------------
        for account in &state.accounts {
            let has_error = state
                .status
                .get(&account.id)
                .map(|(state, _)| *state == ConnectionState::Offline)
                .unwrap_or(false);

            let collapsed = self.config.borrow().collapsed_accounts.contains(&account.id);
            let app = self.clone();
            let account_id = account.id.clone();
            push(
                rows::section_header(&account.short_label(), has_error, collapsed, move || {
                    app.toggle_account_collapsed(&account_id);
                }),
                None,
            );

            if !collapsed {
                if let Some(mailboxes) = state.mailboxes.get(&account.id) {
                    for mailbox in mailboxes {
                        push(
                            rows::mailbox_row(mailbox),
                            Some(SidebarEntry {
                                target: FolderTarget::Mailbox {
                                    account_id: mailbox.account_id.clone(),
                                    path: mailbox.path.clone(),
                                },
                                shortcut: false,
                            }),
                        );
                    }
                }
            }
        }
        drop(state);

        // The map has to be in place before selecting: the selection handler
        // resolves the row index through it.
        let restore = previous.and_then(|entry| {
            index_map
                .iter()
                .position(|candidate| candidate.as_ref() == Some(&entry))
                // The row may have moved between sections; fall back to any
                // row pointing at the same folder.
                .or_else(|| {
                    index_map.iter().position(|candidate| {
                        candidate.as_ref().map(|c| &c.target) == Some(&entry.target)
                    })
                })
        });
        self.state.borrow_mut().sidebar_index = index_map;

        if let Some(index) = restore {
            if let Some(row) = widgets.sidebar_list.row_at_index(index as i32) {
                self.state.borrow_mut().selecting_programmatically = true;
                widgets.sidebar_list.select_row(Some(&row));
                self.state.borrow_mut().selecting_programmatically = false;
            }
        }

        crate::badge::set_unread_count(self.state.borrow().total_inbox_unread());
    }

    /// Fold or unfold one account's folder list.
    fn toggle_account_collapsed(self: &Rc<Self>, account_id: &str) {
        let mut config = self.config.borrow_mut();
        if !config.collapsed_accounts.remove(account_id) {
            config.collapsed_accounts.insert(account_id.to_string());
        }
        if let Err(e) = config.save() {
            log::warn!("could not save the sidebar collapse state: {e:#}");
        }
        drop(config);
        self.rebuild_sidebar();
    }

    /// Wire a shortcut row so it can be dragged, and can accept another
    /// shortcut dropped onto it to reorder the two. While a drag hovers, a
    /// line lights up at the row's near or far edge — the gap it would land
    /// in — rather than silently accepting a drop with no feedback.
    fn attach_shortcut_dnd(self: &Rc<Self>, row: &gtk::ListBoxRow, id: &str) {
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gtk::gdk::DragAction::MOVE);
        let drag_id = id.to_string();
        drag_source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(&drag_id.to_value()))
        });
        row.add_controller(drag_source);

        let drop_target =
            gtk::DropTarget::new(glib::types::Type::STRING, gtk::gdk::DragAction::MOVE);
        drop_target.set_actions(gtk::gdk::DragAction::MOVE);

        {
            let app = self.clone();
            let row_weak = row.downgrade();
            drop_target.connect_motion(move |_, _x, y| {
                if let Some(row) = row_weak.upgrade() {
                    let after = y > row.height() as f64 / 2.0;
                    app.set_drop_indicator(&row, after);
                }
                gtk::gdk::DragAction::MOVE
            });
        }
        {
            let app = self.clone();
            drop_target.connect_leave(move |_| app.clear_drop_indicators());
        }
        {
            let app = self.clone();
            let target_id = id.to_string();
            let row_weak = row.downgrade();
            drop_target.connect_drop(move |_, value, _x, y| {
                app.clear_drop_indicators();
                let Ok(dragged_id) = value.get::<String>() else { return false };
                let Some(target_row) = row_weak.upgrade() else { return false };
                let after = y > target_row.height() as f64 / 2.0;
                app.move_shortcut(&dragged_id, &target_id, after);
                true
            });
        }
        row.add_controller(drop_target);
    }

    /// Light up the reorder line at one row's near or far edge, clearing it
    /// from every other row first — only one drop zone is active at a time.
    fn set_drop_indicator(self: &Rc<Self>, row: &gtk::ListBoxRow, after: bool) {
        self.clear_drop_indicators();
        row.add_css_class(if after { "drop-indicator-after" } else { "drop-indicator-before" });
    }

    fn clear_drop_indicators(self: &Rc<Self>) {
        let mut child = self.widgets.sidebar_list.first_child();
        while let Some(widget) = child {
            widget.remove_css_class("drop-indicator-before");
            widget.remove_css_class("drop-indicator-after");
            child = widget.next_sibling();
        }
    }

    /// Move a shortcut row to just before or after another, and persist the
    /// resulting order.
    fn move_shortcut(self: &Rc<Self>, dragged_id: &str, target_id: &str, after: bool) {
        if dragged_id == target_id {
            return;
        }
        let mut order = {
            let state = self.state.borrow();
            let config = self.config.borrow();
            ordered_shortcut_ids(&state, &config)
        };
        let Some(from) = order.iter().position(|id| id == dragged_id) else { return };
        let item = order.remove(from);
        let Some(mut to) = order.iter().position(|id| id == target_id) else { return };
        if after {
            to += 1;
        }
        order.insert(to.min(order.len()), item);

        let mut config = self.config.borrow_mut();
        config.sidebar_order = order;
        if let Err(e) = config.save() {
            log::warn!("could not save the sidebar order: {e:#}");
        }
        drop(config);
        self.rebuild_sidebar();
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
            // With several accounts the unified inbox is the natural landing
            // place; with one, its own inbox is.
            let wanted = if state.accounts.len() > 1 {
                Some(FolderTarget::Smart(SmartMailbox::AllInboxes))
            } else {
                state.accounts.iter().find_map(|account| {
                    let path = state.inbox_path(&account.id)?;
                    Some(FolderTarget::Mailbox {
                        account_id: account.id.clone(),
                        path: path.to_string(),
                    })
                })
            };
            wanted.and_then(|wanted| {
                state
                    .sidebar_index
                    .iter()
                    .position(|entry| entry.as_ref().map(|e| &e.target) == Some(&wanted))
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

    fn open_target(self: &Rc<Self>, target: FolderTarget) {
        {
            let mut state = self.state.borrow_mut();
            if state.current_target.as_ref() == Some(&target) {
                return;
            }
            state.current_target = Some(target.clone());
            state.buckets.clear();
            state.messages.clear();
            state.visible.clear();
            state.pagination.clear();
            state.current_message = None;
        }

        let title = match &target {
            FolderTarget::Smart(smart) => smart.title().to_string(),
            FolderTarget::Mailbox { account_id, path } => self
                .state
                .borrow()
                .mailbox(account_id, path)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| path.clone()),
        };

        self.widgets.folder_title.set_title(&title);
        self.widgets.folder_title.set_subtitle(t("Caricamento…"));
        self.widgets.message_view.show_empty();
        self.set_actions_enabled(false);
        self.preload_from_cache(&target);
        self.refresh_message_list();
        self.request_messages(&target);
    }

    /// Restore every account's last known folder list before a single
    /// network command has gone out, so the sidebar can populate and
    /// [`select_first_inbox_if_idle`](Self::select_first_inbox_if_idle) has
    /// something to land on immediately instead of waiting on the network.
    fn preload_mailboxes_from_cache(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        for account in state.accounts.clone() {
            if let Some(cached) = crate::cache::load_mailboxes(&account.id) {
                state.mailboxes.insert(account.id, cached);
            }
        }
    }

    /// Show every account's last known inbox at startup, before even the
    /// folder list has come back. "INBOX" is the one mailbox name the IMAP
    /// protocol guarantees exists under that literal spelling on every
    /// server, so it is the only folder worth guessing at before any account
    /// has told us its real folder paths. A slow or stalled connection would
    /// otherwise leave the window showing "no messages" for as long as it
    /// takes to time out.
    ///
    /// Only fills in accounts a real target selection hasn't already
    /// populated with accurate data, so this can never clobber it with a
    /// guess.
    fn preload_inboxes_from_cache(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        for account in state.accounts.clone() {
            if state.buckets.contains_key(&account.id) {
                continue;
            }
            if let Some(cached) = crate::cache::load_summaries(&account.id, "INBOX") {
                state.buckets.insert(account.id, cached);
            }
        }
        state.remerge();
        drop(state);
        self.refresh_message_list();
    }

    /// Show whatever was cached from the last time this target was open,
    /// before the network has answered. It is fully replaced by the first
    /// real batch that arrives, so a stale cache heals itself immediately.
    fn preload_from_cache(self: &Rc<Self>, target: &FolderTarget) {
        let entries: Vec<(String, String)> = match target {
            FolderTarget::Mailbox { account_id, path } => vec![(account_id.clone(), path.clone())],
            FolderTarget::Smart(_) => {
                let state = self.state.borrow();
                state
                    .accounts
                    .iter()
                    .filter_map(|a| state.inbox_path(&a.id).map(|p| (a.id.clone(), p.to_string())))
                    .collect()
            }
        };

        let mut state = self.state.borrow_mut();
        for (account_id, path) in entries {
            if let Some(cached) = crate::cache::load_summaries(&account_id, &path) {
                state.buckets.insert(account_id, cached);
            }
        }
        state.remerge();
    }

    /// Ask the backend for whatever `target` needs. A unified target queries
    /// every account's inbox; the results are merged as they arrive.
    fn request_messages(self: &Rc<Self>, target: &FolderTarget) {
        let page_size = self.config.borrow().page_size;
        let backend = self.backend.borrow();
        let fresh_page = PageState { next_offset: 0, has_more: true, loading: true };

        match target {
            FolderTarget::Mailbox { account_id, path } => {
                self.state.borrow_mut().pagination.insert(account_id.clone(), fresh_page);
                backend.send(
                    account_id,
                    Command::LoadMessages { mailbox: path.clone(), limit: page_size, offset: 0 },
                );
                self.set_activity(account_id, t1("Sincronizzazione — {}", &self.describe_folder(account_id, path)));
            }
            FolderTarget::Smart(_) => {
                let targets: Vec<(String, String)> = {
                    let state = self.state.borrow();
                    state
                        .accounts
                        .iter()
                        .filter_map(|a| {
                            state.inbox_path(&a.id).map(|p| (a.id.clone(), p.to_string()))
                        })
                        .collect()
                };
                {
                    let mut state = self.state.borrow_mut();
                    for (account_id, _) in &targets {
                        state.pagination.insert(account_id.clone(), fresh_page);
                    }
                }
                for (account_id, path) in targets {
                    backend.send(
                        &account_id,
                        Command::LoadMessages { mailbox: path.clone(), limit: page_size, offset: 0 },
                    );
                    self.set_activity(
                        &account_id,
                        t1("Sincronizzazione — {}", &self.describe_folder(&account_id, &path)),
                    );
                }
            }
        }
    }

    /// Pull in the next page for every account of the current target that
    /// might still have older messages, called when the list is scrolled to
    /// its bottom edge.
    fn load_more(self: &Rc<Self>) {
        let Some(target) = self.state.borrow().current_target.clone() else { return };
        let page_size = self.config.borrow().page_size;
        let backend = self.backend.borrow();

        let requests: Vec<(String, String, u32)> = {
            let mut state = self.state.borrow_mut();
            let candidates: Vec<(String, String)> = match &target {
                FolderTarget::Mailbox { account_id, path } => {
                    vec![(account_id.clone(), path.clone())]
                }
                FolderTarget::Smart(_) => state
                    .accounts
                    .iter()
                    .filter_map(|a| {
                        state.inbox_path(&a.id).map(|p| (a.id.clone(), p.to_string()))
                    })
                    .collect(),
            };

            let mut requests = Vec::new();
            for (account_id, path) in candidates {
                let page = state.pagination.entry(account_id.clone()).or_default();
                if page.has_more && !page.loading {
                    page.loading = true;
                    requests.push((account_id, path, page.next_offset));
                }
            }
            requests
        };

        for (account_id, path, offset) in requests {
            self.set_activity(
                &account_id,
                t1("Caricamento altri messaggi — {}", &self.describe_folder(&account_id, &path)),
            );
            backend.send(
                &account_id,
                Command::LoadMessages { mailbox: path, limit: page_size, offset },
            );
        }
    }

    fn refresh_current_folder(self: &Rc<Self>) {
        let Some(target) = self.state.borrow().current_target.clone() else {
            return;
        };

        {
            let backend = self.backend.borrow();
            match &target {
                FolderTarget::Mailbox { account_id, .. } => {
                    backend.send(account_id, Command::LoadMailboxes);
                    self.set_activity(account_id, t("Aggiornamento delle cartelle…").to_string());
                }
                FolderTarget::Smart(_) => {
                    backend.broadcast(Command::LoadMailboxes);
                    let ids: Vec<String> =
                        self.state.borrow().accounts.iter().map(|a| a.id.clone()).collect();
                    for id in ids {
                        self.set_activity(&id, t("Aggiornamento delle cartelle…").to_string());
                    }
                }
            }
        }

        self.request_messages(&target);
    }

    // -------------------------------------------------------- message list

    /// Recompute and apply the message-list header's subtitle ("N messaggi
    /// · M da leggere") from the current `state.visible`/`state.messages`,
    /// without touching the list itself — used after an in-place row update
    /// that does not change which messages are visible.
    fn update_message_list_subtitle(self: &Rc<Self>) {
        let state = self.state.borrow();
        let needle = state.search.trim().to_lowercase();
        let total = state.visible.len();
        let unread =
            state.visible.iter().filter_map(|i| state.messages.get(*i)).filter(|m| !m.seen).count();

        let subtitle = if needle.is_empty() {
            match (total, unread) {
                (0, _) => t("Nessun messaggio").to_string(),
                (n, 0) => format!("{n} {}", plural(n, "messaggio", "messaggi", "message", "messages")),
                (n, u) => format!(
                    "{n} {} · {u} {}",
                    plural(n, "messaggio", "messaggi", "message", "messages"),
                    t("da leggere"),
                ),
            }
        } else {
            format!("{total} {}", plural(total, "risultato", "risultati", "result", "results"))
        };
        drop(state);
        self.widgets.folder_title.set_subtitle(&subtitle);
    }

    fn refresh_message_list(self: &Rc<Self>) {
        let widgets = &self.widgets;

        // Remember what was open by identity, taken from the message actually
        // on screen rather than from the selected row. By the time we get here
        // the merged list may already have been rebuilt underneath the old row
        // indices, so those indices no longer mean anything.
        let previously_open = self.state.borrow().current_message.as_ref().map(|message| {
            (
                message.summary.account_id.clone(),
                message.summary.mailbox.clone(),
                message.summary.uid,
            )
        });

        // Emptying the list fires `row-selected(None)`, which would blank the
        // reading pane. Suppress the handler for the whole rebuild.
        self.state.borrow_mut().restoring_message = true;

        while let Some(child) = widgets.message_list.first_child() {
            widgets.message_list.remove(&child);
        }

        let mut state = self.state.borrow_mut();
        let needle = state.search.trim().to_lowercase();

        // A smart mailbox narrows the merged list further.
        let smart = match &state.current_target {
            Some(FolderTarget::Smart(kind)) => Some(*kind),
            _ => None,
        };
        // In a unified view each row says which account it came from.
        let show_account =
            state.current_target.as_ref().map(FolderTarget::spans_accounts).unwrap_or(false);

        let unread_only = state.unread_only;
        let visible: Vec<usize> = state
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| smart.map(|k| k.accepts(message)).unwrap_or(true))
            .filter(|(_, message)| !unread_only || !message.seen)
            .filter(|(_, message)| matches_search(message, &needle))
            .map(|(index, _)| index)
            .collect();
        state.visible = visible.clone();

        let app = self.clone();
        let rows_to_add: Vec<(gtk::ListBoxRow, (String, String, u32), rows::MessageRowRefs)> = visible
            .iter()
            .filter_map(|i| state.messages.get(*i))
            .map(|message| {
                let account = if show_account {
                    state.account_label(&message.account_id)
                } else {
                    None
                };
                let app = app.clone();
                let account_id = message.account_id.clone();
                let mailbox = message.mailbox.clone();
                let uid = message.uid;
                let (row, refs) = rows::message_row(message, account.as_deref(), {
                    let account_id = account_id.clone();
                    let mailbox = mailbox.clone();
                    let app = app.clone();
                    move |action| app.handle_swipe_action(&account_id, &mailbox, uid, action)
                });

                let right_click = gtk::GestureClick::new();
                right_click.set_button(3);
                {
                    let app = app.clone();
                    let account_id = account_id.clone();
                    let mailbox = mailbox.clone();
                    let row_weak = row.downgrade();
                    let subject = message.subject_or_placeholder().to_string();
                    right_click.connect_pressed(move |gesture, n_press, x, y| {
                        // Explicitly resolve the sequence instead of leaving
                        // it at the default `None` state — otherwise the
                        // implicit grab GtkPopover installs on `popup()` can
                        // leave this gesture's sequence unresolved from the
                        // previous right-click, and it silently stops
                        // recognising presses after the first one.
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        match row_weak.upgrade() {
                            Some(row) => {
                                log::debug!(
                                    "context-menu: press #{n_press} on \"{subject}\" (uid={uid}) \
                                     at ({x:.0},{y:.0}), row size={}x{}",
                                    row.width(),
                                    row.height(),
                                );
                                app.show_message_context_menu(&row, &account_id, &mailbox, uid, x, y);
                            }
                            None => {
                                log::debug!(
                                    "context-menu: press on \"{subject}\" (uid={uid}) but the row \
                                     widget is already gone — this is why nothing appeared"
                                );
                            }
                        }
                    });
                }
                row.add_controller(right_click);

                (row, (account_id, mailbox, uid), refs)
            })
            .collect();
        drop(state);

        let mut row_refs = HashMap::new();
        for (row, key, refs) in rows_to_add {
            row_refs.insert(key, refs);
            widgets.message_list.append(&row);
        }
        self.state.borrow_mut().row_refs = row_refs;

        widgets.list_stack.set_visible_child_name(if visible.is_empty() {
            "empty"
        } else {
            "list"
        });

        self.update_message_list_subtitle();

        // Put the user back on the message they had open. Re-selecting must not
        // re-open it: it is already rendered, and a second fetch would flicker
        // the reading pane.
        if let Some((account_id, mailbox, uid)) = previously_open {
            let row_index = {
                let state = self.state.borrow();
                state.visible.iter().position(|i| {
                    state
                        .messages
                        .get(*i)
                        .map(|m| {
                            m.uid == uid && m.mailbox == mailbox && m.account_id == account_id
                        })
                        .unwrap_or(false)
                })
            };

            match row_index.and_then(|i| widgets.message_list.row_at_index(i as i32)) {
                Some(row) => widgets.message_list.select_row(Some(&row)),
                // It was filtered out or moved away; the pane has nothing left
                // to show.
                None => {
                    widgets.message_view.show_empty();
                    self.state.borrow_mut().current_message = None;
                    self.set_actions_enabled(false);
                }
            }
        }

        self.state.borrow_mut().restoring_message = false;
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

        // A message already downloaded once needs no second trip to the
        // server: show the cached copy straight away.
        match crate::cache::load_message(&account_id, &mailbox, uid) {
            Some(cached) => {
                let prefs = self.render_prefs();
                self.widgets.message_view.show_message(&cached, prefs);
                self.state.borrow_mut().current_message = Some(cached);
                self.set_actions_enabled(true);
            }
            None => {
                backend.send(
                    &account_id,
                    Command::LoadMessage { mailbox: mailbox.clone(), uid },
                );
            }
        }

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
        // Only once: batches from slower accounts must not re-open a message
        // and mark it read behind the user's back.
        if self.state.borrow().preselection_done {
            return;
        }
        if let Some(row) = self.widgets.message_list.row_at_index(index) {
            self.state.borrow_mut().preselection_done = true;
            self.widgets.message_list.select_row(Some(&row));
        }
    }

    fn rerender_body(self: &Rc<Self>) {
        let prefs = self.render_prefs();
        let state = self.state.borrow();
        if let Some(message) = &state.current_message {
            self.widgets.message_view.render_body(message, prefs);
        }
    }

    /// The current display preferences that affect how a message body is
    /// rendered.
    fn render_prefs(self: &Rc<Self>) -> RenderPrefs {
        let config = self.config.borrow();
        RenderPrefs {
            dark: self.state.borrow().dark,
            appearance: config.message_appearance,
            load_remote_content: config.load_remote_content,
        }
    }

    // ------------------------------------------------------------- actions

    fn selected_message(&self) -> Option<(String, String, u32)> {
        let row = self.widgets.message_list.selected_row()?;
        let state = self.state.borrow();
        let summary = state.selected_summary(row.index() as usize)?;
        Some((summary.account_id.clone(), summary.mailbox.clone(), summary.uid))
    }

    /// Set a flag on a specific message, independent of what is selected —
    /// used both by the selection-driven actions and by swiping a row that
    /// need not be the selected one.
    fn set_flag_on(self: &Rc<Self>, account_id: &str, mailbox: &str, uid: u32, flag: &str, on: bool) {
        self.backend.borrow().send(
            account_id,
            Command::SetFlag { mailbox: mailbox.to_string(), uid, flag: flag.to_string(), on },
        );
    }

    fn set_flag_on_selection(self: &Rc<Self>, flag: &str, on: bool) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        self.set_flag_on(&account_id, &mailbox, uid, flag, on);
    }

    fn toggle_read(self: &Rc<Self>, account_id: &str, mailbox: &str, uid: u32) {
        let seen = {
            let state = self.state.borrow();
            state
                .messages
                .iter()
                .find(|m| m.account_id == account_id && m.mailbox == mailbox && m.uid == uid)
                .map(|m| m.seen)
        };
        let Some(seen) = seen else { return };
        self.set_flag_on(account_id, mailbox, uid, "\\Seen", !seen);
    }

    fn toggle_read_on_selection(self: &Rc<Self>) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        self.toggle_read(&account_id, &mailbox, uid);
    }

    fn move_to(self: &Rc<Self>, account_id: &str, mailbox: &str, uid: u32, kind: MailboxKind) {
        let target = {
            let state = self.state.borrow();
            state
                .mailboxes
                .get(account_id)
                .and_then(|list| list.iter().find(|m| m.kind == kind))
                .map(|m| m.path.clone())
        };
        let Some(target) = target else {
            self.toast(&t1("Nessuna cartella {} su questo account", label_for(kind)));
            return;
        };
        if target == mailbox {
            return;
        }
        self.backend.borrow().send(
            account_id,
            Command::MoveMessage { mailbox: mailbox.to_string(), uid, target },
        );
    }

    fn move_selection_to(self: &Rc<Self>, kind: MailboxKind) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        self.move_to(&account_id, &mailbox, uid, kind);
    }

    /// Select whichever row currently shows this (mailbox, uid) — used to
    /// open a message before replying to it from the context menu, without
    /// assuming it is already the selection.
    fn select_message_row(self: &Rc<Self>, mailbox: &str, uid: u32) {
        let index = {
            let state = self.state.borrow();
            state.visible.iter().position(|&i| {
                state.messages.get(i).map(|m| m.mailbox == mailbox && m.uid == uid).unwrap_or(false)
            })
        };
        let Some(index) = index else { return };
        if let Some(row) = self.widgets.message_list.row_at_index(index as i32) {
            self.widgets.message_list.select_row(Some(&row));
        }
    }

    fn delete_message(self: &Rc<Self>, account_id: &str, mailbox: &str, uid: u32) {
        self.backend
            .borrow()
            .send(account_id, Command::DeleteMessage { mailbox: mailbox.to_string(), uid });
    }

    fn delete_selection(self: &Rc<Self>) {
        let Some((account_id, mailbox, uid)) = self.selected_message() else { return };
        self.delete_message(&account_id, &mailbox, uid);
    }

    /// Dispatch a swipe/long-press action from a message row to the
    /// matching backend command.
    fn handle_swipe_action(
        self: &Rc<Self>,
        account_id: &str,
        mailbox: &str,
        uid: u32,
        action: rows::SwipeAction,
    ) {
        match action {
            rows::SwipeAction::ToggleRead => self.toggle_read(account_id, mailbox, uid),
            rows::SwipeAction::Flag => self.set_flag_on(account_id, mailbox, uid, "\\Flagged", true),
            rows::SwipeAction::Archive => self.move_to(account_id, mailbox, uid, MailboxKind::Archive),
            rows::SwipeAction::Delete => self.delete_message(account_id, mailbox, uid),
        }
    }

    /// Right-click on a message row: select it (so Reply/Reply all/Forward
    /// have a loaded message to work from, same as a left click) and pop up
    /// the action menu at the click position.
    /// Right-click on a message row. Built from plain buttons with direct
    /// closures — not a `gio::Menu` of parametrised `win.*` actions — so
    /// each item calls straight into the method that does the work, with no
    /// action-group lookup or GVariant (de)serialisation in between to get
    /// subtly wrong. Deliberately does not select/open the row: a
    /// right-click must act on exactly this message without the side
    /// effects of opening it (opening marks a message read, which would
    /// make "mark as unread" from this very menu immediately undo itself).
    fn show_message_context_menu(
        self: &Rc<Self>,
        row: &gtk::ListBoxRow,
        account_id: &str,
        mailbox: &str,
        uid: u32,
        x: f64,
        y: f64,
    ) {
        log::debug!(
            "context-menu: show_message_context_menu uid={uid} mailbox={mailbox} \
             pointing_to=({x:.0},{y:.0}) row size={}x{}",
            row.width(),
            row.height(),
        );

        let popover = gtk::Popover::new();
        popover.set_parent(row);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // Popovers stay parented once set; unparent on close so repeated
        // right-clicks on the same row do not pile up dead popovers on it.
        popover.connect_closed(|popover| {
            log::debug!("context-menu: popover closed, unparenting");
            popover.unparent();
        });
        popover.connect_visible_notify(|popover| {
            log::debug!("context-menu: visible={}", popover.is_visible());
        });

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("context-menu");
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        popover.set_child(Some(&content));

        let account_id = account_id.to_string();
        let mailbox = mailbox.to_string();

        // Appends one row that runs `action` and closes the popover, or —
        // for the "Sposta in" row — opens a second popover instead.
        let add_item = |label: &str, action: Box<dyn Fn(Rc<Self>)>| {
            let button = gtk::Button::builder().css_classes(["flat"]).build();
            let inner = gtk::Label::new(Some(label));
            inner.set_xalign(0.0);
            button.set_child(Some(&inner));
            content.append(&button);

            let app = self.clone();
            let popover = popover.clone();
            button.connect_clicked(move |_| {
                action(app.clone());
                popover.popdown();
            });
        };

        {
            let mailbox = mailbox.clone();
            add_item(
                t("Rispondi"),
                Box::new(move |app| {
                    app.select_message_row(&mailbox, uid);
                    app.open_compose(ComposeKind::Reply);
                }),
            );
        }
        {
            let mailbox = mailbox.clone();
            add_item(
                t("Rispondi a tutti"),
                Box::new(move |app| {
                    app.select_message_row(&mailbox, uid);
                    app.open_compose(ComposeKind::ReplyAll);
                }),
            );
        }
        {
            let mailbox = mailbox.clone();
            add_item(
                t("Inoltra"),
                Box::new(move |app| {
                    app.select_message_row(&mailbox, uid);
                    app.open_compose(ComposeKind::Forward);
                }),
            );
        }

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let seen = self
            .state
            .borrow()
            .messages
            .iter()
            .find(|m| m.account_id == account_id && m.mailbox == mailbox && m.uid == uid)
            .map(|m| m.seen)
            .unwrap_or(true);
        let read_label = if seen { t("Segna come non letto") } else { t("Segna come letto") };
        {
            let account_id = account_id.clone();
            let mailbox = mailbox.clone();
            add_item(
                read_label,
                Box::new(move |app| app.toggle_read(&account_id, &mailbox, uid)),
            );
        }

        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        {
            let account_id = account_id.clone();
            let mailbox = mailbox.clone();
            add_item(
                t("Archivia"),
                Box::new(move |app| app.move_to(&account_id, &mailbox, uid, MailboxKind::Archive)),
            );
        }
        {
            let account_id = account_id.clone();
            let mailbox = mailbox.clone();
            add_item(
                t("Indesiderata"),
                Box::new(move |app| app.move_to(&account_id, &mailbox, uid, MailboxKind::Junk)),
            );
        }
        {
            let account_id = account_id.clone();
            let mailbox = mailbox.clone();
            add_item(
                t("Elimina"),
                Box::new(move |app| app.delete_message(&account_id, &mailbox, uid)),
            );
        }

        let mailboxes = self.state.borrow().mailboxes.get(&account_id).cloned().unwrap_or_default();
        let other_folders: Vec<_> = mailboxes.into_iter().filter(|m| m.path != mailbox).collect();
        if !other_folders.is_empty() {
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let move_label = gtk::Label::new(Some(t("Sposta in")));
            move_label.set_xalign(0.0);
            move_label.add_css_class("dim-label");
            move_label.set_margin_start(10);
            move_label.set_margin_top(4);
            move_label.set_margin_bottom(2);
            content.append(&move_label);

            for folder in other_folders {
                let account_id = account_id.clone();
                let source = mailbox.clone();
                let target = folder.path.clone();
                add_item(
                    &folder.name,
                    Box::new(move |app| {
                        app.backend.borrow().send(
                            &account_id,
                            Command::MoveMessage {
                                mailbox: source.clone(),
                                uid,
                                target: target.clone(),
                            },
                        );
                    }),
                );
            }
        }

        popover.popup();
        log::debug!("context-menu: popup() called, mapped={}", popover.is_mapped());
    }

    /// The account whose mailbox is currently open, falling back to the first
    /// configured one so "new message" works before anything is selected.
    fn active_account(&self) -> Option<Account> {
        // In a unified view the folder does not identify an account, so the
        // open message decides who a reply comes from.
        if let Some((account_id, _, _)) = self.selected_message() {
            let state = self.state.borrow();
            if let Some(account) = state.accounts.iter().find(|a| a.id == account_id) {
                return Some(account.clone());
            }
        }

        let state = self.state.borrow();
        let id = match &state.current_target {
            Some(FolderTarget::Mailbox { account_id, .. }) => Some(account_id.clone()),
            _ => None,
        };
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
                app.show_error_dialog(t("Impossibile salvare la password"), &format!("{e:#}"));
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
            app.toast(&t1("Account {} aggiunto", &account.email));
        });
    }

    fn open_preferences(self: &Rc<Self>) {
        let accounts = self.state.borrow().accounts.clone();
        let app = self.clone();
        preferences::open(&self.widgets.window, self.config.clone(), accounts, move || {
            app.rerender_body();
        });
    }

    fn open_compose(self: &Rc<Self>, kind: ComposeKind) {
        let Some(account) = self.active_account() else {
            self.toast(t("Nessun account configurato"));
            return;
        };

        // Replies need the open message; a new message does not.
        let state = self.state.borrow();
        let message = state.current_message.as_ref();
        if kind != ComposeKind::New && message.is_none() {
            drop(state);
            self.toast(t("Apri prima un messaggio"));
            return;
        }
        let signature = self.config.borrow().signature(&account.id);
        let prefilled = compose::prefill(kind, &account, message, &signature);
        drop(state);

        let app = self.clone();
        let app_for_draft = self.clone();
        let accounts = self.state.borrow().accounts.clone();
        let known_contacts = self.known_contacts();
        compose::open(
            &self.widgets.window,
            kind,
            &accounts,
            &account,
            prefilled,
            known_contacts,
            move |sender, outgoing| app.send_with_undo(sender, outgoing),
            move |sender, outgoing| app_for_draft.save_draft(sender, outgoing),
        );
    }

    fn save_draft(self: &Rc<Self>, sender: Account, outgoing: Outgoing) {
        self.toast(t("Salvataggio della bozza…"));
        self.backend.borrow().send(&sender.id, Command::SaveDraft { outgoing });
    }

    /// Send a message, honouring the configured send delay: with a delay
    /// set, the message sits in an undo window (a toast with an "Annulla"
    /// button) before it actually goes out.
    fn send_with_undo(self: &Rc<Self>, sender: Account, outgoing: Outgoing) {
        let delay = self.config.borrow().send_delay;
        let seconds = delay.seconds();
        if seconds == 0 {
            self.toast(t("Invio in corso…"));
            self.backend.borrow().send(&sender.id, Command::Send { outgoing });
            return;
        }

        let cancelled = Rc::new(Cell::new(false));

        let toast = adw::Toast::new(&t1("Invio tra {}…", &delay.label().to_lowercase()));
        toast.set_button_label(Some(t("Annulla")));
        toast.set_priority(adw::ToastPriority::High);
        toast.set_timeout(seconds);
        {
            let cancelled = cancelled.clone();
            toast.connect_button_clicked(move |_| cancelled.set(true));
        }
        self.widgets.toast_overlay.add_toast(toast);

        let app = self.clone();
        let mut outgoing = Some(outgoing);
        glib::timeout_add_seconds_local(seconds, move || {
            if !cancelled.get() {
                if let Some(outgoing) = outgoing.take() {
                    app.backend.borrow().send(&sender.id, Command::Send { outgoing });
                }
            }
            glib::ControlFlow::Break
        });
    }

    /// Addresses already seen among the currently loaded messages, deduped
    /// by address, for the compose window's suggestion popovers.
    fn known_contacts(&self) -> Vec<Mailaddr> {
        let state = self.state.borrow();
        let mut seen = std::collections::HashSet::new();
        let mut contacts = Vec::new();
        for message in &state.messages {
            for addr in std::iter::once(&message.from).chain(message.to.iter()) {
                if addr.address.trim().is_empty() {
                    continue;
                }
                if seen.insert(addr.address.to_lowercase()) {
                    contacts.push(addr.clone());
                }
            }
        }
        contacts
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

    /// Report a failure the user needs to actually notice and dismiss —
    /// a connection or credentials problem, say — as a modal dialog rather
    /// than the easy-to-miss banner or an auto-dismissing toast.
    fn show_error_dialog(&self, context: &str, detail: &str) {
        let dialog = adw::AlertDialog::new(Some(context), Some(detail));
        dialog.add_response("ok", t("Chiudi"));
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("ok");
        dialog.present(Some(&self.widgets.window));
    }

    // --------------------------------------------------------- status bar

    /// Human label for a sync in progress, e.g. "Lavoro — In arrivo".
    fn describe_folder(&self, account_id: &str, path: &str) -> String {
        let state = self.state.borrow();
        let account = state.account_label(account_id).unwrap_or_else(|| account_id.to_string());
        let folder =
            state.mailbox(account_id, path).map(|m| m.name.clone()).unwrap_or_else(|| path.to_string());
        format!("{account} — {folder}")
    }

    fn set_activity(self: &Rc<Self>, account_id: &str, text: String) {
        self.state.borrow_mut().activity.insert(account_id.to_string(), text);
        self.refresh_status_bar();
    }

    fn clear_activity(self: &Rc<Self>, account_id: &str) {
        let mut state = self.state.borrow_mut();
        state.activity.remove(account_id);
        if state.activity.is_empty() {
            state.last_sync = Some(chrono::Local::now());
        }
        drop(state);
        self.refresh_status_bar();
    }

    /// Reflect `state.activity` in the bottom status bar: spinning and
    /// describing whatever is in flight, or — once idle — how long ago the
    /// last sync finished.
    fn refresh_status_bar(self: &Rc<Self>) {
        let state = self.state.borrow();
        if state.activity.is_empty() {
            self.widgets.status_spinner.set_visible(false);
            self.widgets.status_spinner.set_spinning(false);
            let text = match state.last_sync {
                Some(when) => t1("Ultima sincronizzazione: {}", &relative_time(when)),
                None => t("Pronto").to_string(),
            };
            self.widgets.status_label.set_label(&text);
        } else {
            self.widgets.status_spinner.set_visible(true);
            self.widgets.status_spinner.set_spinning(true);
            let text = state.activity.values().cloned().collect::<Vec<_>>().join("  ·  ");
            self.widgets.status_label.set_label(&text);
        }
    }
}

/// A moment.js-style relative timestamp: "adesso", "5 min fa", "un'ora fa"…
fn relative_time(from: chrono::DateTime<chrono::Local>) -> String {
    let seconds = (chrono::Local::now() - from).num_seconds().max(0);
    match seconds {
        0..=59 => t("adesso").to_string(),
        60..=3599 => {
            let minutes = seconds / 60;
            if minutes == 1 { t("1 min fa").to_string() } else { t1("{} min fa", &minutes.to_string()) }
        }
        3600..=86399 => {
            let hours = seconds / 3600;
            if hours == 1 { t("un'ora fa").to_string() } else { t1("{} ore fa", &hours.to_string()) }
        }
        _ => {
            let days = seconds / 86400;
            if days == 1 { t("ieri").to_string() } else { t1("{} giorni fa", &days.to_string()) }
        }
    }
}

fn label_for(kind: MailboxKind) -> &'static str {
    match kind {
        MailboxKind::Archive => t("Archivio"),
        MailboxKind::Junk => t("Indesiderata"),
        MailboxKind::Trash => t("Cestino"),
        MailboxKind::Inbox => t("In arrivo"),
        MailboxKind::Sent => t("Inviata"),
        MailboxKind::Drafts => t("Bozze"),
        MailboxKind::Flagged => t("Speciali"),
        MailboxKind::Other => t("Cartella"),
    }
}

/// Italian agreement for the counters in the pane subtitles.
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

fn build_widgets(application: &adw::Application, config: &Config) -> Widgets {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title(t("Posta"))
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
    appearance.append(Some(t("Automatico (sistema)")), Some("win.theme('system')"));
    appearance.append(Some(t("Chiaro")), Some("win.theme('light')"));
    appearance.append(Some(t("Scuro")), Some("win.theme('dark')"));
    menu.append_submenu(Some(t("Aspetto")), &appearance);

    let tools = gio::Menu::new();
    tools.append(Some(t("Aggiungi account IMAP…")), Some("win.add-account"));
    tools.append(Some(t("Aggiorna")), Some("win.refresh"));
    tools.append(Some(t("Cerca")), Some("win.search"));
    menu.append_section(None, &tools);

    let prefs = gio::Menu::new();
    prefs.append(Some(t("Preferenze…")), Some("win.preferences"));
    menu.append_section(None, &prefs);

    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text(t("Menu principale"))
        .build();

    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new(t("Caselle"), "")));
    sidebar_header.pack_end(&menu_button);
    // Only the rightmost column (the reader) gets the window's close button;
    // GtkPaned, unlike AdwNavigationSplitView, has no notion of which pane is
    // "outermost" and gives every header bar its own by default.
    sidebar_header.set_show_start_title_buttons(false);
    sidebar_header.set_show_end_title_buttons(false);

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
        .title(t("Nessun messaggio"))
        .description(t("Questa cartella è vuota."))
        .build();

    let list_stack = gtk::Stack::new();
    list_stack.add_named(&list_scroll, Some("list"));
    list_stack.add_named(&empty_list, Some("empty"));
    list_stack.set_visible_child_name("empty");
    list_stack.set_vexpand(true);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some(t("Cerca nei messaggi")));
    search_entry.set_hexpand(true);

    let search_bar = gtk::SearchBar::builder().child(&search_entry).build();
    search_bar.set_key_capture_widget(Some(&window));
    search_bar.connect_entry(&search_entry);

    let folder_title = adw::WindowTitle::new(t("Posta"), "");

    let search_button = gtk::ToggleButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text(t("Cerca (Ctrl+F)"))
        .build();
    search_button
        .bind_property("active", &search_bar, "search-mode-enabled")
        .bidirectional()
        .sync_create()
        .build();

    let compose_button = gtk::Button::builder()
        .icon_name("mailview-compose-symbolic")
        .tooltip_text(t("Nuovo messaggio (Ctrl+N)"))
        .action_name("win.compose")
        .build();

    let sidebar_toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text(t("Mostra/nascondi le caselle"))
        .active(true)
        .build();
    {
        let sidebar_view = sidebar_view.clone();
        sidebar_toggle.connect_toggled(move |button| {
            sidebar_view.set_visible(button.is_active());
        });
    }

    let unread_filter_button = gtk::ToggleButton::builder()
        .icon_name("mailview-unread-symbolic")
        .tooltip_text(t("Mostra solo i non letti"))
        .build();

    let list_header = adw::HeaderBar::new();
    list_header.set_title_widget(Some(&folder_title));
    list_header.pack_start(&sidebar_toggle);
    list_header.pack_end(&search_button);
    list_header.pack_end(&unread_filter_button);
    list_header.set_show_start_title_buttons(false);
    list_header.set_show_end_title_buttons(false);

    let list_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_body.append(&search_bar);
    list_body.append(&list_stack);

    let list_view = adw::ToolbarView::new();
    list_view.add_top_bar(&list_header);
    list_view.set_content(Some(&list_body));

    // ---- pane 3: the message ---------------------------------------------
    let message_view = MessageView::new();

    let reply_button = gtk::Button::builder()
        .icon_name("mailview-reply-symbolic")
        .tooltip_text(t("Rispondi (Ctrl+Invio)"))
        .action_name("win.reply")
        .build();
    let reply_all_button = gtk::Button::builder()
        .icon_name("mailview-reply-all-symbolic")
        .tooltip_text(t("Rispondi a tutti (Ctrl+Maiusc+Invio)"))
        .action_name("win.reply-all")
        .build();
    let forward_button = gtk::Button::builder()
        .icon_name("mailview-forward-symbolic")
        .tooltip_text(t("Inoltra"))
        .action_name("win.forward")
        .build();

    let archive_button = gtk::Button::builder()
        .icon_name("mail-archive-symbolic")
        .tooltip_text(t("Archivia (Ctrl+E)"))
        .action_name("win.archive")
        .build();
    let junk_button = gtk::Button::builder()
        .icon_name("dialog-warning-symbolic")
        .tooltip_text(t("Segna come indesiderata"))
        .action_name("win.junk")
        .build();
    let delete_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(t("Elimina (Canc)"))
        .action_name("win.delete")
        .build();
    let flag_button = gtk::ToggleButton::builder()
        .icon_name("starred-symbolic")
        .tooltip_text(t("Contrassegna"))
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

    // A gap the width of one button, to separate the action groups instead
    // of running them together.
    let toolbar_gap = || {
        let gap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        gap.set_size_request(32, -1);
        gap
    };

    let reader_header = adw::HeaderBar::new();
    // A header bar with no title widget of its own falls back to showing
    // the *window's* title — which AdwWindowTitle elsewhere (list_header's
    // "Posta") keeps in sync as the window's own title property. An
    // explicit blank title stops that fallback from kicking in here.
    reader_header.set_title_widget(Some(&gtk::Label::new(None)));
    reader_header.pack_start(&compose_button);
    reader_header.pack_start(&toolbar_gap());
    reader_header.pack_start(&reply_group);
    reader_header.pack_start(&toolbar_gap());
    reader_header.pack_start(&filing_group);
    reader_header.pack_start(&toolbar_gap());
    reader_header.pack_start(&flag_button);

    let reader_view = adw::ToolbarView::new();
    reader_view.add_top_bar(&reader_header);
    reader_view.set_content(Some(&message_view.root));

    // ---- assemble the three panes ----------------------------------------
    // Plain GtkPaned, not AdwNavigationSplitView: the latter only ever
    // offered a *preferred* width and never grew a drag handle, so nothing
    // the user did in the window could actually resize a pane. Paned gives a
    // real, mouse-draggable handle between each pair of panes; the adaptive
    // collapse-on-narrow-window behaviour is traded away for that.
    list_view.set_size_request(280, -1);
    reader_view.set_size_request(320, -1);
    let inner_paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&list_view)
        .end_child(&reader_view)
        .resize_start_child(false)
        .shrink_start_child(false)
        .resize_end_child(true)
        .shrink_end_child(false)
        .wide_handle(true)
        .position(config.message_list_width)
        .build();

    sidebar_view.set_size_request(180, -1);
    let outer_paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&sidebar_view)
        .end_child(&inner_paned)
        .resize_start_child(false)
        .shrink_start_child(false)
        .resize_end_child(true)
        .shrink_end_child(false)
        .wide_handle(true)
        .position(config.sidebar_width)
        .build();

    let banner = adw::Banner::new("");
    banner.set_revealed(false);
    banner.set_button_label(Some(t("Chiudi")));
    {
        let banner_clone = banner.clone();
        banner.connect_button_clicked(move |_| banner_clone.set_revealed(false));
    }

    // ---- bottom status bar -------------------------------------------
    let sync_button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(t("Forza la sincronizzazione (Ctrl+R)"))
        .action_name("win.refresh")
        .css_classes(["flat", "circular"])
        .build();

    let status_spinner = gtk::Spinner::new();
    status_spinner.set_spinning(true);
    status_spinner.set_visible(false);

    let status_label = gtk::Label::new(Some(t("Pronto")));
    status_label.add_css_class("caption");
    status_label.add_css_class("dim-label");
    status_label.set_halign(gtk::Align::Start);
    status_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status_label.set_hexpand(true);

    let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    status_bar.add_css_class("mail-status-bar");
    status_bar.set_margin_start(4);
    status_bar.set_margin_end(10);
    status_bar.set_margin_top(3);
    status_bar.set_margin_bottom(3);
    status_bar.append(&sync_button);
    status_bar.append(&status_spinner);
    status_bar.append(&status_label);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&banner);
    content.append(&outer_paned);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&status_bar);
    outer_paned.set_vexpand(true);

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
        list_scroll,
        banner,
        toast_overlay,
        flag_button,
        unread_filter_button,
        action_buttons,
        status_spinner,
        status_label,
        outer_paned,
        inner_paned,
    }
}

/// Register the compiled-in icon set with the display's icon theme.
///
/// Bundling the icons means the application never depends on a matching
/// version being installed system-wide.
pub fn load_icons() {
    // `Bytes::from` copies into GLib-owned memory, which keeps the GVDB header
    // aligned; `from_static` over `include_bytes!` is not guaranteed to be.
    let data = glib::Bytes::from(
        &include_bytes!(concat!(env!("OUT_DIR"), "/mailview.gresource"))[..],
    );

    match gio::Resource::from_data(&data) {
        Ok(resource) => {
            gio::resources_register(&resource);
            if let Some(display) = gtk::gdk::Display::default() {
                gtk::IconTheme::for_display(&display)
                    .add_resource_path("/it/antoniopicone/MailView/icons");
            }
        }
        Err(e) => log::error!("could not register the bundled icons: {e}"),
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
