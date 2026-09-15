//! Persisted settings for accounts the user added by hand.
//!
//! Accounts that come from GNOME Online Accounts are *not* stored here: they
//! are re-read from GOA on every launch so that changes made in Settings are
//! picked up automatically. Only manually configured servers land in the
//! config file, and their passwords go to the Secret Service, never to disk.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::i18n::t;
use crate::model::{Account, AccountSource};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualAccount {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    #[serde(default)]
    pub imap_user: String,
    #[serde(default)]
    pub use_starttls: bool,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_user: String,
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    587
}

impl ManualAccount {
    pub fn to_account(&self) -> Account {
        let user = if self.imap_user.is_empty() { &self.email } else { &self.imap_user };
        Account {
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            imap_host: self.imap_host.clone(),
            imap_port: self.imap_port,
            imap_user: user.clone(),
            use_starttls: self.use_starttls,
            smtp_host: self.smtp_host.clone(),
            smtp_port: self.smtp_port,
            smtp_user: if self.smtp_user.is_empty() {
                user.clone()
            } else {
                self.smtp_user.clone()
            },
            source: AccountSource::Manual,
            goa_path: None,
        }
    }
}

/// What the user chose in the appearance menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreference {
    /// Follow the desktop's own light/dark setting.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "light" | "day" => ThemePreference::Light,
            "dark" | "night" => ThemePreference::Dark,
            _ => ThemePreference::System,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        }
    }
}

/// How HTML messages adapt to the app's light/dark style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MessageAppearance {
    /// Force our readable text colour, leave the message's own background.
    #[default]
    AdaptText,
    /// Force the page background to match the app, leave text colours as
    /// the sender set them.
    AdaptBackground,
    /// Render exactly as the sender authored it, ignoring app theme.
    AcceptSenderFormat,
}

impl MessageAppearance {
    pub const ALL: [MessageAppearance; 3] =
        [Self::AdaptText, Self::AdaptBackground, Self::AcceptSenderFormat];

    pub fn label(&self) -> &'static str {
        match self {
            MessageAppearance::AdaptText => t("Adatta il testo"),
            MessageAppearance::AdaptBackground => t("Adatta lo sfondo"),
            MessageAppearance::AcceptSenderFormat => t("Mantieni il formato del mittente"),
        }
    }

    pub fn subtitle(&self) -> &'static str {
        match self {
            MessageAppearance::AdaptText => {
                t("Il testo resta leggibile, lo sfondo è quello del messaggio")
            }
            MessageAppearance::AdaptBackground => {
                t("Lo sfondo segue il tema, i colori del testo restano quelli del mittente")
            }
            MessageAppearance::AcceptSenderFormat => {
                t("Il messaggio viene mostrato esattamente come inviato")
            }
        }
    }
}

/// How long a sent message waits in the outbox before it actually goes out,
/// giving the user a window to undo the send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SendDelay {
    #[default]
    Off,
    Seconds5,
    Seconds10,
    Seconds30,
    Minutes1,
    Minutes2,
    Minutes5,
}

impl SendDelay {
    pub const ALL: [SendDelay; 7] = [
        Self::Off,
        Self::Seconds5,
        Self::Seconds10,
        Self::Seconds30,
        Self::Minutes1,
        Self::Minutes2,
        Self::Minutes5,
    ];

    pub fn seconds(&self) -> u32 {
        match self {
            SendDelay::Off => 0,
            SendDelay::Seconds5 => 5,
            SendDelay::Seconds10 => 10,
            SendDelay::Seconds30 => 30,
            SendDelay::Minutes1 => 60,
            SendDelay::Minutes2 => 120,
            SendDelay::Minutes5 => 300,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SendDelay::Off => t("Disattivato (invio immediato)"),
            SendDelay::Seconds5 => t("5 secondi"),
            SendDelay::Seconds10 => t("10 secondi"),
            SendDelay::Seconds30 => t("30 secondi"),
            SendDelay::Minutes1 => t("1 minuto"),
            SendDelay::Minutes2 => t("2 minuti"),
            SendDelay::Minutes5 => t("5 minuti"),
        }
    }
}

/// How far back to keep message bodies cached on disk for offline reading,
/// per account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OfflineWindow {
    Nothing,
    LastWeek,
    #[default]
    LastMonth,
    LastYear,
    Everything,
}

impl OfflineWindow {
    pub const ALL: [OfflineWindow; 5] =
        [Self::Nothing, Self::LastWeek, Self::LastMonth, Self::LastYear, Self::Everything];

    pub fn label(&self) -> &'static str {
        match self {
            OfflineWindow::Nothing => t("Nessuno"),
            OfflineWindow::LastWeek => t("Ultima settimana"),
            OfflineWindow::LastMonth => t("Ultimo mese"),
            OfflineWindow::LastYear => t("Ultimo anno"),
            OfflineWindow::Everything => t("Tutti i messaggi"),
        }
    }

    /// Age past which a cached body is pruned; `None` means never.
    pub fn max_age_days(&self) -> Option<i64> {
        match self {
            OfflineWindow::Nothing => Some(0),
            OfflineWindow::LastWeek => Some(7),
            OfflineWindow::LastMonth => Some(30),
            OfflineWindow::LastYear => Some(365),
            OfflineWindow::Everything => None,
        }
    }

    /// The point in time before which a cached body should be pruned;
    /// `None` means nothing is ever pruned.
    pub fn cutoff(&self) -> Option<chrono::DateTime<chrono::Local>> {
        self.max_age_days().map(|days| chrono::Local::now() - chrono::Duration::days(days))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub accounts: Vec<ManualAccount>,
    #[serde(default)]
    pub theme: ThemePreference,
    /// Whether to pull accounts out of GNOME Online Accounts on startup.
    #[serde(default = "default_true")]
    pub use_gnome_online_accounts: bool,
    /// How many messages to fetch per mailbox page.
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    /// Width of the mailbox sidebar in pixels, dragged to size by the user.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: i32,
    /// Width of the message list pane in pixels, dragged to size by the user.
    #[serde(default = "default_message_list_width")]
    pub message_list_width: i32,
    /// Whether HTML messages are allowed to load remote images/resources.
    #[serde(default)]
    pub load_remote_content: bool,
    /// How HTML messages adapt to the app's light/dark style.
    #[serde(default)]
    pub message_appearance: MessageAppearance,
    /// How long a sent message sits in the outbox before actually going out.
    #[serde(default)]
    pub send_delay: SendDelay,
    /// Per-account signature, appended to new/reply/forward bodies. Keyed by
    /// account id.
    #[serde(default)]
    pub signatures: std::collections::BTreeMap<String, String>,
    /// Per-account offline retention window for cached message bodies.
    #[serde(default)]
    pub offline_windows: std::collections::BTreeMap<String, OfflineWindow>,
    /// Account ids whose folder list is currently collapsed in the sidebar.
    #[serde(default)]
    pub collapsed_accounts: std::collections::HashSet<String>,
    /// Custom order of the sidebar's top shortcut rows (unified inbox, each
    /// account's inbox, flagged, unread), as stable ids set by drag & drop.
    /// Empty means "use the default order".
    #[serde(default)]
    pub sidebar_order: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_page_size() -> u32 {
    100
}

fn default_sidebar_width() -> i32 {
    260
}

fn default_message_list_width() -> i32 {
    380
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mailview")
            .join("config.toml")
    }

    /// Load the config, falling back to defaults when it is missing or broken.
    /// A corrupt config should never stop the app from starting.
    pub fn load() -> Self {
        let path = Self::path();
        match Self::load_from(&path) {
            Ok(config) => config,
            Err(e) => {
                if path.exists() {
                    log::warn!("could not read {}: {e}; using defaults", path.display());
                }
                Config {
                    use_gnome_online_accounts: true,
                    page_size: default_page_size(),
                    sidebar_width: default_sidebar_width(),
                    message_list_width: default_message_list_width(),
                    ..Default::default()
                }
            }
        }
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// The signature configured for one account, if any.
    pub fn signature(&self, account_id: &str) -> String {
        self.signatures.get(account_id).cloned().unwrap_or_default()
    }

    /// The offline retention window configured for one account.
    pub fn offline_window(&self, account_id: &str) -> OfflineWindow {
        self.offline_windows.get(account_id).copied().unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialising the config")?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
