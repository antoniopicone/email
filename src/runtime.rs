//! A process-wide Tokio runtime.
//!
//! The UI runs on the GLib main loop and the IMAP workers are synchronous, but
//! zbus needs an async executor to talk to GNOME Online Accounts. Rather than
//! spinning up a runtime per call, we keep one multi-threaded runtime around
//! and block on it from whichever thread needs D-Bus.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// The shared runtime, created on first use.
pub fn handle() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("mailview-dbus")
            .build()
            .expect("failed to start the Tokio runtime")
    })
}
