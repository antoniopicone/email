//! MailView — a GTK4/libadwaita mail client for the GNOME desktop.

mod backend;
mod config;
mod demo;
mod goa;
mod html;
mod model;
mod runtime;
mod secrets;
mod ui;

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;

use config::{Config, ThemePreference};
use model::Account;

/// Command line options. Kept deliberately small.
struct Options {
    /// Show the built-in sample mailbox instead of talking to a server.
    demo: bool,
    /// Skip GNOME Online Accounts discovery.
    no_gnome: bool,
}

fn parse_options() -> Options {
    let mut options = Options { demo: false, no_gnome: false };
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--demo" => options.demo = true,
            "--no-gnome" | "--no-goa" => options.no_gnome = true,
            "--help" | "-h" => {
                println!(
                    "MailView — client di posta per GNOME\n\n\
                     Uso: mailview [OPZIONI]\n\n\
                     Opzioni:\n  \
                     --demo       usa la casella dimostrativa, senza connessioni di rete\n  \
                     --no-gnome   non leggere gli account da GNOME Online Accounts\n  \
                     -h, --help   mostra questo messaggio\n"
                );
                std::process::exit(0);
            }
            other => eprintln!("opzione sconosciuta ignorata: {other}"),
        }
    }
    options
}

/// Collect the accounts to show: GNOME Online Accounts first, then any the
/// user configured by hand.
fn gather_accounts(options: &Options, config: &Config) -> Vec<Account> {
    if options.demo {
        log::info!("starting in demo mode");
        return demo::accounts();
    }

    let mut accounts = Vec::new();

    if config.use_gnome_online_accounts && !options.no_gnome {
        match goa::discover_blocking() {
            Ok(found) => {
                for account in found {
                    log::info!(
                        "GNOME account: {} — provider {} ({}), identity {}, auth {:?}, via {}",
                        account.email,
                        account.provider_name,
                        account.provider_type,
                        account.presentation_identity,
                        account.auth,
                        account.imap_host
                    );
                    accounts.push(account.into_account());
                }
            }
            Err(e) => log::warn!("GNOME Online Accounts lookup failed: {e:#}"),
        }
    }

    for manual in &config.accounts {
        accounts.push(manual.to_account());
    }

    accounts
}

/// Honour `MAILVIEW_FORCE_SCHEME` so the screenshot tooling can pin the theme
/// on a display that has no desktop settings to follow.
fn initial_theme(config: &Config) -> ThemePreference {
    match std::env::var("MAILVIEW_FORCE_SCHEME") {
        Ok(value) if !value.is_empty() => ThemePreference::from_str_lossy(&value),
        _ => config.theme,
    }
}

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("mailview=info,warn"),
    )
    .init();

    let options = parse_options();
    let config = Config::load();
    let accounts = gather_accounts(&options, &config);

    if accounts.is_empty() && !options.demo {
        log::warn!(
            "no accounts found; add one in GNOME Settings → Online Accounts, \
             or run with --demo"
        );
    }

    let application = adw::Application::builder()
        .application_id(ui::app_id())
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let theme = initial_theme(&config);

    application.connect_startup(move |_| {
        ui::load_css();
        ui::apply_theme(theme);
    });

    application.connect_activate(move |application| {
        let app = ui::App::build(application, accounts.clone(), config.clone());
        app.present();
        // Keep the controller alive for the lifetime of the window.
        std::mem::forget(app);
    });

    // GTK would otherwise try to parse our own flags.
    application.run_with_args::<&str>(&[])
}

use gtk::glib;
