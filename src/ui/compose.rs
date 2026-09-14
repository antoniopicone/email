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

/// Work out the initial field contents for a compose action. `signature`, if
/// not empty, is inserted right where the user is expected to start typing —
/// above any quoted or forwarded content — the same spot for every kind, so
/// a fresh message gets just the signature and a reply gets signature, then
/// quote.
pub fn prefill(
    kind: ComposeKind,
    account: &Account,
    message: Option<&Message>,
    signature: &str,
) -> Outgoing {
    let Some(message) = message else {
        return Outgoing { body: signature_block(signature), ..Default::default() };
    };
    let summary = &message.summary;

    let mut outgoing = match kind {
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
    };
    outgoing.body = format!("{}{}", signature_block(signature), outgoing.body);
    outgoing
}

/// The signature, prefixed with the conventional `-- ` delimiter line, or
/// empty when there is no signature to insert.
fn signature_block(signature: &str) -> String {
    if signature.trim().is_empty() {
        String::new()
    } else {
        format!("-- \n{}\n", signature.trim_end())
    }
}

/// The text shown in the "Da" field and its picker popover for one account.
fn account_display(account: &Account) -> String {
    if account.display_name.trim().is_empty() || account.display_name == account.email {
        account.email.clone()
    } else {
        format!("{} <{}>", account.display_name, account.email)
    }
}

/// A flat, borderless field row: a fixed-width caption on the left (kept in
/// `captions` so every row's label lines up) and a hexpanding slot for the
/// field itself on the right.
fn field_row(caption_text: &str, captions: &gtk::SizeGroup, field: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);

    let caption = gtk::Label::new(Some(caption_text));
    caption.add_css_class("dim-label");
    caption.set_xalign(0.0);
    captions.add_widget(&caption);
    row.append(&caption);

    field.upcast_ref::<gtk::Widget>().set_hexpand(true);
    row.append(field);

    row
}

/// Live syntax validation plus a contact-suggestion popover on a recipient
/// field. `contacts` is shared across all three recipient fields and can
/// grow after the window is already open, once the (async, best-effort)
/// address book lookup returns.
fn setup_recipient_field(row: &gtk::Entry, contacts: &Rc<RefCell<Vec<Mailaddr>>>) {
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
    accounts: &[Account],
    default_account: &Account,
    prefilled: Outgoing,
    known_contacts: Vec<Mailaddr>,
    on_send: F,
) where
    F: Fn(Account, Outgoing) + 'static,
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

    let captions = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);

    // "Da": which configured account the message is sent from. Always a
    // picker, even with a single account, so it behaves the same regardless
    // of how many are configured.
    let selected_account: Rc<RefCell<Account>> = Rc::new(RefCell::new(default_account.clone()));

    let from_label = gtk::Label::new(Some(&account_display(default_account)));
    from_label.set_xalign(0.0);
    from_label.set_hexpand(true);
    from_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let from_chevron = gtk::Image::from_icon_name("pan-down-symbolic");
    from_chevron.add_css_class("dim-label");

    let from_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    from_content.append(&from_label);
    from_content.append(&from_chevron);

    let from_button = gtk::Button::builder()
        .child(&from_content)
        .css_classes(["flat", "compose-field"])
        .build();

    let from_popover = gtk::Popover::builder().autohide(true).has_arrow(false).build();
    from_popover.set_parent(&from_button);
    let from_list = gtk::ListBox::new();
    from_list.add_css_class("boxed-list");
    for account in accounts {
        let row = adw::ActionRow::builder()
            .title(account_display(account))
            .activatable(true)
            .build();
        from_list.append(&row);
    }
    from_popover.set_child(Some(&from_list));

    {
        let from_popover = from_popover.clone();
        from_button.connect_clicked(move |_| from_popover.popup());
    }
    {
        let from_label = from_label.clone();
        let from_popover = from_popover.clone();
        let selected_account = selected_account.clone();
        let accounts: Vec<Account> = accounts.to_vec();
        from_list.connect_row_activated(move |_, activated| {
            if let Some(account) = accounts.get(activated.index() as usize) {
                from_label.set_text(&account_display(account));
                *selected_account.borrow_mut() = account.clone();
            }
            from_popover.popdown();
        });
    }

    let from_row = field_row("Da", &captions, &from_button);

    let to_entry = gtk::Entry::builder().css_classes(["compose-field"]).build();
    to_entry.set_text(&prefilled.to);
    setup_recipient_field(&to_entry, &contacts);

    // Small closed-by-default disclosure toggles next to the "A" field, so
    // Cc and Ccn stay out of the way until asked for.
    let cc_toggle = gtk::ToggleButton::builder()
        .label("Cc")
        .valign(gtk::Align::Center)
        .active(!prefilled.cc.trim().is_empty())
        .css_classes(["flat"])
        .build();
    let bcc_toggle = gtk::ToggleButton::builder()
        .label("Ccn")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    let to_row = field_row("A", &captions, &to_entry);
    to_row.append(&cc_toggle);
    to_row.append(&bcc_toggle);

    let cc_entry = gtk::Entry::builder().css_classes(["compose-field"]).build();
    cc_entry.set_text(&prefilled.cc);
    setup_recipient_field(&cc_entry, &contacts);
    let cc_row = field_row("Cc", &captions, &cc_entry);
    cc_row.set_visible(!prefilled.cc.trim().is_empty());
    {
        let cc_row = cc_row.clone();
        cc_toggle.connect_toggled(move |button| cc_row.set_visible(button.is_active()));
    }

    let bcc_entry = gtk::Entry::builder().css_classes(["compose-field"]).build();
    setup_recipient_field(&bcc_entry, &contacts);
    let bcc_row = field_row("Ccn", &captions, &bcc_entry);
    bcc_row.set_visible(false);
    {
        let bcc_row = bcc_row.clone();
        bcc_toggle.connect_toggled(move |button| bcc_row.set_visible(button.is_active()));
    }

    let subject_entry = gtk::Entry::builder().css_classes(["compose-field"]).build();
    subject_entry.set_text(&prefilled.subject);
    let subject_row = field_row("Oggetto", &captions, &subject_entry);

    let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
    fields.append(&from_row);
    fields.append(&to_row);
    fields.append(&cc_row);
    fields.append(&bcc_row);
    fields.append(&subject_row);
    fields.set_margin_top(14);
    fields.set_margin_bottom(10);
    fields.set_margin_start(14);
    fields.set_margin_end(14);

    let body = gtk::TextView::new();
    body.set_wrap_mode(gtk::WrapMode::WordChar);
    body.set_top_margin(6);
    body.set_bottom_margin(10);
    body.set_left_margin(14);
    body.set_right_margin(14);
    body.set_monospace(false);
    body.buffer().set_text(&prefilled.body);

    let body_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .build();

    let send_button = gtk::Button::builder().label("Invia").build();
    send_button.add_css_class("suggested-action");

    let cancel_button = gtk::Button::builder().label("Annulla").build();

    let window_title = adw::WindowTitle::new(kind.title(), &default_account.email);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&window_title));

    // The window title follows the subject once the user leaves the field,
    // the way most mail clients label a compose window — but only once
    // there is something to show, and only after the field is a considered
    // choice (focus-out), not on every keystroke.
    {
        let window = window.clone();
        let window_title = window_title.clone();
        let subject_entry_handle = subject_entry.clone();
        let default_title = kind.title().to_string();
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            let subject = subject_entry_handle.text();
            let trimmed = subject.trim();
            let title = if trimmed.is_empty() { default_title.as_str() } else { trimmed };
            window_title.set_title(title);
            window.set_title(Some(title));
        });
        subject_entry.add_controller(focus);
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
        let to_entry = to_entry.clone();
        let cc_entry = cc_entry.clone();
        let bcc_entry = bcc_entry.clone();
        let subject_entry = subject_entry.clone();
        let body = body.clone();
        let in_reply_to = prefilled.in_reply_to.clone();
        let toast_overlay = toast_overlay.clone();
        let selected_account = selected_account.clone();

        send_button.connect_clicked(move |_| {
            let buffer = body.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();

            let outgoing = Outgoing {
                to: to_entry.text().to_string(),
                cc: cc_entry.text().to_string(),
                bcc: bcc_entry.text().to_string(),
                subject: subject_entry.text().to_string(),
                body: text,
                in_reply_to: in_reply_to.clone(),
            };

            if outgoing.to.trim().is_empty() {
                toast_overlay.add_toast(adw::Toast::new("Indica almeno un destinatario"));
                to_entry.grab_focus();
                return;
            }

            for (entry, field, label) in [
                (&to_entry, &outgoing.to, "A"),
                (&cc_entry, &outgoing.cc, "Cc"),
                (&bcc_entry, &outgoing.bcc, "Ccn"),
            ] {
                if !field.trim().is_empty() && parse_recipients(field).is_err() {
                    toast_overlay.add_toast(adw::Toast::new(&format!(
                        "Controlla gli indirizzi nel campo {label}"
                    )));
                    entry.grab_focus();
                    return;
                }
            }

            on_send(selected_account.borrow().clone(), outgoing);
            window.close();
        });
    }

    window.present();

    // Put the cursor where the user is most likely to start typing.
    if prefilled.to.is_empty() {
        to_entry.grab_focus();
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
