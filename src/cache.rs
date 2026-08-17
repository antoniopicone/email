//! On-disk cache for downloaded messages.
//!
//! Message lists and opened messages are written to `$XDG_CACHE_HOME/mailview`
//! keyed by account and mailbox, so a folder shows something instantly while
//! the network request is still in flight, and a message that has already
//! been opened once does not need a second round trip to the server.
//!
//! Writes happen on a throwaway thread so a slow disk never stalls the GTK
//! main loop; reads are synchronous because the UI needs the answer right
//! away and the payloads involved are small.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::model::{Attachment, Mailaddr, Message, MessageSummary};

#[derive(Serialize, Deserialize)]
struct CachedAddr {
    name: String,
    address: String,
}

impl From<&Mailaddr> for CachedAddr {
    fn from(value: &Mailaddr) -> Self {
        Self { name: value.name.clone(), address: value.address.clone() }
    }
}

impl From<CachedAddr> for Mailaddr {
    fn from(value: CachedAddr) -> Self {
        Mailaddr { name: value.name, address: value.address }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedSummary {
    account_id: String,
    mailbox: String,
    uid: u32,
    from: CachedAddr,
    to: Vec<CachedAddr>,
    subject: String,
    snippet: String,
    /// Unix timestamp; sidesteps chrono's own serde format entirely.
    date: i64,
    seen: bool,
    flagged: bool,
    answered: bool,
    has_attachments: bool,
}

impl From<&MessageSummary> for CachedSummary {
    fn from(value: &MessageSummary) -> Self {
        Self {
            account_id: value.account_id.clone(),
            mailbox: value.mailbox.clone(),
            uid: value.uid,
            from: (&value.from).into(),
            to: value.to.iter().map(Into::into).collect(),
            subject: value.subject.clone(),
            snippet: value.snippet.clone(),
            date: value.date.timestamp(),
            seen: value.seen,
            flagged: value.flagged,
            answered: value.answered,
            has_attachments: value.has_attachments,
        }
    }
}

impl From<CachedSummary> for MessageSummary {
    fn from(value: CachedSummary) -> Self {
        MessageSummary {
            account_id: value.account_id,
            mailbox: value.mailbox,
            uid: value.uid,
            from: value.from.into(),
            to: value.to.into_iter().map(Into::into).collect(),
            subject: value.subject,
            snippet: value.snippet,
            date: Local.timestamp_opt(value.date, 0).single().unwrap_or_else(Local::now),
            seen: value.seen,
            flagged: value.flagged,
            answered: value.answered,
            has_attachments: value.has_attachments,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedAttachment {
    filename: String,
    mime_type: String,
    size: usize,
    #[serde(with = "base64_bytes")]
    data: Vec<u8>,
}

impl From<&Attachment> for CachedAttachment {
    fn from(value: &Attachment) -> Self {
        Self {
            filename: value.filename.clone(),
            mime_type: value.mime_type.clone(),
            size: value.size,
            data: value.data.clone(),
        }
    }
}

impl From<CachedAttachment> for Attachment {
    fn from(value: CachedAttachment) -> Self {
        Attachment { filename: value.filename, mime_type: value.mime_type, size: value.size, data: value.data }
    }
}

#[derive(Serialize, Deserialize)]
struct CachedMessage {
    summary: CachedSummary,
    cc: Vec<CachedAddr>,
    html: Option<String>,
    text: String,
    attachments: Vec<CachedAttachment>,
}

impl From<&Message> for CachedMessage {
    fn from(value: &Message) -> Self {
        Self {
            summary: (&value.summary).into(),
            cc: value.cc.iter().map(Into::into).collect(),
            html: value.html.clone(),
            text: value.text.clone(),
            attachments: value.attachments.iter().map(Into::into).collect(),
        }
    }
}

impl From<CachedMessage> for Message {
    fn from(value: CachedMessage) -> Self {
        Message {
            summary: value.summary.into(),
            cc: value.cc.into_iter().map(Into::into).collect(),
            html: value.html,
            text: value.text,
            attachments: value.attachments.into_iter().map(Into::into).collect(),
        }
    }
}

mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        STANDARD.encode(bytes).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(d)?;
        STANDARD.decode(encoded.as_bytes()).map_err(serde::de::Error::custom)
    }
}

// ------------------------------------------------------------------ paths

fn root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("mailview")
}

/// Filesystem-safe stand-in for an account id or IMAP mailbox path, which can
/// contain slashes, brackets and other characters folders disallow.
fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

fn mailbox_dir(account_id: &str, mailbox: &str) -> PathBuf {
    root().join(sanitize(account_id)).join(sanitize(mailbox))
}

fn summaries_path(account_id: &str, mailbox: &str) -> PathBuf {
    mailbox_dir(account_id, mailbox).join("summaries.json")
}

fn message_path(account_id: &str, mailbox: &str, uid: u32) -> PathBuf {
    mailbox_dir(account_id, mailbox).join(format!("msg-{uid}.json"))
}

// -------------------------------------------------------------------- I/O

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let tmp = path.with_extension(format!("tmp{nonce}"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn spawn_write<T: Serialize + Send + 'static>(path: PathBuf, value: T) {
    std::thread::spawn(move || match serde_json::to_vec(&value) {
        Ok(bytes) => {
            if let Err(e) = atomic_write(&path, &bytes) {
                log::warn!("could not write cache file {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("could not serialise data for the cache: {e}"),
    });
}

// ------------------------------------------------------------------ public

/// The most recently cached page of a mailbox's message list, if any.
pub fn load_summaries(account_id: &str, mailbox: &str) -> Option<Vec<MessageSummary>> {
    let bytes = fs::read(summaries_path(account_id, mailbox)).ok()?;
    let cached: Vec<CachedSummary> = serde_json::from_slice(&bytes).ok()?;
    Some(cached.into_iter().map(Into::into).collect())
}

/// Remember a mailbox's freshly loaded first page for the next launch.
pub fn store_summaries(account_id: &str, mailbox: &str, summaries: &[MessageSummary]) {
    let cached: Vec<CachedSummary> = summaries.iter().map(Into::into).collect();
    spawn_write(summaries_path(account_id, mailbox), cached);
}

/// A previously opened message, if it is still on disk.
pub fn load_message(account_id: &str, mailbox: &str, uid: u32) -> Option<Message> {
    let bytes = fs::read(message_path(account_id, mailbox, uid)).ok()?;
    let cached: CachedMessage = serde_json::from_slice(&bytes).ok()?;
    Some(cached.into())
}

/// Save a fully fetched message so opening it again needs no network trip.
pub fn store_message(account_id: &str, mailbox: &str, uid: u32, message: &Message) {
    let cached: CachedMessage = message.into();
    spawn_write(message_path(account_id, mailbox, uid), cached);
}
