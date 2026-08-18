//! Outgoing mail.
//!
//! Sending reuses whatever credentials the account already authenticates with:
//! a password for plain IMAP servers, or the same GNOME Online Accounts OAuth2
//! token that Gmail's IMAP side uses, presented over SASL XOAUTH2.

use anyhow::{anyhow, Context, Result};
use lettre::message::{header::ContentType, Mailbox as LettreMailbox, Message as LettreMessage};
use lettre::transport::smtp::authentication::{Credentials as SmtpCredentials, Mechanism};
use lettre::{SmtpTransport, Transport};

use crate::model::{Account, Credentials};

/// A message the user has written and asked to send.
#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    /// `Message-ID` of the message being answered, when this is a reply.
    pub in_reply_to: Option<String>,
}

/// Split a comma- or semicolon-separated recipient list into mailboxes.
///
/// Also used by the compose window to validate a field's contents live, so
/// what turns green there is exactly what will parse at send time.
pub fn parse_recipients(list: &str) -> Result<Vec<LettreMailbox>> {
    let mut out = Vec::new();
    for candidate in list.split([',', ';']) {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mailbox: LettreMailbox = trimmed
            .parse()
            .with_context(|| format!("indirizzo non valido: {trimmed}"))?;
        out.push(mailbox);
    }
    Ok(out)
}

/// Build the RFC 5322 message. Returned separately from sending so the caller
/// can also append a copy to the Sent folder.
pub fn build(account: &Account, outgoing: &Outgoing) -> Result<LettreMessage> {
    let from: LettreMailbox = format!("{} <{}>", account.display_name, account.email)
        .parse()
        .or_else(|_| account.email.parse())
        .with_context(|| format!("indirizzo mittente non valido: {}", account.email))?;

    let to = parse_recipients(&outgoing.to)?;
    if to.is_empty() {
        return Err(anyhow!("indica almeno un destinatario"));
    }
    let cc = parse_recipients(&outgoing.cc)?;
    let bcc = parse_recipients(&outgoing.bcc)?;

    let mut builder = LettreMessage::builder().from(from).subject(&outgoing.subject);
    for mailbox in to {
        builder = builder.to(mailbox);
    }
    for mailbox in cc {
        builder = builder.cc(mailbox);
    }
    for mailbox in bcc {
        builder = builder.bcc(mailbox);
    }
    if let Some(reference) = &outgoing.in_reply_to {
        if !reference.trim().is_empty() {
            builder = builder.in_reply_to(reference.clone()).references(reference.clone());
        }
    }

    builder
        .header(ContentType::TEXT_PLAIN)
        .body(outgoing.body.clone())
        .context("comporre il messaggio")
}

/// Send a message over SMTP.
pub fn send(account: &Account, credentials: &Credentials, outgoing: &Outgoing) -> Result<Vec<u8>> {
    if account.smtp_host.trim().is_empty() {
        return Err(anyhow!(
            "nessun server SMTP configurato per {}; impostalo nelle preferenze dell'account",
            account.email
        ));
    }

    let message = build(account, outgoing)?;
    let user = if account.smtp_user.is_empty() { &account.email } else { &account.smtp_user };

    // Port 465 is implicit TLS; everything else negotiates STARTTLS.
    let builder = if account.smtp_port == 465 {
        SmtpTransport::relay(&account.smtp_host)
    } else {
        SmtpTransport::starttls_relay(&account.smtp_host)
    }
    .with_context(|| format!("preparing a connection to {}", account.smtp_host))?;

    let (secret, mechanism) = match credentials {
        Credentials::Password(password) => (password.clone(), Mechanism::Plain),
        Credentials::OAuth2(token) => (token.clone(), Mechanism::Xoauth2),
    };

    let transport = builder
        .port(account.smtp_port)
        .credentials(SmtpCredentials::new(user.clone(), secret))
        .authentication(vec![mechanism])
        .build();

    log::info!(
        "sending via {}:{} as {user} using {mechanism:?}",
        account.smtp_host,
        account.smtp_port
    );

    transport
        .send(&message)
        .with_context(|| format!("invio tramite {}", account.smtp_host))?;

    Ok(message.formatted())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_recipients_on_commas_and_semicolons() {
        let parsed = parse_recipients("a@x.it, Nome Cognome <b@y.it>; c@z.it").unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].email.to_string(), "b@y.it");
    }

    #[test]
    fn ignores_empty_entries() {
        assert_eq!(parse_recipients("  ,  a@x.it ,, ").unwrap().len(), 1);
        assert!(parse_recipients("").unwrap().is_empty());
    }

    #[test]
    fn rejects_malformed_addresses() {
        assert!(parse_recipients("non-un-indirizzo").is_err());
    }
}
