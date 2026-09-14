//! The reading pane: message header plus a sandboxed WebView for the body.

use gtk4 as gtk;
// use gtk::prelude::*;
use libadwaita as adw;
use webkit6::prelude::*;

use crate::config::MessageAppearance;
use crate::html;
use crate::model::Message;
use crate::ui::rows;

/// The subset of preferences that affect how a message body is rendered.
#[derive(Debug, Clone, Copy)]
pub struct RenderPrefs {
    pub dark: bool,
    pub appearance: MessageAppearance,
    pub load_remote_content: bool,
}

pub struct MessageView {
    pub root: gtk::Box,
    avatar: adw::Avatar,
    subject: gtk::Label,
    from: gtk::Label,
    recipients: gtk::Label,
    date: gtk::Label,
    attachments: gtk::Box,
    webview: webkit6::WebView,
    placeholder: adw::StatusPage,
    stack: gtk::Stack,
}

impl MessageView {
    pub fn new() -> Self {
        // Mail is untrusted input: no scripting, no local storage, and no
        // loading of anything the message did not already carry.
        let settings = webkit6::Settings::new();
        settings.set_enable_javascript(false);
        settings.set_enable_javascript_markup(false);
        settings.set_enable_html5_database(false);
        settings.set_enable_html5_local_storage(false);
        settings.set_enable_developer_extras(false);
        settings.set_enable_page_cache(false);
        settings.set_javascript_can_access_clipboard(false);
        settings.set_enable_back_forward_navigation_gestures(false);

        let webview = webkit6::WebView::builder().settings(&settings).build();
        webview.set_vexpand(true);
        webview.set_hexpand(true);

        // Links open in the user's browser rather than inside the mail window.
        webview.connect_decide_policy(|view, decision, decision_type| {
            if decision_type != webkit6::PolicyDecisionType::NavigationAction {
                return false;
            }
            let Ok(nav) = decision.clone().downcast::<webkit6::NavigationPolicyDecision>() else {
                return false;
            };
            let Some(mut action) = nav.navigation_action() else { return false };
            let Some(request) = action.request() else { return false };
            let Some(uri) = request.uri() else { return false };

            // The initial load_html navigation must be allowed through.
            if uri.starts_with("about:") || uri.is_empty() {
                return false;
            }

            decision.ignore();
            if uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("mailto:")
            {
                let launcher = gtk::UriLauncher::new(&uri);
                let window = view.root().and_downcast::<gtk::Window>();
                launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |result| {
                    if let Err(e) = result {
                        log::warn!("could not open the link: {e}");
                    }
                });
            }
            true
        });

        // ---- header ------------------------------------------------------
        let header = gtk::Box::new(gtk::Orientation::Vertical, 6);
        header.add_css_class("message-header");

        let subject = gtk::Label::new(None);
        subject.set_xalign(0.0);
        subject.set_wrap(true);
        subject.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        subject.set_lines(2);
        subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subject.add_css_class("reader-subject");
        subject.set_selectable(true);
        header.append(&subject);

        let identity = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        identity.set_margin_top(6);

        let avatar = adw::Avatar::new(38, None, true);
        avatar.set_valign(gtk::Align::Center);
        identity.append(&avatar);

        let who = gtk::Box::new(gtk::Orientation::Vertical, 1);
        who.set_hexpand(true);
        who.set_valign(gtk::Align::Center);

        let from = gtk::Label::new(None);
        from.set_xalign(0.0);
        from.set_ellipsize(gtk::pango::EllipsizeMode::End);
        from.add_css_class("reader-from");
        from.set_selectable(true);
        who.append(&from);

        let recipients = gtk::Label::new(None);
        recipients.set_xalign(0.0);
        recipients.set_ellipsize(gtk::pango::EllipsizeMode::End);
        recipients.add_css_class("reader-meta");
        who.append(&recipients);

        identity.append(&who);

        let date = gtk::Label::new(None);
        date.set_valign(gtk::Align::Start);
        date.add_css_class("reader-date");
        identity.append(&date);

        header.append(&identity);

        // ---- attachments -------------------------------------------------
        let attachments = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        attachments.add_css_class("attachment-bar");
        attachments.set_visible(false);

        // ---- empty state -------------------------------------------------
        let placeholder = adw::StatusPage::builder()
            .icon_name("mail-unread-symbolic")
            .title("Nessun messaggio selezionato")
            .description("Scegli una conversazione dall'elenco per leggerla qui.")
            .build();
        placeholder.set_vexpand(true);

        let reader = gtk::Box::new(gtk::Orientation::Vertical, 0);
        reader.append(&header);
        reader.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        reader.append(&webview);
        reader.append(&attachments);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);
        stack.add_named(&placeholder, Some("empty"));
        stack.add_named(&reader, Some("message"));
        stack.set_visible_child_name("empty");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&stack);
        stack.set_vexpand(true);

        Self {
            root,
            avatar,
            subject,
            from,
            recipients,
            date,
            attachments,
            webview,
            placeholder,
            stack,
        }
    }

    /// Show a fully loaded message.
    pub fn show_message(&self, message: &Message, prefs: RenderPrefs) {
        let summary = &message.summary;

        self.subject.set_text(summary.subject_or_placeholder());
        self.avatar.set_text(Some(summary.from.label()));
        self.from.set_text(&summary.from.full());

        let mut meta = String::new();
        if !summary.to.is_empty() {
            let names: Vec<String> =
                summary.to.iter().take(3).map(|a| a.label().to_string()).collect();
            meta.push_str(&format!("A: {}", names.join(", ")));
            if summary.to.len() > 3 {
                meta.push_str(&format!(" e altri {}", summary.to.len() - 3));
            }
        }
        if !message.cc.is_empty() {
            let names: Vec<String> =
                message.cc.iter().take(2).map(|a| a.label().to_string()).collect();
            if !meta.is_empty() {
                meta.push_str("  ·  ");
            }
            meta.push_str(&format!("Cc: {}", names.join(", ")));
        }
        self.recipients.set_text(&meta);
        self.recipients.set_visible(!meta.is_empty());

        self.date.set_text(&summary.date.format("%d %b %Y, %H:%M").to_string());

        // Attachments.
        while let Some(child) = self.attachments.first_child() {
            self.attachments.remove(&child);
        }
        if message.attachments.is_empty() {
            self.attachments.set_visible(false);
        } else {
            let label = gtk::Label::new(Some(&format!(
                "{} allegat{}",
                message.attachments.len(),
                if message.attachments.len() == 1 { "o" } else { "i" }
            )));
            label.add_css_class("dim-label");
            self.attachments.append(&label);
            for attachment in message.attachments.iter().take(6) {
                let window = self.root.root().and_downcast::<gtk::Window>();
                let owned = attachment.clone();
                let chip = rows::attachment_chip(
                    &attachment.filename,
                    &attachment.human_size(),
                    &attachment.mime_type,
                    move || save_attachment(window.as_ref(), &owned),
                );
                self.attachments.append(&chip);
            }
            self.attachments.set_visible(true);
        }

        self.render_body(message, prefs);
        self.stack.set_visible_child_name("message");
    }

    /// Re-render the body, e.g. after the system switched to dark mode or a
    /// display preference changed.
    pub fn render_body(&self, message: &Message, prefs: RenderPrefs) {
        let document = match &message.html {
            Some(body) if !body.trim().is_empty() => {
                let body = if prefs.load_remote_content {
                    html::allow_remote_images(body)
                } else {
                    body.clone()
                };
                html::wrap_document(&body, prefs.dark, prefs.appearance)
            }
            _ => html::plain_text_document(&message.text, prefs.dark, prefs.appearance),
        };

        let effective_dark = prefs.dark && prefs.appearance != MessageAppearance::AcceptSenderFormat;
        self.webview.set_background_color(&background_rgba(effective_dark));
        self.webview.load_html(&document, None);
    }

    /// Show a message that is still being fetched.
    pub fn show_loading(&self, subject: &str) {
        self.placeholder.set_icon_name(Some("content-loading-symbolic"));
        self.placeholder.set_title("Caricamento…");
        self.placeholder.set_description(Some(subject));
        self.stack.set_visible_child_name("empty");
    }

    /// Back to the neutral empty state.
    pub fn show_empty(&self) {
        self.placeholder.set_icon_name(Some("mail-unread-symbolic"));
        self.placeholder.set_title("Nessun messaggio selezionato");
        self.placeholder
            .set_description(Some("Scegli una conversazione dall'elenco per leggerla qui."));
        self.stack.set_visible_child_name("empty");
    }

    /// Report a failure in place of the message.
    pub fn show_error(&self, context: &str, detail: &str) {
        self.placeholder.set_icon_name(Some("dialog-warning-symbolic"));
        self.placeholder.set_title("Impossibile aprire il messaggio");
        self.placeholder.set_description(Some(&format!("{context}: {detail}")));
        self.stack.set_visible_child_name("empty");
    }

}

/// Ask where to put an attachment and write it there.
fn save_attachment(parent: Option<&gtk::Window>, attachment: &crate::model::Attachment) {
    if attachment.data.is_empty() {
        log::info!("{} has no stored bytes to save", attachment.filename);
        return;
    }

    let dialog = gtk::FileDialog::builder()
        .title(format!("Salva {}", attachment.filename))
        .initial_name(&attachment.filename)
        .modal(true)
        .build();

    let data = attachment.data.clone();
    let name = attachment.filename.clone();
    dialog.save(parent, gtk::gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else { return };
        let Some(path) = file.path() else { return };
        match std::fs::write(&path, &data) {
            Ok(()) => log::info!("saved {name} to {}", path.display()),
            Err(e) => log::warn!("could not save {name}: {e}"),
        }
    });
}

fn background_rgba(dark: bool) -> gtk::gdk::RGBA {
    if dark {
        gtk::gdk::RGBA::new(0.114, 0.114, 0.125, 1.0)
    } else {
        gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)
    }
}

impl Default for MessageView {
    fn default() -> Self {
        Self::new()
    }
}
