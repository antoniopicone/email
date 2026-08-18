//! The compose window, used for new messages, replies and forwards.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::backend::smtp::{parse_recipients, Outgoing};
use crate::model::{Account, Mailaddr, Message};

/// What the user asked for, which decides how the fields are prefilled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    Reply,
    ReplyAll,
    Forward,
}

impl ComposeKind {
    fn title(&self) -> &'static str {
        match self {
            ComposeKind::New => "Nuovo messaggio",
            ComposeKind::Reply => "Rispondi",
            ComposeKind::ReplyAll => "Rispondi a tutti",
            ComposeKind::Forward => "Inoltra",
        }
    }
}

/// Prefix a subject once: `Re: Re: x` helps nobody.
fn prefixed_subject(prefix: &str, subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.to_lowercase().starts_with(&prefix.to_lowercase()) {
        trimmed.to_string()
    } else {
        format!("{prefix} {trimmed}")
    }
}

/// Quote the original message the way mail clients have done for decades.
fn quote(message: &Message) -> String {
    let when = message.summary.date.format("%d/%m/%Y alle %H:%M");
    let who = message.summary.from.full();
    let quoted: String =
        message.text.lines().map(|line| format!("> {line}\n")).collect();
    format!("\n\nIl {when}, {who} ha scritto:\n{quoted}")
}

fn forwarded(message: &Message) -> String {
    let summary = &message.summary;
    format!(
        "\n\n---------- Messaggio inoltrato ----------\n\
         Da: {}\n\
         Data: {}\n\
         Oggetto: {}\n\
         A: {}\n\n{}",
        summary.from.full(),
        summary.date.format("%d/%m/%Y %H:%M"),
        summary.subject,
        summary.to.iter().map(|a| a.full()).collect::<Vec<_>>().join(", "),
        message.text
    )
}

/// Work out the initial field contents for a compose action.
pub fn prefill(kind: ComposeKind, account: &Account, message: Option<&Message>) -> Outgoing {
    let Some(message) = message else { return Outgoing::default() };
    let summary = &message.summary;

    match kind {
        ComposeKind::New => Outgoing::default(),

        ComposeKind::Reply => Outgoing {
            to: summary.from.full(),
            subject: prefixed_subject("Re:", &summary.subject),
            body: quote(message),
            ..Default::default()
        },

        ComposeKind::ReplyAll => {
            // Everyone on the original except ourselves.
            let mut others: Vec<String> = summary
                .to
                .iter()
                .chain(message.cc.iter())
                .filter(|a| !a.address.eq_ignore_ascii_case(&account.email))
                .map(|a| a.full())
                .collect();
            others.dedup();

            Outgoing {
                to: summary.from.full(),
                cc: others.join(", "),
                subject: prefixed_subject("Re:", &summary.subject),
                body: quote(message),
                ..Default::default()
            }
        }

        ComposeKind::Forward => Outgoing {
            subject: prefixed_subject("Fwd:", &summary.subject),
            body: forwarded(message),
            ..Default::default()
        },
    }
}

/// Live syntax validation plus a contact-suggestion popover on a recipient
/// field. `contacts` is shared across all three recipient fields and can
/// grow after the window is already open, once the (async, best-effort)
/// address book lookup returns.
fn setup_recipient_field(row: &adw::EntryRow, contacts: &Rc<RefCell<Vec<Mailaddr>>>) {
    // Validation: an empty field is fine (nothing to send yet), anything
    // else must parse the same way the SMTP send path will parse it.
    {
        let row = row.clone();
        row.connect_changed(move |row| {
            let valid = row.text().trim().is_empty() || parse_recipients(&row.text()).is_ok();
            if valid {
                row.remove_css_class("error");
            } else {
                row.add_css_class("error");
            }
        });
    }

    // Suggestions, matched against whatever is typed after the last comma.
    let popover = gtk::Popover::builder().autohide(false).has_arrow(false).build();
    popover.set_parent(row);
    let suggestion_list = gtk::ListBox::new();
    suggestion_list.add_css_class("boxed-list");
    popover.set_child(Some(&suggestion_list));

    {
        let row = row.clone();
        let popover = popover.clone();
        let suggestion_list = suggestion_list.clone();
        let contacts = contacts.clone();
        row.connect_changed(move |row| {
            while let Some(child) = suggestion_list.first_child() {
                suggestion_list.remove(&child);
            }

            let text = row.text();
            let fragment = text.rsplit(',').next().unwrap_or("").trim().to_lowercase();
            if fragment.is_empty() {
                popover.popdown();
                return;
            }

            let matches: Vec<Mailaddr> = contacts
                .borrow()
                .iter()
                .filter(|c| {
                    c.address.to_lowercase().contains(&fragment)
                        || c.name.to_lowercase().contains(&fragment)
                })
                .take(6)
                .cloned()
                .collect();

            if matches.is_empty() {
                popover.popdown();
                return;
            }

            for contact in &matches {
                let title = if contact.name.trim().is_empty() {
                    contact.address.clone()
                } else {
                    contact.name.clone()
                };
                let suggestion = adw::ActionRow::builder()
                    .title(title)
                    .subtitle(&contact.address)
                    .activatable(true)
                    .build();
                suggestion_list.append(&suggestion);
            }
            popover.popup();
        });
    }

    {
        let row = row.clone();
        let popover = popover.clone();
        suggestion_list.connect_row_activated(move |_, activated| {
            let Some(picked) = activated.downcast_ref::<adw::ActionRow>() else { return };
            let address = picked.subtitle().map(|s| s.to_string()).unwrap_or_default();

            let current = row.text().to_string();
            let prefix = match current.rfind(',') {
                Some(idx) => format!("{} ", &current[..=idx]),
                None => String::new(),
            };
            row.set_text(&format!("{prefix}{address}, "));
            row.set_position(-1);
            popover.popdown();
            row.grab_focus();
        });
    }
}

/// Open the compose window. `on_send` receives the finished message.
/// `known_contacts` seeds the suggestion popovers with addresses already
/// seen in loaded mail; GNOME's local address book, when reachable, is
/// merged in shortly after the window opens.
pub fn open<F>(
    parent: &impl IsA<gtk::Window>,
    kind: ComposeKind,
    account: &Account,
    prefilled: Outgoing,
    known_contacts: Vec<Mailaddr>,
    on_send: F,
) where
    F: Fn(Outgoing) + 'static,
{
    let window = adw::Window::builder()
        .transient_for(parent)
        .modal(false)
        .title(kind.title())
        .default_width(720)
        .default_height(560)
        .build();

    let contacts = Rc::new(RefCell::new(known_contacts));
    {
        let contacts = contacts.clone();
        glib::spawn_future_local(async move {
            let mut fetched = crate::contacts::from_local_address_book().await;
            if fetched.is_empty() {
                return;
            }
            let mut current = contacts.borrow_mut();
            let known: std::collections::HashSet<String> =
                current.iter().map(|c| c.address.to_lowercase()).collect();
            fetched.retain(|c| !known.contains(&c.address.to_lowercase()));
            current.extend(fetched);
        });
    }

    let to_row = adw::EntryRow::builder().title("A").build();
    to_row.set_text(&prefilled.to);
    setup_recipient_field(&to_row, &contacts);

    let cc_row = adw::EntryRow::builder().title("Cc").build();
    cc_row.set_text(&prefilled.cc);
    cc_row.set_visible(!prefilled.cc.trim().is_empty());
    setup_recipient_field(&cc_row, &contacts);

    let bcc_row = adw::EntryRow::builder().title("Ccn").build();
    bcc_row.set_visible(false);
    setup_recipient_field(&bcc_row, &contacts);

    // Small closed-by-default disclosure toggles on the "A" row, so Cc and
    // Ccn stay out of the way until asked for.
    let cc_toggle = gtk::ToggleButton::builder()
        .label("Cc")
        .valign(gtk::Align::Center)
        .active(!prefilled.cc.trim().is_empty())
        .css_classes(["flat"])
        .build();
    {
        let cc_row = cc_row.clone();
        cc_toggle.connect_toggled(move |button| cc_row.set_visible(button.is_active()));
    }
    let bcc_toggle = gtk::ToggleButton::builder()
        .label("Ccn")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    {
        let bcc_row = bcc_row.clone();
        bcc_toggle.connect_toggled(move |button| bcc_row.set_visible(button.is_active()));
    }
    to_row.add_suffix(&cc_toggle);
    to_row.add_suffix(&bcc_toggle);

    let subject_row = adw::EntryRow::builder().title("Oggetto").build();
    subject_row.set_text(&prefilled.subject);

    let fields = adw::PreferencesGroup::new();
    fields.add(&to_row);
    fields.add(&cc_row);
    fields.add(&bcc_row);
    fields.add(&subject_row);
    fields.set_margin_top(12);
    fields.set_margin_bottom(6);
    fields.set_margin_start(12);
    fields.set_margin_end(12);

    let body = gtk::TextView::new();
    body.set_wrap_mode(gtk::WrapMode::WordChar);
    body.set_top_margin(10);
    body.set_bottom_margin(10);
    body.set_left_margin(12);
    body.set_right_margin(12);
    body.set_monospace(false);
    body.buffer().set_text(&prefilled.body);

    let body_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(12)
        .build();
    body_scroll.add_css_class("card");

    let send_button = gtk::Button::builder().label("Invia").build();
    send_button.add_css_class("suggested-action");

    let cancel_button = gtk::Button::builder().label("Annulla").build();

    let window_title = adw::WindowTitle::new(kind.title(), &account.email);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&window_title));

    // The window title follows the subject once the user leaves the field,
    // the way most mail clients label a compose window — but only once
    // there is something to show, and only after the field is a considered
    // choice (focus-out), not on every keystroke.
    {
        let window = window.clone();
        let window_title = window_title.clone();
        let subject_row_handle = subject_row.clone();
        let default_title = kind.title().to_string();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            let subject = subject_row_handle.text();
            let trimmed = subject.trim();
            let title = if trimmed.is_empty() { default_title.as_str() } else { trimmed };
            window_title.set_title(title);
            window.set_title(Some(title));
        });
        subject_row.add_controller(focus);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&fields);
    content.append(&body_scroll);

    let action_bar = gtk::ActionBar::new();
    action_bar.pack_start(&cancel_button);
    action_bar.pack_end(&send_button);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    toolbar.add_bottom_bar(&action_bar);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&toolbar));
    window.set_content(Some(&toast_overlay));

    {
        let window = window.clone();
        cancel_button.connect_clicked(move |_| window.close());
    }

    {
        let window = window.clone();
        let to_row = to_row.clone();
        let cc_row = cc_row.clone();
        let bcc_row = bcc_row.clone();
        let subject_row = subject_row.clone();
        let body = body.clone();
        let in_reply_to = prefilled.in_reply_to.clone();
        let toast_overlay = toast_overlay.clone();

        send_button.connect_clicked(move |_| {
            let buffer = body.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();

            let outgoing = Outgoing {
                to: to_row.text().to_string(),
                cc: cc_row.text().to_string(),
                bcc: bcc_row.text().to_string(),
                subject: subject_row.text().to_string(),
                body: text,
                in_reply_to: in_reply_to.clone(),
            };

            if outgoing.to.trim().is_empty() {
                toast_overlay.add_toast(adw::Toast::new("Indica almeno un destinatario"));
                to_row.grab_focus();
                return;
            }

            for (row, field, label) in
                [(&to_row, &outgoing.to, "A"), (&cc_row, &outgoing.cc, "Cc"), (&bcc_row, &outgoing.bcc, "Ccn")]
            {
                if !field.trim().is_empty() && parse_recipients(field).is_err() {
                    toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Controlla gli indirizzi nel campo {label}"
                    )));
                    row.grab_focus();
                    return;
                }
            }

            on_send(outgoing);
            window.close();
        });
    }

    window.present();

    // Put the cursor where the user is most likely to start typing.
    if prefilled.to.is_empty() {
        to_row.grab_focus();
    } else {
        body.grab_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_prefix_is_not_stacked() {
        assert_eq!(prefixed_subject("Re:", "Preventivo"), "Re: Preventivo");
        assert_eq!(prefixed_subject("Re:", "Re: Preventivo"), "Re: Preventivo");
        // Servers and clients disagree on case; treat them the same.
        assert_eq!(prefixed_subject("Re:", "RE: Preventivo"), "RE: Preventivo");
        assert_eq!(prefixed_subject("Fwd:", "Contratto"), "Fwd: Contratto");
    }

    #[test]
    fn subject_prefix_trims_whitespace() {
        assert_eq!(prefixed_subject("Re:", "  Ciao  "), "Re: Ciao");
    }
}
