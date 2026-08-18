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
