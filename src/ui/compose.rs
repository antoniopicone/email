//! The compose window, used for new messages, replies and forwards.

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::backend::smtp::Outgoing;
use crate::model::{Account, Message};

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

/// Open the compose window. `on_send` receives the finished message.
pub fn open<F>(
    parent: &impl IsA<gtk::Window>,
    kind: ComposeKind,
    account: &Account,
    prefilled: Outgoing,
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

    let to_row = adw::EntryRow::builder().title("A").build();
    to_row.set_text(&prefilled.to);

    let cc_row = adw::EntryRow::builder().title("Cc").build();
    cc_row.set_text(&prefilled.cc);

    let subject_row = adw::EntryRow::builder().title("Oggetto").build();
    subject_row.set_text(&prefilled.subject);

    let fields = adw::PreferencesGroup::new();
    fields.add(&to_row);
    fields.add(&cc_row);
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

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(kind.title(), &account.email)));
    header.pack_start(&cancel_button);
    header.pack_end(&send_button);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&fields);
    content.append(&body_scroll);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

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
                subject: subject_row.text().to_string(),
                body: text,
                in_reply_to: in_reply_to.clone(),
            };

            if outgoing.to.trim().is_empty() {
                toast_overlay.add_toast(adw::Toast::new("Indica almeno un destinatario"));
                to_row.grab_focus();
                return;
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
