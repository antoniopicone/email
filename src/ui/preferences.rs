//! The Preferences window: how messages are displayed, how mail goes out,
//! how much is kept for offline reading, and the local cache.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::config::{Config, MessageAppearance, OfflineWindow, SendDelay};
use crate::i18n::t;
use crate::model::Account;

fn save(config: &Config) {
    if let Err(e) = config.save() {
        log::warn!("could not save the preferences: {e:#}");
    }
}

/// Open the Preferences window. `on_display_change` fires whenever a setting
/// that affects the currently open message's rendering changes (remote
/// content, appearance), so the caller can re-render it live.
pub fn open(
    parent: &impl IsA<gtk::Window>,
    config: Rc<RefCell<Config>>,
    accounts: Vec<Account>,
    on_display_change: impl Fn() + 'static,
) {
    let on_display_change: Rc<dyn Fn()> = Rc::new(on_display_change);

    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(
        &build_display_page(&config, &on_display_change),
        Some("display"),
        t("Display"),
        "mail-unread-symbolic",
    );
    stack.add_titled_with_icon(
        &build_outgoing_page(&config, &accounts),
        Some("outgoing"),
        t("In uscita"),
        "mailview-sent-symbolic",
    );
    stack.add_titled_with_icon(
        &build_offline_page(&config, &accounts),
        Some("offline"),
        t("Offline"),
        "network-offline-symbolic",
    );
    stack.add_titled_with_icon(
        &build_local_mail_page(),
        Some("local"),
        t("Posta locale"),
        "drive-harddisk-symbolic",
    );
    stack.add_titled_with_icon(
        &build_about_page(),
        Some("about"),
        t("Informazioni"),
        "help-about-symbolic",
    );

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&switcher));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));

    let window = adw::Window::builder()
        .transient_for(parent)
        .modal(false)
        .title(t("Preferenze"))
        .default_width(720)
        .default_height(560)
        .build();
    window.set_content(Some(&toolbar));
    window.present();
}

// ------------------------------------------------------------------ display

fn build_display_page(config: &Rc<RefCell<Config>>, on_change: &Rc<dyn Fn()>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();

    let remote_row = adw::SwitchRow::builder()
        .title(t("Carica contenuti remoti"))
        .subtitle(t("Mostra immagini remote e risorse collegate nei messaggi HTML"))
        .active(config.borrow().load_remote_content)
        .build();
    {
        let config = config.clone();
        let on_change = on_change.clone();
        remote_row.connect_active_notify(move |row| {
            {
                let mut config = config.borrow_mut();
                config.load_remote_content = row.is_active();
                save(&config);
            }
            on_change();
        });
    }
    group.add(&remote_row);

    let appearance_row = adw::ComboRow::builder().title(t("Aspetto dei messaggi")).build();
    let appearance_labels: [&str; 3] = MessageAppearance::ALL.map(|a| a.label());
    appearance_row.set_model(Some(&gtk::StringList::new(&appearance_labels)));
    let current_appearance = config.borrow().message_appearance;
    appearance_row.set_selected(
        MessageAppearance::ALL.iter().position(|a| *a == current_appearance).unwrap_or(0) as u32,
    );
    appearance_row.set_subtitle(current_appearance.subtitle());
    {
        let config = config.clone();
        let on_change = on_change.clone();
        appearance_row.connect_selected_notify(move |row| {
            let Some(chosen) = MessageAppearance::ALL.get(row.selected() as usize) else { return };
            row.set_subtitle(chosen.subtitle());
            {
                let mut config = config.borrow_mut();
                config.message_appearance = *chosen;
                save(&config);
            }
            on_change();
        });
    }
    group.add(&appearance_row);

    page.add(&group);
    page
}

// ----------------------------------------------------------------- outgoing

fn build_outgoing_page(config: &Rc<RefCell<Config>>, accounts: &[Account]) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let accounts = Rc::new(accounts.to_vec());

    // ---- signatures ----------------------------------------------------
    let sig_group = adw::PreferencesGroup::new();
    sig_group.set_title(t("Firme"));

    let account_row = adw::ComboRow::builder().title(t("Account")).build();
    let account_labels: Vec<&str> = accounts.iter().map(|a| a.email.as_str()).collect();
    account_row.set_model(Some(&gtk::StringList::new(&account_labels)));
    sig_group.add(&account_row);

    let sig_view = gtk::TextView::new();
    sig_view.set_wrap_mode(gtk::WrapMode::WordChar);
    sig_view.set_top_margin(10);
    sig_view.set_bottom_margin(10);
    sig_view.set_left_margin(10);
    sig_view.set_right_margin(10);
    if let Some(account) = accounts.first() {
        sig_view.buffer().set_text(&config.borrow().signature(&account.id));
    }

    let sig_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(120)
        .child(&sig_view)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    sig_scroll.add_css_class("card");
    sig_scroll.set_sensitive(!accounts.is_empty());
    sig_group.add(&sig_scroll);

    {
        let config = config.clone();
        let accounts = accounts.clone();
        let sig_view = sig_view.clone();
        account_row.connect_selected_notify(move |row| {
            if let Some(account) = accounts.get(row.selected() as usize) {
                sig_view.buffer().set_text(&config.borrow().signature(&account.id));
            }
        });
    }
    {
        let config = config.clone();
        let accounts = accounts.clone();
        let account_row = account_row.clone();
        sig_view.buffer().connect_changed(move |buffer| {
            let Some(account) = accounts.get(account_row.selected() as usize) else { return };
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            let mut config = config.borrow_mut();
            config.signatures.insert(account.id.clone(), text);
            save(&config);
        });
    }

    // ---- sending ---------------------------------------------------------
    let sending_group = adw::PreferencesGroup::new();
    sending_group.set_title(t("Invio"));

    let delay_row = adw::ComboRow::builder()
        .title(t("Ritardo invio"))
        .subtitle(t("Trattieni i messaggi in Posta in uscita prima dell'invio, così puoi annullarlo"))
        .build();
    let delay_labels: [&str; 7] = SendDelay::ALL.map(|d| d.label());
    delay_row.set_model(Some(&gtk::StringList::new(&delay_labels)));
    let current_delay = config.borrow().send_delay;
    delay_row.set_selected(SendDelay::ALL.iter().position(|d| *d == current_delay).unwrap_or(0) as u32);
    {
        let config = config.clone();
        delay_row.connect_selected_notify(move |row| {
            let Some(chosen) = SendDelay::ALL.get(row.selected() as usize) else { return };
            let mut config = config.borrow_mut();
            config.send_delay = *chosen;
            save(&config);
        });
    }
    sending_group.add(&delay_row);

    page.add(&sig_group);
    page.add(&sending_group);
    page
}

// ------------------------------------------------------------------ offline

fn build_offline_page(config: &Rc<RefCell<Config>>, accounts: &[Account]) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title(t("Scarica i corpi dei messaggi"));
    group.set_description(Some(
        t("Per quanto tempo indietro tenere i messaggi già letti disponibili offline, per ogni account."),
    ));

    for account in accounts {
        let row = adw::ComboRow::builder().title(&account.email).build();
        let labels: [&str; 5] = OfflineWindow::ALL.map(|w| w.label());
        row.set_model(Some(&gtk::StringList::new(&labels)));
        let current = config.borrow().offline_window(&account.id);
        row.set_selected(OfflineWindow::ALL.iter().position(|w| *w == current).unwrap_or(2) as u32);
        {
            let config = config.clone();
            let account_id = account.id.clone();
            row.connect_selected_notify(move |row| {
                let Some(chosen) = OfflineWindow::ALL.get(row.selected() as usize) else { return };
                {
                    let mut config = config.borrow_mut();
                    config.offline_windows.insert(account_id.clone(), *chosen);
                    save(&config);
                }
                crate::cache::prune_messages_older_than(&account_id, chosen.cutoff());
            });
        }
        group.add(&row);
    }

    if accounts.is_empty() {
        group.set_description(Some(t("Nessun account configurato.")));
    }

    page.add(&group);
    page
}

// --------------------------------------------------------------- local mail

fn build_local_mail_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.set_title(t("Cache locale"));
    group.set_description(Some(
        t("MailView tiene una copia locale di cartelle e messaggi per aprirli all'istante e leggerli offline."),
    ));

    let location_row = adw::ActionRow::builder()
        .title(t("Posizione"))
        .subtitle(crate::cache::location().display().to_string())
        .build();
    group.add(&location_row);

    let size_label = gtk::Label::new(Some(&human_size(crate::cache::disk_usage())));
    size_label.add_css_class("dim-label");
    size_label.set_valign(gtk::Align::Center);
    let size_row = adw::ActionRow::builder().title(t("Spazio occupato")).build();
    size_row.add_suffix(&size_label);
    group.add(&size_row);

    let clear_button = gtk::Button::builder()
        .label(t("Svuota"))
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    let clear_row = adw::ActionRow::builder()
        .title(t("Elimina tutti i dati scaricati"))
        .subtitle(t("Cartelle e messaggi verranno riscaricati al bisogno"))
        .build();
    clear_row.add_suffix(&clear_button);
    clear_row.set_activatable_widget(Some(&clear_button));
    {
        let size_label = size_label.clone();
        clear_button.connect_clicked(move |_| {
            if let Err(e) = crate::cache::clear_all() {
                log::warn!("could not clear the local cache: {e:#}");
            }
            size_label.set_text(&human_size(crate::cache::disk_usage()));
        });
    }
    group.add(&clear_row);

    page.add(&group);
    page
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

// ------------------------------------------------------------------- about

fn build_about_page() -> adw::StatusPage {
    let status = adw::StatusPage::builder()
        .icon_name("it.antoniopicone.MailView")
        .title("MailView")
        .description(format!(
            "{} {}\n{}",
            t("Versione"),
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_DESCRIPTION")
        ))
        .build();
    status
}
