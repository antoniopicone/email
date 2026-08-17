//! GNOME Online Accounts integration.
//!
//! GOA is the service behind Settings → Online Accounts. When the user has
//! already added their Google (or any IMAP) account there, we can reuse those
//! settings and — crucially for Gmail — ask GOA for a fresh OAuth2 access
//! token instead of running our own OAuth flow or asking for a password.
//!
//! The service exposes a `org.freedesktop.DBus.ObjectManager` at
//! `/org/gnome/OnlineAccounts`. Each account object carries:
//!
//! * `org.gnome.OnlineAccounts.Account` — identity and provider metadata
//! * `org.gnome.OnlineAccounts.Mail` — IMAP/SMTP host, port and user name
//! * `org.gnome.OnlineAccounts.OAuth2Based` — `GetAccessToken()` for Gmail
//! * `org.gnome.OnlineAccounts.PasswordBased` — `GetPassword()` for plain IMAP

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use zbus::zvariant::OwnedValue;

use crate::model::{Account, AccountSource, Credentials};
use crate::runtime;

const GOA_SERVICE: &str = "org.gnome.OnlineAccounts";
const GOA_MANAGER_PATH: &str = "/org/gnome/OnlineAccounts";
const IFACE_ACCOUNT: &str = "org.gnome.OnlineAccounts.Account";
const IFACE_MAIL: &str = "org.gnome.OnlineAccounts.Mail";
const IFACE_OAUTH2: &str = "org.gnome.OnlineAccounts.OAuth2Based";
const IFACE_PASSWORD: &str = "org.gnome.OnlineAccounts.PasswordBased";

#[zbus::proxy(
    interface = "org.gnome.OnlineAccounts.OAuth2Based",
    default_service = "org.gnome.OnlineAccounts"
)]
trait OAuth2Based {
    /// Returns the current access token and its remaining lifetime in seconds.
    /// GOA refreshes it transparently when it has expired.
    fn get_access_token(&self) -> zbus::Result<(String, i32)>;
}

#[zbus::proxy(
    interface = "org.gnome.OnlineAccounts.PasswordBased",
    default_service = "org.gnome.OnlineAccounts"
)]
trait PasswordBased {
    fn get_password(&self, id: &str) -> zbus::Result<String>;
}

/// How a GOA account authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoaAuth {
    OAuth2,
    Password,
}

/// An account as advertised by GOA, before we turn it into an [`Account`].
#[derive(Debug, Clone)]
pub struct GoaAccount {
    pub object_path: String,
    pub id: String,
    pub provider_type: String,
    pub provider_name: String,
    pub presentation_identity: String,
    pub email: String,
    pub display_name: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_user: String,
    pub imap_starttls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub auth: GoaAuth,
}

impl GoaAccount {
    pub fn into_account(self) -> Account {
        Account {
            id: format!("goa:{}", self.id),
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            imap_host: self.imap_host.clone(),
            imap_port: self.imap_port,
            imap_user: self.imap_user.clone(),
            use_starttls: self.imap_starttls,
            smtp_host: self.smtp_host.clone(),
            smtp_port: self.smtp_port,
            smtp_user: self.smtp_user.clone(),
            source: AccountSource::Gnome,
            goa_path: Some(self.object_path.clone()),
        }
    }
}

fn as_string(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn as_bool(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props.get(key).and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false)
}

fn as_u16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u16> {
    let value = props.get(key)?;
    // GOA reports ports as unsigned 32-bit integers.
    if let Ok(v) = u32::try_from(value.clone()) {
        return u16::try_from(v).ok();
    }
    if let Ok(v) = i32::try_from(value.clone()) {
        return u16::try_from(v).ok();
    }
    u16::try_from(value.clone()).ok()
}

/// Enumerate every GOA account that has mail enabled.
///
/// A missing GOA service is not an error: plenty of systems do not run GNOME.
/// In that case this returns an empty list.
pub async fn discover() -> Result<Vec<GoaAccount>> {
    let connection = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            log::info!("no session bus, skipping GNOME Online Accounts: {e}");
            return Ok(Vec::new());
        }
    };

    let manager = zbus::fdo::ObjectManagerProxy::builder(&connection)
        .destination(GOA_SERVICE)?
        .path(GOA_MANAGER_PATH)?
        .build()
        .await;

    let manager = match manager {
        Ok(m) => m,
        Err(e) => {
            log::info!("GNOME Online Accounts unavailable: {e}");
            return Ok(Vec::new());
        }
    };

    let objects = match manager.get_managed_objects().await {
        Ok(o) => o,
        Err(e) => {
            log::info!("GNOME Online Accounts is not running: {e}");
            return Ok(Vec::new());
        }
    };

    let mut accounts = Vec::new();

    for (path, interfaces) in objects {
        let Some(account) = interfaces.get(IFACE_ACCOUNT) else { continue };
        let Some(mail) = interfaces.get(IFACE_MAIL) else { continue };

        // The user can keep an account but switch its Mail feature off.
        if as_bool(account, "MailDisabled") {
            log::debug!("skipping {path}: mail disabled in GNOME settings");
            continue;
        }

        let imap_host = as_string(mail, "ImapHost");
        if imap_host.is_empty() {
            continue;
        }

        let auth = if interfaces.contains_key(IFACE_OAUTH2) {
            GoaAuth::OAuth2
        } else if interfaces.contains_key(IFACE_PASSWORD) {
            GoaAuth::Password
        } else {
            log::warn!("skipping {path}: no supported authentication interface");
            continue;
        };

        // GOA distinguishes implicit TLS (`ImapUseSsl`) from STARTTLS
        // (`ImapUseTls`) and omits the port when it is the default.
        let starttls = as_bool(mail, "ImapUseTls") && !as_bool(mail, "ImapUseSsl");
        let imap_port = as_u16(mail, "ImapPort")
            .filter(|p| *p != 0)
            .unwrap_or(if starttls { 143 } else { 993 });
        let smtp_port = as_u16(mail, "SmtpPort").filter(|p| *p != 0).unwrap_or(587);

        let presentation_identity = as_string(account, "PresentationIdentity");

        let email = {
            let addr = as_string(mail, "EmailAddress");
            if addr.is_empty() {
                presentation_identity.clone()
            } else {
                addr
            }
        };

        let imap_user = {
            let user = as_string(mail, "ImapUserName");
            if user.is_empty() {
                email.clone()
            } else {
                user
            }
        };

        let display_name = {
            let name = as_string(mail, "Name");
            if name.is_empty() {
                as_string(account, "ProviderName")
            } else {
                name
            }
        };

        let provider_type = as_string(account, "ProviderType");
        log::debug!(
            "GNOME account {path}: provider={provider_type}, identity={presentation_identity}, \
             auth={auth:?}, imap={imap_host}:{imap_port}"
        );

        accounts.push(GoaAccount {
            object_path: path.to_string(),
            id: as_string(account, "Id"),
            provider_type,
            provider_name: as_string(account, "ProviderName"),
            presentation_identity,
            email,
            display_name,
            imap_host,
            imap_port,
            imap_user,
            imap_starttls: starttls,
            smtp_host: as_string(mail, "SmtpHost"),
            smtp_port,
            smtp_user: as_string(mail, "SmtpUserName"),
            auth,
        });
    }

    accounts.sort_by(|a, b| a.email.cmp(&b.email));
    log::info!("GNOME Online Accounts: found {} mail account(s)", accounts.len());
    Ok(accounts)
}

/// Ask GOA for a fresh OAuth2 access token for the account at `object_path`.
///
/// GOA owns the refresh token and renews expired access tokens itself, so this
/// is the whole of our Gmail authentication story.
pub async fn access_token(object_path: &str) -> Result<String> {
    let connection = zbus::Connection::session().await.context("connecting to the session bus")?;
    let proxy = OAuth2BasedProxy::builder(&connection)
        .path(object_path.to_string())?
        .build()
        .await
        .context("building the OAuth2 proxy")?;
    let (token, expires_in) = proxy
        .get_access_token()
        .await
        .with_context(|| format!("requesting an access token for {object_path}"))?;
    log::debug!("got access token for {object_path}, expires in {expires_in}s");
    if token.is_empty() {
        return Err(anyhow!("GNOME Online Accounts returned an empty access token"));
    }
    Ok(token)
}

/// Ask GOA for the stored password of a password-based account.
pub async fn password(object_path: &str, id: &str) -> Result<String> {
    let connection = zbus::Connection::session().await.context("connecting to the session bus")?;
    let proxy = PasswordBasedProxy::builder(&connection)
        .path(object_path.to_string())?
        .build()
        .await
        .context("building the password proxy")?;
    proxy
        .get_password(id)
        .await
        .with_context(|| format!("requesting the password for {object_path}"))
        .map_err(Into::into)
}

/// Blocking wrappers, for use from the synchronous IMAP worker threads.
pub fn discover_blocking() -> Result<Vec<GoaAccount>> {
    runtime::handle().block_on(discover())
}

/// Resolve the credentials for a GOA-backed account, refreshing the OAuth2
/// token if that is what the provider uses.
pub fn credentials_blocking(account: &Account) -> Result<Credentials> {
    let path = account
        .goa_path
        .as_deref()
        .ok_or_else(|| anyhow!("account {} is not backed by GNOME Online Accounts", account.id))?;
    let goa_id = account.id.strip_prefix("goa:").unwrap_or(&account.id).to_string();

    runtime::handle().block_on(async move {
        // Prefer OAuth2; fall back to the stored password for plain IMAP.
        match access_token(path).await {
            Ok(token) => Ok(Credentials::OAuth2(token)),
            Err(oauth_err) => match password(path, &goa_id).await {
                Ok(pass) => Ok(Credentials::Password(pass)),
                Err(pass_err) => Err(anyhow!(
                    "could not get credentials from GNOME Online Accounts \
                     (OAuth2: {oauth_err}; password: {pass_err})"
                )),
            },
        }
    })
}
