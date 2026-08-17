//! Row widgets for the sidebar and the message list.

use gtk4 as gtk;
use gtk::prelude::*;

use crate::model::{Mailbox, MessageSummary};

/// A non-selectable heading that starts an account's group of mailboxes.
pub fn section_header(title: &str, subtitle: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.add_css_class("section-header");

    let boxx = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    boxx.append(&label);

    if !subtitle.is_empty() {
        let status = gtk::Label::new(Some(subtitle));
        status.set_xalign(0.0);
        status.add_css_class("account-status");
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        boxx.append(&status);
    }

    row.set_child(Some(&boxx));
    row
}

/// One mailbox in the sidebar: icon, name, and an unread badge.
pub fn mailbox_row(mailbox: &Mailbox) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("mailbox-row");

    let boxx = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    // Nested folders line up under their parent.
    boxx.set_margin_start(4 + (mailbox.depth as i32 * 14));

    let icon = gtk::Image::from_icon_name(mailbox.kind.icon());
    boxx.append(&icon);

    let name = gtk::Label::new(Some(&mailbox.name));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("mailbox-name");
    boxx.append(&name);

    if mailbox.unread > 0 {
        let badge = gtk::Label::new(Some(&mailbox.unread.to_string()));
        badge.add_css_class("mailbox-badge");
        badge.set_valign(gtk::Align::Center);
        boxx.append(&badge);
    }

    row.set_tooltip_text(Some(&match (mailbox.total, mailbox.unread) {
        (0, _) => format!("{} — vuota", mailbox.path),
        (total, 0) => format!("{} — {total} messaggi", mailbox.path),
        (total, unread) => format!("{} — {total} messaggi, {unread} da leggere", mailbox.path),
    }));

    row.set_child(Some(&boxx));
    row
}

/// One message in the middle pane, laid out the way Apple Mail does it:
/// sender and date on the first line, subject on the second, and a dimmed
/// two-line preview underneath.
pub fn message_row(message: &MessageSummary) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();

    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // Left gutter: the unread dot, or empty space keeping the text aligned.
    let gutter = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gutter.set_size_request(12, -1);
    gutter.set_valign(gtk::Align::Start);
    gutter.set_margin_top(5);
    if !message.seen {
        let dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        dot.add_css_class("unread-dot");
        dot.set_halign(gtk::Align::Center);
        dot.set_valign(gtk::Align::Center);
        gutter.append(&dot);
    }
    outer.append(&gutter);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    column.set_hexpand(true);

    // Line 1 — sender, then flags and the date pushed to the right.
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let sender = gtk::Label::new(Some(message.from.label()));
    sender.set_xalign(0.0);
    sender.set_hexpand(true);
    sender.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sender.add_css_class("msg-sender");
    if !message.seen {
        sender.add_css_class("unread");
    }
    top.append(&sender);

    if message.answered {
        let icon = gtk::Image::from_icon_name("mail-replied-symbolic");
        icon.add_css_class("msg-attachment");
        icon.set_valign(gtk::Align::Center);
        top.append(&icon);
    }
    if message.has_attachments {
        let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
        icon.add_css_class("msg-attachment");
        icon.set_valign(gtk::Align::Center);
        top.append(&icon);
    }
    if message.flagged {
        let icon = gtk::Image::from_icon_name("starred-symbolic");
        icon.add_css_class("msg-flag");
        icon.set_valign(gtk::Align::Center);
        top.append(&icon);
    }

    let date = gtk::Label::new(Some(&message.date_label()));
    date.add_css_class("msg-date");
    date.set_valign(gtk::Align::Start);
    top.append(&date);

    column.append(&top);

    // Line 2 — subject.
    let subject = gtk::Label::new(Some(message.subject_or_placeholder()));
    subject.set_xalign(0.0);
    subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject.add_css_class("msg-subject");
    if !message.seen {
        subject.add_css_class("unread");
    }
    column.append(&subject);

    // Lines 3-4 — the preview.
    if !message.snippet.trim().is_empty() {
        let snippet = gtk::Label::new(Some(message.snippet.trim()));
        snippet.set_xalign(0.0);
        snippet.set_wrap(true);
        snippet.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        snippet.set_lines(2);
        snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
        snippet.set_max_width_chars(1);
        snippet.add_css_class("msg-snippet");
        column.append(&snippet);
    }

    outer.append(&column);
    row.set_child(Some(&outer));
    row
}

/// A clickable chip describing one attachment in the reading pane.
pub fn attachment_chip<F>(
    name: &str,
    size: &str,
    mime_type: &str,
    on_activate: F,
) -> gtk::Widget
where
    F: Fn() + 'static,
{
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
    content.append(&icon);

    let label = gtk::Label::new(Some(name));
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_max_width_chars(28);
    content.append(&label);

    let size_label = gtk::Label::new(Some(size));
    size_label.add_css_class("dim-label");
    content.append(&size_label);

    let button = gtk::Button::builder().child(&content).build();
    button.add_css_class("attachment-chip");
    button.add_css_class("flat");
    button.set_tooltip_text(Some(&format!("Salva “{name}” — {mime_type}, {size}")));
    button.connect_clicked(move |_| on_activate());

    button.upcast()
}
