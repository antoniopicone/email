//! Row widgets for the sidebar and the message list.

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::model::{Mailbox, MessageSummary};

/// The four quick actions reachable on a message row, either by dragging it
/// (Mail-style: short/long from either edge) or through the long-press
/// action sheet, for whoever is not dragging anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeAction {
    ToggleRead,
    Flag,
    Archive,
    Delete,
}

/// Past this many pixels a drag counts as a swipe at all.
const SWIPE_SHORT: f64 = 60.0;
/// Past this many pixels a swipe is the "long" gesture on its edge.
const SWIPE_LONG: f64 = 160.0;
/// How far the row is allowed to visibly shift while dragging — feedback
/// that something is happening, not a real reveal-and-drop panel.
const SWIPE_VISUAL_CAP: f64 = 96.0;

/// Wires the swipe gestures and the long-press action sheet onto a message
/// row. `on_action` fires with whichever of the four actions was picked,
/// by either input method.
fn attach_swipe_actions(
    row: &gtk::ListBoxRow,
    content: &gtk::Widget,
    on_action: Rc<dyn Fn(SwipeAction)>,
) {
    let drag = gtk::GestureDrag::new();
    {
        let content = content.clone();
        drag.connect_drag_update(move |_, offset_x, _| {
            let shift = offset_x.clamp(-SWIPE_VISUAL_CAP, SWIPE_VISUAL_CAP);
            content.set_margin_start(shift.max(0.0) as i32);
            content.set_margin_end((-shift.min(0.0)) as i32);
        });
    }
    {
        let content = content.clone();
        let on_action = on_action.clone();
        drag.connect_drag_end(move |_, offset_x, _| {
            content.set_margin_start(0);
            content.set_margin_end(0);

            let distance = offset_x.abs();
            if distance < SWIPE_SHORT {
                return;
            }
            let action = match (offset_x > 0.0, distance >= SWIPE_LONG) {
                (true, false) => SwipeAction::ToggleRead,
                (true, true) => SwipeAction::Flag,
                (false, false) => SwipeAction::Archive,
                (false, true) => SwipeAction::Delete,
            };
            on_action(action);
        });
    }
    row.add_controller(drag);

    let long_press = gtk::GestureLongPress::new();
    {
        let row = row.clone();
        long_press.connect_pressed(move |_, _, _| {
            show_action_sheet(&row, on_action.clone());
        });
    }
    row.add_controller(long_press);
}

/// The "modal window" alternative to swiping: press and hold a row to see
/// the same four actions as buttons.
fn show_action_sheet(row: &gtk::ListBoxRow, on_action: Rc<dyn Fn(SwipeAction)>) {
    let dialog = adw::AlertDialog::new(Some("Azioni sul messaggio"), None);
    dialog.add_response("toggle-read", "Segna come letto/da leggere");
    dialog.add_response("flag", "Contrassegna");
    dialog.add_response("archive", "Archivia");
    dialog.add_response("delete", "Elimina");
    dialog.add_response("cancel", "Annulla");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_, response| {
        let action = match response {
            "toggle-read" => SwipeAction::ToggleRead,
            "flag" => SwipeAction::Flag,
            "archive" => SwipeAction::Archive,
            "delete" => SwipeAction::Delete,
            _ => return,
        };
        on_action(action);
    });

    dialog.present(Some(row));
}

/// A heading that starts an account's group of mailboxes, with a chevron
/// that folds the group away. Not selectable like a folder row — clicking it
/// toggles `on_toggle` instead of navigating anywhere.
///
/// Quiet by design: a working account says nothing under its name, and only
/// a real connection problem earns a warning line — routine status text like
/// "Connesso" would just be noise repeated for every account, every time.
pub fn section_header(
    title: &str,
    has_error: bool,
    collapsed: bool,
    on_toggle: impl Fn() + 'static,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.add_css_class("section-header");

    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);

    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&label);

    if has_error {
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        status.add_css_class("account-status");
        status.add_css_class("account-status-error");

        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        status.append(&icon);

        let label = gtk::Label::new(Some("Errore di connessione"));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.append(&label);

        text.append(&status);
    }

    let chevron = gtk::Image::from_icon_name(if collapsed {
        "pan-end-symbolic"
    } else {
        "pan-down-symbolic"
    });
    chevron.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&text);
    content.append(&chevron);

    let button = gtk::Button::builder().child(&content).css_classes(["flat"]).build();
    button.connect_clicked(move |_| on_toggle());

    row.set_child(Some(&button));
    row
}

/// One mailbox in the sidebar: icon, name, and an unread badge.
pub fn mailbox_row(mailbox: &Mailbox) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("mailbox-row");

    let boxx = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    boxx.set_margin_start(4);

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
/// A sidebar row for a unified mailbox or an account inbox shortcut.
pub fn smart_row(title: &str, icon_name: &str, unread: u32) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("mailbox-row");

    let boxx = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    boxx.set_margin_start(4);

    boxx.append(&gtk::Image::from_icon_name(icon_name));

    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.add_css_class("mailbox-name");
    boxx.append(&label);

    if unread > 0 {
        let badge = gtk::Label::new(Some(&unread.to_string()));
        badge.add_css_class("mailbox-badge");
        badge.set_valign(gtk::Align::Center);
        boxx.append(&badge);
    }

    row.set_child(Some(&boxx));
    row
}

/// `account` labels which account a message came from, shown only in the
/// unified views where the list mixes several accounts together.
/// `on_swipe_action` fires when the user swipes the row or picks an action
/// from the long-press sheet.
pub fn message_row(
    message: &MessageSummary,
    account: Option<&str>,
    on_swipe_action: impl Fn(SwipeAction) + 'static,
) -> gtk::ListBoxRow {
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

    // Line 2 — subject, with the source account alongside it when the list
    // mixes accounts together.
    let subject = gtk::Label::new(Some(message.subject_or_placeholder()));
    subject.set_xalign(0.0);
    subject.set_hexpand(true);
    subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject.add_css_class("msg-subject");
    if !message.seen {
        subject.add_css_class("unread");
    }

    match account {
        Some(name) => {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.append(&subject);

            let tag = gtk::Label::new(Some(name));
            tag.set_ellipsize(gtk::pango::EllipsizeMode::End);
            tag.set_max_width_chars(16);
            tag.add_css_class("msg-account");
            tag.set_valign(gtk::Align::Center);
            line.append(&tag);

            column.append(&line);
        }
        None => column.append(&subject),
    }

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
    attach_swipe_actions(&row, outer.upcast_ref(), Rc::new(on_swipe_action));
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
