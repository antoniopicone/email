//! Password storage for manually configured accounts.
//!
//! Passwords go to the desktop keyring through the Secret Service API, the
//! same store GNOME Keyring and KWallet expose, so they are never written to
//! our config file. Accounts that come from GNOME Online Accounts do not use
//! this at all — GOA hands us credentials directly.

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::runtime;

const ATTR_APP: &str = "application";
const ATTR_ACCOUNT: &str = "account-id";
const APP_ID: &str = "it.antoniopicone.MailView";

fn attributes(account_id: &str) -> HashMap<&str, &str> {
    HashMap::from([(ATTR_APP, APP_ID), (ATTR_ACCOUNT, account_id)])
}

/// Store (or replace) the IMAP password for an account.
pub async fn store_password(account_id: &str, label: &str, password: &str) -> Result<()> {
    let keyring = oo7::Keyring::new().await.context("opening the desktop keyring")?;
    keyring
        .create_item(label, &attributes(account_id), password.as_bytes(), true)
        .await
        .with_context(|| format!("storing the password for {account_id}"))?;
    Ok(())
}

/// Look up a stored password. Returns `None` when nothing is saved yet.
pub async fn lookup_password(account_id: &str) -> Result<Option<String>> {
    let keyring = oo7::Keyring::new().await.context("opening the desktop keyring")?;
    let items = keyring
        .search_items(&attributes(account_id))
        .await
        .with_context(|| format!("searching the keyring for {account_id}"))?;

    let Some(item) = items.first() else { return Ok(None) };
    let secret = item.secret().await.context("reading the stored secret")?;
    Ok(Some(String::from_utf8_lossy(&secret).into_owned()))
}


pub fn store_password_blocking(account_id: &str, label: &str, password: &str) -> Result<()> {
    runtime::handle().block_on(store_password(account_id, label, password))
}

pub fn lookup_password_blocking(account_id: &str) -> Result<Option<String>> {
    runtime::handle().block_on(lookup_password(account_id))
}

