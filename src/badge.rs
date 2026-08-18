//! Unread-count badge on the taskbar/dock icon.
//!
//! GTK4 has no in-process API left for drawing on a window's launcher icon —
//! that is entirely up to the desktop shell, keyed off the application's
//! `.desktop` file. The de-facto standard several shells and docks
//! (Budgie, Cinnamon, Dash to Dock, …) use to receive a badge count is a
//! broadcast signal on the session bus, `com.canonical.Unity.LauncherEntry`,
//! first introduced by Unity and copied since by everyone else who wanted
//! badges without inventing their own protocol. A shell that does not listen
//! for it just never sees the signal — there is nothing to detect or fall
//! back on.

use std::collections::HashMap;

use zbus::zvariant::Value;

use crate::runtime;

/// Must match the `Icon=`/filename of the installed `.desktop` file exactly,
/// `.desktop` suffix included — it is how a listening shell matches the
/// signal back to a running application.
const APP_URI: &str = "application://it.antoniopicone.MailView.desktop";

/// Update (or clear, at `count == 0`) the unread badge. Fire-and-forget: a
/// desktop with no listener for this signal is the common case, not an
/// error worth surfacing to the user.
pub fn set_unread_count(count: u32) {
    runtime::handle().spawn(async move {
        if let Err(e) = update(count).await {
            log::debug!("could not update the launcher badge: {e:#}");
        }
    });
}

async fn update(count: u32) -> anyhow::Result<()> {
    let connection = zbus::Connection::session().await?;

    let mut properties: HashMap<&str, Value> = HashMap::new();
    properties.insert("count", Value::from(i64::from(count)));
    properties.insert("count-visible", Value::from(count > 0));

    connection
        .emit_signal(
            None::<&str>,
            "/com/canonical/unity/launcherentry/mailview",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(APP_URI, properties),
        )
        .await?;
    Ok(())
}
