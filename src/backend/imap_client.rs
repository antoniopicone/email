//! The synchronous IMAP session used by the per-account worker threads.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{Local, TimeZone};
use mail_parser::{Address, MessageParser, MimeHeaders};
use native_tls::{TlsConnector, TlsStream};

use super::xoauth2::XOAuth2;
use crate::model::{
    Account, Attachment, Credentials, Mailaddr, Mailbox, MailboxKind, Message, MessageSummary,
};

type Session = imap::Session<TlsStream<TcpStream>>;

/// Bounds how long the initial TCP handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bounds every subsequent read/write on the socket. Without this, a
/// connection that stops answering mid-command (a dropped packet a firewall
/// or a broken IPv6 path swallows, say) blocks the worker thread forever:
/// the UI would keep showing that account as "syncing" indefinitely, with no
/// error and no way to recover short of restarting the app.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Open a TCP connection with both a connect and an I/O deadline, so a
/// server or network that stops responding fails loudly instead of hanging
/// the worker thread.
fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let addr = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}:{port}"))?
        .next()
        .ok_or_else(|| anyhow!("no address found for {host}:{port}"))?;
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .with_context(|| format!("connecting to {host}:{port}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).context("setting the socket read timeout")?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).context("setting the socket write timeout")?;
    Ok(stream)
}

/// Fields we ask for when building the message list. Everything here is cheap
/// except the leading kilobyte of body text, which gives us the list preview
/// without a second round trip.
const LIST_QUERY: &str =
    "(UID FLAGS ENVELOPE INTERNALDATE RFC822.SIZE BODYSTRUCTURE BODY.PEEK[TEXT]<0.1024>)";

pub struct ImapClient {
    account: Account,
    session: Session,
    delimiter: char,
}

impl ImapClient {
    /// Open a TLS connection and authenticate.
    pub fn connect(account: &Account, credentials: &Credentials) -> Result<Self> {
        let tls = TlsConnector::builder()
            .build()
            .context("initialising TLS")?;

        let host = account.imap_host.as_str();
        let port = account.imap_port;

        log::info!(
            "connecting to {host}:{port} as {} using {}",
            account.imap_user,
            credentials.mechanism()
        );

        let tcp = connect_tcp(host, port)?;

        let client = if account.use_starttls {
            let mut plain = imap::Client::new(tcp);
            plain
                .read_greeting()
                .with_context(|| format!("reading the greeting from {host}:{port}"))?;
            plain
                .secure(host, &tls)
                .map_err(|e| anyhow!("STARTTLS negotiation with {host}:{port} failed: {e}"))?
        } else {
            let tls_stream = tls
                .connect(host, tcp)
                .with_context(|| format!("TLS handshake with {host}:{port}"))?;
            let mut socket = imap::Client::new(tls_stream);
            socket
                .read_greeting()
                .with_context(|| format!("reading the greeting from {host}:{port}"))?;
            socket
        };

        let session = match credentials {
            Credentials::OAuth2(token) => {
                let auth = XOAuth2::new(&account.imap_user, token);
                client.authenticate("XOAUTH2", &auth).map_err(|(e, _)| {
                    anyhow!("XOAUTH2 authentication failed for {}: {e}", account.email)
                })?
            }
            Credentials::Password(password) => {
                client.login(&account.imap_user, password).map_err(|(e, _)| {
                    anyhow!("login failed for {}: {e}", account.email)
                })?
            }
        };

        log::info!("authenticated as {}", account.email);

        Ok(Self {
            account: account.clone(),
            session,
            delimiter: '/',
        })
    }

    /// List the folders on the server, with unread counts.
    pub fn list_mailboxes(&mut self) -> Result<Vec<Mailbox>> {
        let names = self.session.list(Some(""), Some("*")).context("listing mailboxes")?;

        let mut raw: Vec<(String, Vec<String>)> = Vec::new();
        for name in names.iter() {
            if let Some(delim) = name.delimiter().and_then(|d| d.chars().next()) {
                self.delimiter = delim;
            }
            let attributes: Vec<String> =
                name.attributes().iter().map(|a| format!("{a:?}")).collect();

            // `\Noselect` folders are pure containers, e.g. Gmail's `[Gmail]`.
            let selectable = !attributes.iter().any(|a| a.contains("NoSelect"));
            if !selectable {
                continue;
            }
            raw.push((name.name().to_string(), attributes));
        }

        let delimiter = self.delimiter;
        let mut mailboxes = Vec::new();

        for (path, attributes) in raw {
            // `format!("{:?}")` on NameAttribute renders `Custom("\\Sent")`, so
            // pull the inner token back out for classification.
            let cleaned: Vec<String> = attributes
                .iter()
                .map(|a| {
                    a.trim_start_matches("Custom(")
                        .trim_end_matches(')')
                        .trim_matches('"')
                        .replace("\\\\", "\\")
                        .to_string()
                })
                .collect();

            let kind = MailboxKind::classify(&path, &cleaned);
            let leaf = path.rsplit(delimiter).next().unwrap_or(&path).to_string();

            let (unread, total) = self.folder_counts(&path);

            mailboxes.push(Mailbox {
                account_id: self.account.id.clone(),
                path: path.clone(),
                name: leaf,
                kind,
                unread,
                total,
            });
        }

        // Standard mailboxes first in Apple Mail's order, then the rest
        // alphabetically.
        mailboxes.sort_by(|a, b| {
            a.kind
                .order()
                .cmp(&b.kind.order())
                .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
        });

        Ok(mailboxes)
    }

    /// STATUS is cheap and does not disturb the selected mailbox.
    /// (unread, total) for one folder.
    ///
    /// The `imap` crate parses a `STATUS` reply correctly at the wire level,
    /// but its `Mailbox::exists`/`::unseen` fields only ever come from the
    /// untagged responses `SELECT`/`EXAMINE` send (`* n EXISTS`, `* OK
    /// [UNSEEN n]`). A bare `STATUS` command's `* STATUS "mbox" (MESSAGES n
    /// UNSEEN n)` line takes a different shape on the wire, which the crate
    /// treats as an *unsolicited* update and only ever forwards to
    /// `Session::unsolicited_responses` — so `status()` itself always hands
    /// back an untouched, all-zero default `Mailbox`. The real numbers are
    /// sitting in that channel; read them back out of it instead.
    fn folder_counts(&mut self, path: &str) -> (u32, u32) {
        if let Err(e) = self.session.status(path, "(MESSAGES UNSEEN)") {
            log::debug!("STATUS failed for {path}: {e}");
            return (0, 0);
        }

        while let Ok(response) = self.session.unsolicited_responses.try_recv() {
            let imap::types::UnsolicitedResponse::Status { mailbox, attributes } = response else {
                continue;
            };
            if mailbox != path {
                continue;
            }
            let mut unseen = 0;
            let mut total = 0;
            for attribute in attributes {
                match attribute {
                    imap::types::StatusAttribute::Unseen(n) => unseen = n,
                    imap::types::StatusAttribute::Messages(n) => total = n,
                    _ => {}
                }
            }
            return (unseen, total);
        }

        log::debug!("STATUS for {path} produced no status data");
        (0, 0)
    }

    /// Always re-issues SELECT, even when `mailbox` is already open.
    ///
    /// An earlier version skipped straight to STATUS when the mailbox was
    /// already selected, to save a round trip. STATUS on the *currently
    /// selected* mailbox is explicitly discouraged by RFC 3501 §6.3.10, and
    /// some servers answer it with a stale or zero EXISTS count instead of
    /// the real one — which made the message list "lose" every message in
    /// the folder on a refresh. SELECT always gives an accurate count.
    fn select(&mut self, mailbox: &str) -> Result<u32> {
        let meta = self
            .session
            .select(mailbox)
            .with_context(|| format!("selecting {mailbox}"))?;
        Ok(meta.exists)
    }

    /// Fetch the newest `limit` messages of `mailbox`, skipping the newest
    /// `offset` (used for paging further back in history).
    pub fn list_messages(
        &mut self,
        mailbox: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageSummary>> {
        let exists = self.select(mailbox)?;
        if exists == 0 {
            return Ok(Vec::new());
        }

        let end = exists.saturating_sub(offset);
        if end == 0 {
            return Ok(Vec::new());
        }
        let start = end.saturating_sub(limit.saturating_sub(1)).max(1);
        let sequence = format!("{start}:{end}");

        let fetches = self
            .session
            .fetch(&sequence, LIST_QUERY)
            .with_context(|| format!("fetching {sequence} from {mailbox}"))?;

        let mut summaries: Vec<MessageSummary> = fetches
            .iter()
            .filter_map(|f| self.summary_from_fetch(mailbox, f))
            .collect();

        // Newest first, the way a mail client shows a folder.
        summaries.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(summaries)
    }

    fn summary_from_fetch(&self, mailbox: &str, fetch: &imap::types::Fetch) -> Option<MessageSummary> {
        let uid = fetch.uid?;
        let envelope = fetch.envelope();

        let subject = envelope
            .and_then(|e| e.subject.as_ref())
            .map(|s| decode_header_bytes(s))
            .unwrap_or_default();

        let from = envelope
            .and_then(|e| e.from.as_ref())
            .and_then(|list| list.first())
            .map(address_from_imap)
            .unwrap_or_default();

        let to = envelope
            .and_then(|e| e.to.as_ref())
            .map(|list| list.iter().map(address_from_imap).collect())
            .unwrap_or_default();

        let date = fetch
            .internal_date()
            .map(|d| d.with_timezone(&Local))
            .or_else(|| {
                envelope
                    .and_then(|e| e.date.as_ref())
                    .and_then(|d| parse_rfc2822(&String::from_utf8_lossy(d)))
            })
            .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap_or_else(Local::now));

        let flags: Vec<String> = fetch.flags().iter().map(|f| format!("{f:?}")).collect();
        let has = |needle: &str| flags.iter().any(|f| f.contains(needle));

        let snippet = fetch
            .text()
            .map(|raw| snippet_from_partial_body(raw))
            .unwrap_or_default();

        let has_attachments = fetch
            .bodystructure()
            .map(bodystructure_has_attachment)
            .unwrap_or(false);

        Some(MessageSummary {
            account_id: self.account.id.clone(),
            mailbox: mailbox.to_string(),
            uid,
            from,
            to,
            subject,
            snippet,
            date,
            seen: has("Seen"),
            flagged: has("Flagged"),
            answered: has("Answered"),
            has_attachments,
        })
    }

    /// Fetch and parse one full message.
    pub fn load_message(&mut self, mailbox: &str, uid: u32) -> Result<Message> {
        self.select(mailbox)?;

        let fetches = self
            .session
            .uid_fetch(uid.to_string(), "(UID FLAGS INTERNALDATE RFC822)")
            .with_context(|| format!("fetching message {uid} from {mailbox}"))?;

        let fetch = fetches
            .iter()
            .next()
            .ok_or_else(|| anyhow!("message {uid} not found in {mailbox}"))?;

        let raw = fetch
            .body()
            .ok_or_else(|| anyhow!("message {uid} came back without a body"))?;

        let parsed = MessageParser::default()
            .parse(raw)
            .ok_or_else(|| anyhow!("could not parse message {uid}"))?;

        let from = parsed
            .from()
            .and_then(first_address)
            .unwrap_or_default();
        let to = parsed.to().map(all_addresses).unwrap_or_default();
        let cc = parsed.cc().map(all_addresses).unwrap_or_default();

        let text = parsed.body_text(0).map(|t| t.into_owned()).unwrap_or_default();
        let html_raw = parsed.body_html(0).map(|h| h.into_owned());

        let attachments: Vec<Attachment> = parsed
            .attachments()
            .map(|part| {
                let data = part.contents().to_vec();
                Attachment {
                    filename: part
                        .attachment_name()
                        .unwrap_or("allegato")
                        .to_string(),
                    mime_type: part
                        .content_type()
                        .map(|ct| match ct.subtype() {
                            Some(sub) => format!("{}/{}", ct.ctype(), sub),
                            None => ct.ctype().to_string(),
                        })
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    size: data.len(),
                    data,
                }
            })
            .collect();

        let date = parsed
            .date()
            .and_then(|d| {
                Local
                    .timestamp_opt(d.to_timestamp(), 0)
                    .single()
            })
            .or_else(|| fetch.internal_date().map(|d| d.with_timezone(&Local)))
            .unwrap_or_else(Local::now);

        let flags: Vec<String> = fetch.flags().iter().map(|f| format!("{f:?}")).collect();
        let has = |needle: &str| flags.iter().any(|f| f.contains(needle));

        let subject = parsed.subject().unwrap_or_default().to_string();
        let snippet = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();

        let summary = MessageSummary {
            account_id: self.account.id.clone(),
            mailbox: mailbox.to_string(),
            uid,
            from,
            to,
            subject,
            snippet,
            date,
            seen: has("Seen"),
            flagged: has("Flagged"),
            answered: has("Answered"),
            has_attachments: !attachments.is_empty(),
        };

        Ok(Message {
            summary,
            cc,
            html: html_raw.map(|h| crate::html::sanitize(&h)),
            text,
            attachments,
        })
    }

    /// Add or remove a flag on a message.
    pub fn set_flag(&mut self, mailbox: &str, uid: u32, flag: &str, on: bool) -> Result<()> {
        self.select(mailbox)?;
        let op = if on { "+FLAGS" } else { "-FLAGS" };
        self.session
            .uid_store(uid.to_string(), format!("{op} ({flag})"))
            .with_context(|| format!("setting {flag} on message {uid}"))?;
        Ok(())
    }

    /// Move a message to another folder, falling back to copy+delete on
    /// servers without the MOVE extension.
    pub fn move_message(&mut self, mailbox: &str, uid: u32, target: &str) -> Result<()> {
        self.select(mailbox)?;
        match self.session.uid_mv(uid.to_string(), target) {
            Ok(()) => Ok(()),
            Err(e) => {
                log::debug!("UID MOVE unsupported ({e}), falling back to COPY+STORE");
                self.session
                    .uid_copy(uid.to_string(), target)
                    .with_context(|| format!("copying message {uid} to {target}"))?;
                self.session
                    .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
                    .with_context(|| format!("marking message {uid} deleted"))?;
                self.session.expunge().context("expunging after a copy-move")?;
                Ok(())
            }
        }
    }

    /// File a copy of a just-sent message in the Sent folder.
    ///
    /// Servers do not do this for us, and a sent message that only exists in
    /// the recipient's mailbox is a surprising thing to hand a user.
    pub fn append_to_sent(&mut self, raw: &[u8]) -> Result<()> {
        self.append_to(MailboxKind::Sent, raw, &[imap::types::Flag::Seen])
    }

    /// Save a message to the Drafts folder.
    pub fn append_to_drafts(&mut self, raw: &[u8]) -> Result<()> {
        self.append_to(
            MailboxKind::Drafts,
            raw,
            &[imap::types::Flag::Draft, imap::types::Flag::Seen],
        )
    }

    fn append_to(&mut self, kind: MailboxKind, raw: &[u8], flags: &[imap::types::Flag]) -> Result<()> {
        let folder = self
            .list_mailboxes()?
            .into_iter()
            .find(|m| m.kind == kind)
            .map(|m| m.path);

        let Some(folder) = folder else {
            log::info!("no {kind:?} folder on this account, skipping the copy");
            return Ok(());
        };

        self.session
            .append_with_flags(&folder, raw, flags)
            .with_context(|| format!("appending to {folder}"))?;

        Ok(())
    }

    /// The folder a "delete" should move to, if there is one.
    pub fn trash_folder(&mut self) -> Option<String> {
        self.list_mailboxes()
            .ok()?
            .into_iter()
            .find(|m| m.kind == MailboxKind::Trash)
            .map(|m| m.path)
    }

    pub fn logout(&mut self) {
        let _ = self.session.logout();
    }
}

fn address_from_imap(addr: &imap_proto::Address) -> Mailaddr {
    let name = addr.name.as_ref().map(|n| decode_header_bytes(n)).unwrap_or_default();
    let mailbox = addr.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).into_owned());
    let host = addr.host.as_ref().map(|h| String::from_utf8_lossy(h).into_owned());
    let address = match (mailbox, host) {
        (Some(m), Some(h)) => format!("{m}@{h}"),
        (Some(m), None) => m,
        _ => String::new(),
    };
    Mailaddr { name, address }
}

fn first_address(address: &Address) -> Option<Mailaddr> {
    all_addresses(address).into_iter().next()
}

fn all_addresses(address: &Address) -> Vec<Mailaddr> {
    match address {
        Address::List(list) => list
            .iter()
            .map(|a| Mailaddr {
                name: a.name.as_deref().unwrap_or_default().to_string(),
                address: a.address.as_deref().unwrap_or_default().to_string(),
            })
            .collect(),
        Address::Group(groups) => groups
            .iter()
            .flat_map(|g| g.addresses.iter())
            .map(|a| Mailaddr {
                name: a.name.as_deref().unwrap_or_default().to_string(),
                address: a.address.as_deref().unwrap_or_default().to_string(),
            })
            .collect(),
    }
}

/// Decode an RFC 2047 encoded-word header, e.g. `=?UTF-8?B?...?=`.
///
/// mail-parser handles this for full messages; ENVELOPE gives us raw bytes, so
/// we run them back through the parser as a synthetic Subject header.
fn decode_header_bytes(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    if !raw.contains("=?") {
        return raw.into_owned();
    }
    let synthetic = format!("Subject: {raw}\r\n\r\n");
    MessageParser::default()
        .parse(synthetic.as_bytes())
        .and_then(|m| m.subject().map(|s| s.to_string()))
        .unwrap_or_else(|| raw.into_owned())
}

fn parse_rfc2822(value: &str) -> Option<chrono::DateTime<Local>> {
    chrono::DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|d| d.with_timezone(&Local))
}

/// Turn the first kilobyte of a raw body into a one-line list preview.
fn snippet_from_partial_body(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let body = skip_mime_preamble(&text);

    let decoded = if looks_like_base64(body) {
        decode_partial_base64(body)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|| decode_quoted_printable(body))
    } else {
        decode_quoted_printable(body)
    };
    let stripped = crate::html::to_plain_text(&decoded);

    stripped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// A partial `BODY[TEXT]` fetch starts at the MIME preamble, not at a leaf
/// part's actual content. A single top-level blank-line skip is only enough
/// for a flat message: a *nested* multipart (e.g. `multipart/mixed` wrapping
/// a `multipart/alternative`) has several `--boundary` + part-header blocks
/// stacked up, each ending at its own blank line, before real text starts —
/// so this walks past as many of those blocks as it finds.
fn skip_mime_preamble(text: &str) -> &str {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start_matches(['\r', '\n']);
        let first_line = trimmed.split(['\r', '\n']).next().unwrap_or("");
        // "-- " (and bare "--") is the conventional signature delimiter, not
        // a MIME boundary — never treat it as one, or a real signature line
        // eats the rest of the preview.
        let is_boundary = first_line.starts_with("--") && first_line.trim() != "--";
        let is_header = is_mime_header_line(first_line);
        if !is_boundary && !is_header {
            return rest;
        }
        match find_blank_line(trimmed) {
            Some(idx) => rest = &trimmed[idx..],
            None => return rest,
        }
    }
}

fn find_blank_line(text: &str) -> Option<usize> {
    if let Some(idx) = text.find("\r\n\r\n") {
        Some(idx + 4)
    } else {
        text.find("\n\n").map(|idx| idx + 2)
    }
}

fn is_mime_header_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("content-type:")
        || lower.starts_with("content-transfer-encoding:")
        || lower.starts_with("content-disposition:")
        || lower.starts_with("mime-version:")
}

/// A forgiving quoted-printable decoder for preview text only.
///
/// Decodes into raw bytes and only turns those into a `String` right at the
/// end. A non-ASCII character is `=XX=YY=ZZ` — three escapes forming one
/// multi-byte UTF-8 sequence — so casting each decoded byte to `char` as it
/// comes off (the previous version did exactly that) reads each byte as its
/// own Latin-1 codepoint and mangles every accented letter and emoji into
/// mojibake instead of reassembling them.
fn decode_quoted_printable(input: &str) -> String {
    if !input.contains('=') {
        return input.to_string();
    }
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            let hex = &input[i + 1..i + 3];
            if hex == "\r\n" || hex.starts_with('\n') {
                i += if hex.starts_with('\n') { 2 } else { 3 };
                continue;
            }
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A partial fetch has no way to ask the server to decode
/// Content-Transfer-Encoding for us, so base64 has to be guessed from the
/// bytes — its alphabet is distinctive enough that plain text essentially
/// never matches it by chance.
fn looks_like_base64(text: &str) -> bool {
    let stripped: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    stripped.len() >= 8 && stripped.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
}

/// Decode as much base64 as divides evenly into 4-character groups — a
/// partial fetch is cut off at an arbitrary byte, so the tail past the last
/// full group is dropped rather than treated as a decode failure.
fn decode_partial_base64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let stripped: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let usable_len = (stripped.len() / 4) * 4;
    if usable_len == 0 {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(&stripped[..usable_len]).ok()
}

/// Walk a BODYSTRUCTURE looking for a part with a filename or an
/// `attachment` disposition.
fn bodystructure_has_attachment(body: &imap_proto::BodyStructure) -> bool {
    use imap_proto::BodyStructure;

    fn disposition_is_attachment(disposition: &Option<imap_proto::ContentDisposition>) -> bool {
        let Some(d) = disposition else { return false };
        if d.ty.to_ascii_lowercase().contains("attachment") {
            return true;
        }
        // Some servers only signal an attachment through a filename parameter.
        d.params
            .as_ref()
            .map(|params| params.iter().any(|(key, _)| key.eq_ignore_ascii_case("filename")))
            .unwrap_or(false)
    }

    match body {
        BodyStructure::Multipart { bodies, .. } => bodies.iter().any(bodystructure_has_attachment),
        BodyStructure::Basic { common, .. }
        | BodyStructure::Message { common, .. }
        | BodyStructure::Text { common, .. } => disposition_is_attachment(&common.disposition),
    }
}

/// Resolve credentials for an account, from GOA or the keyring. `purpose`
/// only matters for GOA `PasswordBased` accounts, which can keep separate
/// secrets for IMAP and SMTP.
pub fn resolve_credentials(
    account: &Account,
    purpose: crate::goa::CredentialPurpose,
) -> Result<Credentials> {
    use crate::model::AccountSource;
    match account.source {
        AccountSource::Gnome => crate::goa::credentials_blocking(account, purpose),
        AccountSource::Manual => {
            let password = crate::secrets::lookup_password_blocking(&account.id)
                .context("reading the password from the keyring")?
                .ok_or_else(|| {
                    anyhow!("no password saved for {}; add it in the account settings", account.email)
                })?;
            Ok(Credentials::Password(password))
        }
        AccountSource::Demo => Err(anyhow!("demo accounts do not connect to a server")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn quoted_printable_reassembles_multibyte_utf8() {
        // è as =C3=A8 (two escapes, one 2-byte UTF-8 character) — decoding
        // byte-by-byte into `char` would turn this into two Latin-1
        // characters (Ã¨) instead of one correct one.
        assert_eq!(decode_quoted_printable("caff=C3=A8"), "caffè");
        // 👤 as =F0=9F=91=A4 (four escapes, one 4-byte UTF-8 character).
        assert_eq!(decode_quoted_printable("=F0=9F=91=A4 Ilaria"), "👤 Ilaria");
    }

    #[test]
    fn quoted_printable_leaves_plain_ascii_alone() {
        assert_eq!(decode_quoted_printable("hello world"), "hello world");
    }

    #[test]
    fn recognises_base64_alphabet() {
        assert!(looks_like_base64("DQpQaG90b2dyYXBoIG9mIEFuZHJlYQ=="));
        assert!(!looks_like_base64("Ciao, come stai oggi?"));
        assert!(!looks_like_base64("short"));
    }

    #[test]
    fn decodes_a_base64_prefix_even_when_truncated_mid_group() {
        // "Hello, world!" is 13 bytes -> 20 base64 chars incl. padding;
        // chop it well short of a 4-char boundary, the way a 1KB partial
        // IMAP fetch would land mid-stream.
        let full = base64::engine::general_purpose::STANDARD.encode("Hello, world!");
        let truncated = &full[..full.len() - 3];
        let decoded = decode_partial_base64(truncated).expect("should decode the whole groups");
        assert_eq!(String::from_utf8_lossy(&decoded), "Hello, world");
    }

    #[test]
    fn snippet_decodes_base64_body() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("Ciao a tutti, come va?");
        let raw = format!("Content-Type: text/plain\r\n\r\n{encoded}");
        assert_eq!(snippet_from_partial_body(raw.as_bytes()), "Ciao a tutti, come va?");
    }

    #[test]
    fn snippet_decodes_quoted_printable_body_with_accents() {
        let raw = b"Content-Type: text/plain\r\n\r\nCiao, tutto bene? Un caff=C3=A8 con te?";
        assert_eq!(snippet_from_partial_body(raw), "Ciao, tutto bene? Un caffè con te?");
    }

    #[test]
    fn snippet_skips_nested_multipart_boundaries_and_headers() {
        // multipart/mixed wrapping a multipart/alternative — the shape of the
        // real promotional email that leaked its MIME boundary line and part
        // headers into the list preview ("--TKcJI7Jo1M9w=_? Content-Type:
        // text/plain; charset=\"utf-8\" Content-Transfer-Encoding: 8bit Live
        // Na...").
        let raw = concat!(
            "--outerBoundary\r\n",
            "Content-Type: multipart/alternative; boundary=\"innerBoundary\"\r\n",
            "\r\n",
            "--innerBoundary\r\n",
            "Content-Type: text/plain; charset=\"utf-8\"\r\n",
            "Content-Transfer-Encoding: 8bit\r\n",
            "\r\n",
            "Live Nation presents Oasis, live on stage.",
        );
        assert_eq!(
            snippet_from_partial_body(raw.as_bytes()),
            "Live Nation presents Oasis, live on stage."
        );
    }

    #[test]
    fn snippet_preview_does_not_eat_a_signature_delimiter() {
        let raw = b"Content-Type: text/plain\r\n\r\nCiao!\r\n-- \r\nInviato dal mio iPhone";
        let snippet = snippet_from_partial_body(raw);
        assert!(snippet.starts_with("Ciao!"), "signature delimiter ate the real content: {snippet}");
    }
}

