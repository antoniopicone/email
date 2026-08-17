//! Core mail types shared between the backend workers and the UI.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// How an account proves who it is to the IMAP server.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// Classic `LOGIN` with a password.
    Password(String),
    /// `AUTHENTICATE XOAUTH2` with a bearer token, used by Gmail.
    OAuth2(String),
}

impl Credentials {
    pub fn mechanism(&self) -> &'static str {
        match self {
            Credentials::Password(_) => "LOGIN",
            Credentials::OAuth2(_) => "XOAUTH2",
        }
    }
}

/// Where an account's settings came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountSource {
    /// Discovered through GNOME Online Accounts.
    Gnome,
    /// Entered by the user and stored in our own config file.
    Manual,
    /// Built-in sample data; never touches the network.
    Demo,
}

/// A mail account the client can talk to.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_user: String,
    pub use_starttls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub source: AccountSource,
    /// GNOME Online Accounts object path, for refreshing OAuth2 tokens.
    pub goa_path: Option<String>,
}

impl Account {

    /// The short label shown as the account's sidebar section header.
    pub fn short_label(&self) -> String {
        if self.display_name.trim().is_empty() {
            self.email.clone()
        } else {
            self.display_name.clone()
        }
    }
}

/// The well-known role of a mailbox, derived from IMAP SPECIAL-USE flags or
/// from the folder name when the server does not advertise them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MailboxKind {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
    Flagged,
    Other,
}

impl MailboxKind {
    /// Name of the bundled symbolic icon for this kind of mailbox.
    pub fn icon(&self) -> &'static str {
        match self {
            MailboxKind::Inbox => "mailview-inbox-symbolic",
            MailboxKind::Drafts => "mailview-drafts-symbolic",
            MailboxKind::Sent => "mailview-sent-symbolic",
            MailboxKind::Archive => "mailview-archive-symbolic",
            MailboxKind::Junk => "mailview-junk-symbolic",
            MailboxKind::Trash => "mailview-trash-symbolic",
            MailboxKind::Flagged => "mailview-flagged-symbolic",
            MailboxKind::Other => "mailview-folder-symbolic",
        }
    }

    /// Sort weight so the standard mailboxes appear in Apple Mail's order.
    pub fn order(&self) -> u8 {
        match self {
            MailboxKind::Inbox => 0,
            MailboxKind::Flagged => 1,
            MailboxKind::Drafts => 2,
            MailboxKind::Sent => 3,
            MailboxKind::Archive => 4,
            MailboxKind::Junk => 5,
            MailboxKind::Trash => 6,
            MailboxKind::Other => 7,
        }
    }

    /// Classify a mailbox from its SPECIAL-USE attributes and its name.
    pub fn classify(name: &str, attributes: &[String]) -> MailboxKind {
        for attr in attributes {
            let attr = attr.trim_start_matches('\\').to_ascii_lowercase();
            match attr.as_str() {
                "inbox" => return MailboxKind::Inbox,
                "drafts" => return MailboxKind::Drafts,
                "sent" => return MailboxKind::Sent,
                "archive" | "all" => return MailboxKind::Archive,
                "junk" => return MailboxKind::Junk,
                "trash" => return MailboxKind::Trash,
                "flagged" => return MailboxKind::Flagged,
                _ => {}
            }
        }

        // Fall back to name matching, covering the common localisations that
        // servers without SPECIAL-USE still use.
        let leaf = name.rsplit(['/', '.']).next().unwrap_or(name).to_ascii_lowercase();
        match leaf.as_str() {
            "inbox" | "posta in arrivo" | "boîte de réception" | "posteingang" => MailboxKind::Inbox,
            "drafts" | "bozze" | "brouillons" | "entwürfe" | "borradores" => MailboxKind::Drafts,
            "sent" | "sent mail" | "sent items" | "posta inviata" | "inviata" | "gesendet"
            | "envoyés" => MailboxKind::Sent,
            "archive" | "all mail" | "archivio" | "archiv" => MailboxKind::Archive,
            "junk" | "spam" | "indesiderata" | "posta indesiderata" => MailboxKind::Junk,
            "trash" | "deleted" | "deleted items" | "cestino" | "papierkorb" | "corbeille" => {
                MailboxKind::Trash
            }
            _ => MailboxKind::Other,
        }
    }
}

/// A folder on the server.
#[derive(Debug, Clone)]
pub struct Mailbox {
    pub account_id: String,
    /// Full IMAP path, e.g. `[Gmail]/Sent Mail`.
    pub path: String,
    /// Leaf name shown in the sidebar.
    pub name: String,
    pub kind: MailboxKind,
    pub unread: u32,
    pub total: u32,
    /// Nesting depth, used to indent nested folders in the sidebar.
    pub depth: usize,
}

/// A parsed `From:`/`To:` entry.
#[derive(Debug, Clone, Default)]
pub struct Mailaddr {
    pub name: String,
    pub address: String,
}

impl Mailaddr {
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self { name: name.into(), address: address.into() }
    }

    /// Best available human label: the display name, else the address.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.address
        } else {
            &self.name
        }
    }


    pub fn full(&self) -> String {
        if self.name.trim().is_empty() {
            self.address.clone()
        } else {
            format!("{} <{}>", self.name, self.address)
        }
    }
}

/// The lightweight per-message record backing the message list. Bodies are
/// fetched separately and only when a message is opened.
#[derive(Debug, Clone)]
pub struct MessageSummary {
    pub account_id: String,
    pub mailbox: String,
    pub uid: u32,
    pub from: Mailaddr,
    pub to: Vec<Mailaddr>,
    pub subject: String,
    /// First couple of lines of the text body, for the list preview.
    pub snippet: String,
    pub date: DateTime<Local>,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub has_attachments: bool,
}

impl MessageSummary {
    /// Apple Mail style relative date: time for today, weekday within the last
    /// week, otherwise a short date.
    pub fn date_label(&self) -> String {
        let now = Local::now();
        let age = now.signed_duration_since(self.date);
        if age.num_days() == 0 && now.date_naive() == self.date.date_naive() {
            self.date.format("%H:%M").to_string()
        } else if age.num_days() < 7 && age.num_days() >= 0 {
            self.date.format("%a").to_string()
        } else if self.date.format("%Y").to_string() == now.format("%Y").to_string() {
            self.date.format("%d/%m").to_string()
        } else {
            self.date.format("%d/%m/%y").to_string()
        }
    }

    pub fn subject_or_placeholder(&self) -> &str {
        if self.subject.trim().is_empty() {
            "(nessun oggetto)"
        } else {
            &self.subject
        }
    }
}

/// A file attached to a message.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub size: usize,
    pub data: Vec<u8>,
}

impl Attachment {
    pub fn human_size(&self) -> String {
        const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
        let mut size = self.size as f64;
        let mut unit = 0;
        while size >= 1024.0 && unit < UNITS.len() - 1 {
            size /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{} {}", self.size, UNITS[0])
        } else {
            format!("{size:.1} {}", UNITS[unit])
        }
    }
}

/// A fully fetched message, ready to display.
#[derive(Debug, Clone)]
pub struct Message {
    pub summary: MessageSummary,
    pub cc: Vec<Mailaddr>,
    /// Sanitised HTML body, when the message had one.
    pub html: Option<String>,
    /// Plain text body, always populated as a fallback.
    pub text: String,
    pub attachments: Vec<Attachment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn special_use_flags_win_over_names() {
        // Gmail localises folder names but still advertises \Sent.
        assert_eq!(
            MailboxKind::classify("[Gmail]/Posta inviata", &attrs(&["\\Sent"])),
            MailboxKind::Sent
        );
        // Gmail's "All Mail" is the closest thing it has to an archive.
        assert_eq!(MailboxKind::classify("[Gmail]/Tutti", &attrs(&["\\All"])), MailboxKind::Archive);
    }

    #[test]
    fn falls_back_to_localised_names() {
        assert_eq!(MailboxKind::classify("INBOX", &[]), MailboxKind::Inbox);
        assert_eq!(MailboxKind::classify("Posta inviata", &[]), MailboxKind::Sent);
        assert_eq!(MailboxKind::classify("Cestino", &[]), MailboxKind::Trash);
        assert_eq!(MailboxKind::classify("Progetti/2026", &[]), MailboxKind::Other);
    }

    #[test]
    fn classifies_by_leaf_not_by_parent() {
        // A folder named "Archivio" nested under anything is still an archive.
        assert_eq!(MailboxKind::classify("Lavoro/Archivio", &[]), MailboxKind::Archive);
    }

    #[test]
    fn standard_mailboxes_sort_in_apple_mail_order() {
        let mut kinds = vec![
            MailboxKind::Trash,
            MailboxKind::Inbox,
            MailboxKind::Sent,
            MailboxKind::Drafts,
        ];
        kinds.sort_by_key(|k| k.order());
        assert_eq!(
            kinds,
            vec![MailboxKind::Inbox, MailboxKind::Drafts, MailboxKind::Sent, MailboxKind::Trash]
        );
    }

    #[test]
    fn address_label_prefers_the_display_name() {
        let named = Mailaddr::new("Giulia Ferrari", "giulia@example.it");
        assert_eq!(named.label(), "Giulia Ferrari");
        assert_eq!(named.full(), "Giulia Ferrari <giulia@example.it>");

        let bare = Mailaddr::new("", "solo@example.it");
        assert_eq!(bare.label(), "solo@example.it");
        assert_eq!(bare.full(), "solo@example.it");
    }

    fn summary_at(date: DateTime<Local>) -> MessageSummary {
        MessageSummary {
            account_id: "a".into(),
            mailbox: "INBOX".into(),
            uid: 1,
            from: Mailaddr::new("X", "x@y.z"),
            to: vec![],
            subject: String::new(),
            snippet: String::new(),
            date,
            seen: false,
            flagged: false,
            answered: false,
            has_attachments: false,
        }
    }

    #[test]
    fn date_label_uses_the_time_for_today() {
        let label = summary_at(Local::now()).date_label();
        assert!(label.contains(':'), "expected a clock time, got {label}");
    }

    #[test]
    fn date_label_uses_a_date_for_old_messages() {
        let label = summary_at(Local::now() - chrono::Duration::days(400)).date_label();
        assert_eq!(label.matches('/').count(), 2, "expected d/m/y, got {label}");
    }

    #[test]
    fn empty_subject_gets_a_placeholder() {
        let mut summary = summary_at(Local::now());
        summary.subject = "   ".into();
        assert_eq!(summary.subject_or_placeholder(), "(nessun oggetto)");
    }

    #[test]
    fn attachment_size_is_human_readable() {
        let make = |size| Attachment {
            filename: "x".into(),
            mime_type: "application/pdf".into(),
            size,
            data: Vec::new(),
        };
        assert_eq!(make(512).human_size(), "512 B");
        assert_eq!(make(2048).human_size(), "2.0 KB");
        assert_eq!(make(1_572_864).human_size(), "1.5 MB");
    }
}
