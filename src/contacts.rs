//! Recipient suggestions for the compose window.
//!
//! Two sources, merged: addresses already seen in loaded mail (always
//! available, no setup needed) and, best effort, GNOME's local address book
//! via evolution-data-server. EDS was not installed on the machine this was
//! developed on, so that half could not be exercised against a live
//! service — it is written defensively (a hard timeout, every step
//! fallible) so a wrong assumption about its D-Bus surface just means no
//! extra suggestions, never a hang or a crash. If it does not turn up
//! contacts on a real desktop, the interface/method names below are the
//! first thing to check against `busctl --user introspect` on that machine.

use std::time::Duration;

use futures_util::StreamExt;

use crate::model::Mailaddr;
use crate::runtime;

const QUERY_TIMEOUT: Duration = Duration::from_secs(4);

/// Kick off a best-effort address book query in the background. The
/// returned future never fails and never blocks the caller beyond
/// [`QUERY_TIMEOUT`] — safe to `.await` from the GTK main loop via
/// `glib::spawn_future_local`.
pub fn from_local_address_book() -> impl std::future::Future<Output = Vec<Mailaddr>> {
    let handle = runtime::handle().spawn(async {
        match tokio::time::timeout(QUERY_TIMEOUT, query_eds()).await {
            Ok(Ok(contacts)) => contacts,
            Ok(Err(e)) => {
                log::debug!("no contacts from the local address book: {e:#}");
                Vec::new()
            }
            Err(_) => {
                log::debug!("local address book query timed out");
                Vec::new()
            }
        }
    });
    async move { handle.await.unwrap_or_default() }
}

async fn query_eds() -> anyhow::Result<Vec<Mailaddr>> {
    let connection = zbus::Connection::session().await?;

    let service = eds_service_name(&connection)
        .await
        .ok_or_else(|| anyhow::anyhow!("evolution-data-server is not running"))?;

    let (book_path,): (zbus::zvariant::OwnedObjectPath,) = connection
        .call_method(
            Some(service.as_str()),
            "/org/gnome/evolution/dataserver/AddressBookFactory",
            Some("org.gnome.evolution.dataserver.AddressBookFactory"),
            "OpenAddressBook",
            &("system-address-book",),
        )
        .await?
        .body()
        .deserialize()?;

    connection
        .call_method(
            Some(service.as_str()),
            &book_path,
            Some("org.gnome.evolution.dataserver.AddressBook"),
            "Open",
            &(),
        )
        .await?;

    let (view_path,): (zbus::zvariant::OwnedObjectPath,) = connection
        .call_method(
            Some(service.as_str()),
            &book_path,
            Some("org.gnome.evolution.dataserver.AddressBook"),
            "GetView",
            &("(exists-vcard-field \"email\")",),
        )
        .await?
        .body()
        .deserialize()?;

    // Signals only start flowing once we are subscribed, so the match rule
    // has to be in place before Start() — otherwise the first batch (often
    // the only one for a small local address book) can race past us.
    let rule = format!(
        "type='signal',path='{}',interface='org.gnome.evolution.dataserver.AddressBookView',member='ObjectsAdded'",
        view_path.as_str()
    );
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &(rule,),
        )
        .await?;

    let mut stream = zbus::MessageStream::from(&connection);

    connection
        .call_method(
            Some(service.as_str()),
            &view_path,
            Some("org.gnome.evolution.dataserver.AddressBookView"),
            "Start",
            &(),
        )
        .await?;

    let mut contacts = Vec::new();
    // A local address book answers almost instantly; give it a couple of
    // seconds inside the overall query timeout and take whatever arrived.
    let collect = async {
        while let Some(Ok(message)) = stream.next().await {
            let header = message.header();
            if header.path().map(|p| p.as_str()) != Some(view_path.as_str())
                || header.member().map(|m| m.as_str()) != Some("ObjectsAdded")
            {
                continue;
            }
            if let Ok((vcards,)) = message.body().deserialize::<(Vec<String>,)>() {
                contacts.extend(vcards.iter().filter_map(|v| contact_from_vcard(v)));
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), collect).await;

    let _ = connection
        .call_method(
            Some(service.as_str()),
            &view_path,
            Some("org.gnome.evolution.dataserver.AddressBookView"),
            "Stop",
            &(),
        )
        .await;

    Ok(contacts)
}

/// Find whichever versioned `org.gnome.evolution.dataserver.AddressBookN`
/// name is actually available — EDS bumps this suffix across releases, but
/// the interfaces reached through it have stayed stable for a long time.
async fn eds_service_name(connection: &zbus::Connection) -> Option<String> {
    let dbus = zbus::fdo::DBusProxy::new(connection).await.ok()?;
    let mut names: Vec<String> =
        dbus.list_activatable_names().await.ok()?.iter().map(|n| n.to_string()).collect();
    if let Ok(running) = dbus.list_names().await {
        names.extend(running.iter().map(|n| n.to_string()));
    }
    names.into_iter().find(|n| n.starts_with("org.gnome.evolution.dataserver.AddressBook"))
}

/// Pull a name and the first email address out of a vCard. Good enough for
/// suggestions; not a full vCard parser.
fn contact_from_vcard(vcard: &str) -> Option<Mailaddr> {
    let mut name = String::new();
    let mut address = String::new();
    for line in vcard.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.split(';').next().unwrap_or(key);
        match key {
            "FN" => name = value.trim().to_string(),
            "EMAIL" if address.is_empty() => address = value.trim().to_string(),
            _ => {}
        }
    }
    if address.is_empty() {
        None
    } else {
        Some(Mailaddr::new(name, address))
    }
}
