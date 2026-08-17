//! The "add an IMAP account" dialog.
//!
//! Accounts that already exist in GNOME Online Accounts do not go through
//! here — they are discovered automatically. This is for servers GNOME does
//! not know about. The password goes to the desktop keyring; only the
//! non-secret settings are written to our config file.

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

use crate::config::ManualAccount;

/// Guess sensible server settings from the address' domain, the way most mail
/// clients do, so the common cases need almost no typing.
fn guess_servers(email: &str) -> Option<(String, String)> {
    let domain = email.split('@').nth(1)?.trim().to_ascii_lowercase();
    if domain.is_empty() {
        return None;
    }
    let known = match domain.as_str() {
        "gmail.com" | "googlemail.com" => Some(("imap.gmail.com", "smtp.gmail.com")),
        "outlook.com" | "hotmail.com" | "live.com" => {
            Some(("outlook.office365.com", "smtp.office365.com"))
        }
        "yahoo.com" => Some(("imap.mail.yahoo.com", "smtp.mail.yahoo.com")),
        "icloud.com" | "me.com" => Some(("imap.mail.me.com", "smtp.mail.me.com")),
        _ => None,
    };
    match known {
        Some((imap, smtp)) => Some((imap.to_string(), smtp.to_string())),
        None => Some((format!("imap.{domain}"), format!("smtp.{domain}"))),
    }
}

fn entry_row(title: &str, text: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).build();
    row.set_text(text);
    row
}

/// Open the dialog. `on_save` receives the account and its password.
pub fn open<F>(parent: &impl IsA<gtk::Window>, on_save: F)
where
    F: Fn(ManualAccount, String) + 'static,
{
    let window = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("Aggiungi account")
        .default_width(520)
        .default_height(640)
        .build();

    let name_row = entry_row("Nome visualizzato", "");
    let email_row = entry_row("Indirizzo e-mail", "");
    let password_row = adw::PasswordEntryRow::builder().title("Password").build();

    let identity = adw::PreferencesGroup::builder().title("Identità").build();
    identity.add(&name_row);
    identity.add(&email_row);
    identity.add(&password_row);

    let imap_host_row = entry_row("Server IMAP", "");
    let imap_port_row = entry_row("Porta IMAP", "993");
    let starttls_row = adw::SwitchRow::builder()
        .title("Usa STARTTLS")
        .subtitle("Attiva per i server sulla porta 143; lascia disattivo per la 993")
        .build();

    let incoming = adw::PreferencesGroup::builder().title("Posta in arrivo").build();
    incoming.add(&imap_host_row);
    incoming.add(&imap_port_row);
    incoming.add(&starttls_row);

    let smtp_host_row = entry_row("Server SMTP", "");
    let smtp_port_row = entry_row("Porta SMTP", "587");

    let outgoing = adw::PreferencesGroup::builder().title("Posta in uscita").build();
    outgoing.add(&smtp_host_row);
    outgoing.add(&smtp_port_row);

    // Fill the server fields in as soon as the address looks complete.
    {
        let imap_host_row = imap_host_row.clone();
        let smtp_host_row = smtp_host_row.clone();
        email_row.connect_changed(move |entry| {
            let Some((imap, smtp)) = guess_servers(&entry.text()) else { return };
            if imap_host_row.text().is_empty() || imap_host_row.text().starts_with("imap.") {
                imap_host_row.set_text(&imap);
            }
            if smtp_host_row.text().is_empty() || smtp_host_row.text().starts_with("smtp.") {
                smtp_host_row.set_text(&smtp);
            }
        });
    }

    let save_button = gtk::Button::builder().label("Aggiungi").build();
    save_button.add_css_class("suggested-action");
    let cancel_button = gtk::Button::builder().label("Annulla").build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Aggiungi account", "IMAP")));
    header.pack_start(&cancel_button);
    header.pack_end(&save_button);

    let page = adw::PreferencesPage::new();
    page.add(&identity);
    page.add(&incoming);
    page.add(&outgoing);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&toolbar));
    window.set_content(Some(&toast_overlay));

    {
        let window = window.clone();
        cancel_button.connect_clicked(move |_| window.close());
    }

    {
        let window = window.clone();
        let toast_overlay = toast_overlay.clone();
        let (name_row, email_row, password_row) =
            (name_row.clone(), email_row.clone(), password_row.clone());
        let (imap_host_row, imap_port_row, starttls_row) =
            (imap_host_row.clone(), imap_port_row.clone(), starttls_row.clone());
        let (smtp_host_row, smtp_port_row) = (smtp_host_row.clone(), smtp_port_row.clone());

        save_button.connect_clicked(move |_| {
            let email = email_row.text().trim().to_string();
            let password = password_row.text().to_string();
            let imap_host = imap_host_row.text().trim().to_string();

            let complain = |message: &str| {
                toast_overlay.add_toast(adw::Toast::new(message));
            };

            if !email.contains('@') {
                complain("Inserisci un indirizzo e-mail valido");
                return;
            }
            if imap_host.is_empty() {
                complain("Indica il server IMAP");
                return;
            }
            if password.is_empty() {
                complain("Inserisci la password");
                return;
            }

            let imap_port = imap_port_row.text().trim().parse::<u16>().unwrap_or(993);
            let smtp_port = smtp_port_row.text().trim().parse::<u16>().unwrap_or(587);

            let account = ManualAccount {
                // Stable id derived from the address, so re-adding the same
                // account replaces it instead of duplicating it.
                id: format!("imap:{email}"),
                display_name: name_row.text().trim().to_string(),
                email: email.clone(),
                imap_host,
                imap_port,
                imap_user: email.clone(),
                use_starttls: starttls_row.is_active(),
                smtp_host: smtp_host_row.text().trim().to_string(),
                smtp_port,
                smtp_user: email,
            };

            on_save(account, password);
            window.close();
        });
    }

    window.present();
    email_row.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_well_known_providers() {
        let (imap, smtp) = guess_servers("tizio@gmail.com").unwrap();
        assert_eq!(imap, "imap.gmail.com");
        assert_eq!(smtp, "smtp.gmail.com");
    }

    #[test]
    fn falls_back_to_the_domain() {
        let (imap, smtp) = guess_servers("info@antoniopicone.it").unwrap();
        assert_eq!(imap, "imap.antoniopicone.it");
        assert_eq!(smtp, "smtp.antoniopicone.it");
    }

    #[test]
    fn rejects_addresses_without_a_domain() {
        assert!(guess_servers("non-un-indirizzo").is_none());
    }
}
