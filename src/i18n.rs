//! Minimal localization: Italian (the app's original language, used as the
//! lookup key — like gettext's `msgid`) and English, picked once at startup
//! from the desktop locale. No external tooling (gettext/fluent) and nothing
//! to compile or ship separately — the string set is small enough to keep
//! in one table.
//!
//! Usage: wrap a literal that used to be inline Italian with `t(...)`:
//! `.title("Preferenze")` becomes `.title(t("Preferenze"))`. A string with
//! no English entry yet just falls back to the Italian original instead of
//! showing a missing-translation placeholder.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    It,
    En,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// Decide the UI language once, from the desktop locale. Defaults to
/// Italian — this app's original language — unless the environment clearly
/// asks for English.
pub fn init() {
    let requested = std::env::var("MAILVIEW_LANG")
        .ok()
        .or_else(|| std::env::var("LANGUAGE").ok())
        .or_else(|| std::env::var("LC_ALL").ok())
        .or_else(|| std::env::var("LC_MESSAGES").ok())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default();
    let lang = if requested.to_lowercase().starts_with("en") { Lang::En } else { Lang::It };
    let _ = LANG.set(lang);
}

pub fn lang() -> Lang {
    *LANG.get().unwrap_or(&Lang::It)
}

/// Translate a string, keyed by its Italian original. In Italian mode this
/// is always the identity function; in English mode it looks the string up
/// in the table below, falling back to the Italian text if that particular
/// string has not been translated yet.
pub fn t(italian: &'static str) -> &'static str {
    translate(lang(), italian)
}

/// The pure lookup behind [`t`], taking the language explicitly so it can be
/// tested without touching the process-wide language (`LANG`/`lang()` can
/// only ever be set once, so tests can't flip it back and forth).
fn translate(lang: Lang, italian: &'static str) -> &'static str {
    match lang {
        Lang::It => italian,
        Lang::En => en_table().get(italian).copied().unwrap_or(italian),
    }
}

/// Pick the singular or plural form of a word, in whichever language is
/// active — the two languages' pairs are unrelated strings, not a
/// lookup-table pair, so this takes both directly rather than going through
/// `t`.
pub fn plural(
    count: usize,
    singular_it: &'static str,
    plural_it: &'static str,
    singular_en: &'static str,
    plural_en: &'static str,
) -> &'static str {
    let (singular, plural) = match lang() {
        Lang::It => (singular_it, plural_it),
        Lang::En => (singular_en, plural_en),
    };
    if count == 1 { singular } else { plural }
}

const WEEKDAYS_IT: [&str; 7] = ["Lun", "Mar", "Mer", "Gio", "Ven", "Sab", "Dom"];
const WEEKDAYS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS_IT: [&str; 12] =
    ["gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic"];
const MONTHS_EN: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A short weekday name in the active language — `chrono`'s own `%a` always
/// renders in English without pulling in its (large, "unstable") locale-data
/// feature, so message dates spell this out by hand instead.
pub fn weekday_abbr(weekday: chrono::Weekday) -> &'static str {
    let index = weekday.num_days_from_monday() as usize;
    match lang() {
        Lang::It => WEEKDAYS_IT[index],
        Lang::En => WEEKDAYS_EN[index],
    }
}

/// A short month name in the active language, for the same reason as
/// [`weekday_abbr`]. `month` is 1-12, as `chrono::Datelike::month` returns.
pub fn month_abbr(month: u32) -> &'static str {
    let index = month.saturating_sub(1).min(11) as usize;
    match lang() {
        Lang::It => MONTHS_IT[index],
        Lang::En => MONTHS_EN[index],
    }
}

/// Translate a template with one `{}` placeholder, then substitute it — for
/// the handful of user-facing strings built with `format!` at runtime,
/// which can't go through `t` directly since that needs a literal.
pub fn t1(template: &'static str, value: &str) -> String {
    t(template).replacen("{}", value, 1)
}

fn en_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        HashMap::from([
            // ---- sidebar / smart mailboxes -------------------------------
            ("In entrata (tutte)", "All Inboxes"),
            ("Contrassegnati", "Flagged"),
            ("Non letti", "Unread"),
            ("Caselle", "Mailboxes"),
            ("Nessun account configurato. Aggiungine uno in Impostazioni → Account online, \
              oppure avvia con --demo per esplorare l'interfaccia.",
             "No account configured. Add one in Settings → Online Accounts, \
              or start with --demo to explore the interface."),
            ("Errore di connessione", "Connection error"),

            // ---- mailbox kind labels --------------------------------------
            ("Archivio", "Archive"),
            ("Indesiderata", "Junk"),
            ("Cestino", "Trash"),
            ("In arrivo", "Inbox"),
            ("Inviata", "Sent"),
            ("Bozze", "Drafts"),
            ("Speciali", "Special"),
            ("Cartella", "Folder"),

            // ---- message list header / empty states ------------------------
            ("Posta", "Mail"),
            ("Nessun messaggio", "No messages"),
            ("Questa cartella è vuota.", "This folder is empty."),
            ("Cerca nei messaggi", "Search messages"),
            ("Caricamento…", "Loading…"),

            // ---- reading pane -----------------------------------------------
            ("Messaggio", "Message"),
            ("Nessun messaggio selezionato", "No message selected"),
            ("Scegli una conversazione dall'elenco per leggerla qui.",
             "Choose a conversation from the list to read it here."),
            ("Impossibile aprire il messaggio", "Couldn't open the message"),
            ("A: {}", "To: {}"),
            (" e altri {}", " and {} more"),
            ("Salva {}", "Save {}"),

            // ---- toolbar tooltips --------------------------------------------
            ("Menu principale", "Main menu"),
            ("Cerca (Ctrl+F)", "Search (Ctrl+F)"),
            ("Aggiorna (Ctrl+R)", "Refresh (Ctrl+R)"),
            ("Nuovo messaggio (Ctrl+N)", "New message (Ctrl+N)"),
            ("Mostra/nascondi le caselle", "Show/hide mailboxes"),
            ("Mostra solo i non letti", "Show unread only"),
            ("Rispondi (Ctrl+Invio)", "Reply (Ctrl+Enter)"),
            ("Rispondi a tutti (Ctrl+Maiusc+Invio)", "Reply all (Ctrl+Shift+Enter)"),
            ("Inoltra", "Forward"),
            ("Archivia (Ctrl+E)", "Archive (Ctrl+E)"),
            ("Segna come indesiderata", "Mark as junk"),
            ("Elimina (Canc)", "Delete (Del)"),
            ("Contrassegna", "Flag"),
            ("Forza la sincronizzazione (Ctrl+R)", "Force sync (Ctrl+R)"),

            // ---- app menu -----------------------------------------------------
            ("Automatico (sistema)", "Automatic (system)"),
            ("Chiaro", "Light"),
            ("Scuro", "Dark"),
            ("Aspetto", "Appearance"),
            ("Aggiungi account IMAP…", "Add IMAP account…"),
            ("Aggiorna", "Refresh"),
            ("Cerca", "Search"),
            ("Preferenze…", "Preferences…"),
            ("Chiudi", "Close"),

            // ---- status bar -----------------------------------------------------
            ("Pronto", "Ready"),
            ("Ultima sincronizzazione: {}", "Last synced: {}"),
            ("adesso", "just now"),
            ("1 min fa", "1 min ago"),
            ("{} min fa", "{} min ago"),
            ("un'ora fa", "an hour ago"),
            ("{} ore fa", "{} hours ago"),
            ("ieri", "yesterday"),
            ("{} giorni fa", "{} days ago"),
            ("da leggere", "unread"),
            ("{} — vuota", "{} — empty"),
            ("Azioni sul messaggio", "Message actions"),
            ("Segna come letto/da leggere", "Mark as read/unread"),
            ("Sincronizzazione — {}", "Syncing — {}"),
            ("Caricamento altri messaggi — {}", "Loading more messages — {}"),

            // ---- context menu -----------------------------------------------------
            ("Rispondi", "Reply"),
            ("Rispondi a tutti", "Reply all"),
            ("Segna come non letto", "Mark as unread"),
            ("Segna come letto", "Mark as read"),
            ("Sposta in", "Move to"),

            // ---- toasts / errors -----------------------------------------------
            ("Messaggio spostato", "Message moved"),
            ("Messaggio inviato", "Message sent"),
            ("Bozza salvata", "Draft saved"),
            ("Nessuna cartella {} su questo account", "No {} folder on this account"),
            ("Impossibile salvare la password", "Couldn't save the password"),
            ("Account {} aggiunto", "Account {} added"),
            ("Nessun account configurato", "No account configured"),
            ("Apri prima un messaggio", "Open a message first"),
            ("Salvataggio della bozza…", "Saving the draft…"),
            ("Invio in corso…", "Sending…"),
            ("Invio tra {}…", "Sending in {}…"),
            ("Annulla", "Cancel"),
            ("Aggiornamento delle cartelle…", "Updating folders…"),

            // ---- compose window -----------------------------------------------
            ("Nuovo messaggio", "New message"),
            ("Da", "From"),
            ("A", "To"),
            ("Cc", "Cc"),
            ("Ccn", "Bcc"),
            ("Oggetto", "Subject"),
            ("Invia", "Send"),
            ("Indica almeno un destinatario", "Add at least one recipient"),
            ("Controlla gli indirizzi nel campo {}", "Check the addresses in the {} field"),
            ("Salvare la bozza?", "Save the draft?"),
            ("Il messaggio non è stato inviato.", "The message hasn't been sent."),
            ("Non salvare", "Don't save"),
            ("Salva bozza", "Save draft"),

            // ---- accounts dialog -------------------------------------------------
            ("Aggiungi account", "Add account"),
            ("Nome visualizzato", "Display name"),
            ("Indirizzo e-mail", "Email address"),
            ("Password", "Password"),
            ("Identità", "Identity"),
            ("Server IMAP", "IMAP server"),
            ("Porta IMAP", "IMAP port"),
            ("Usa STARTTLS", "Use STARTTLS"),
            ("Attiva per i server sulla porta 143; lascia disattivo per la 993",
             "Turn on for servers on port 143; leave off for 993"),
            ("Posta in arrivo", "Incoming mail"),
            ("Server SMTP", "SMTP server"),
            ("Porta SMTP", "SMTP port"),
            ("Posta in uscita", "Outgoing mail"),
            ("Aggiungi", "Add"),
            ("Inserisci un indirizzo e-mail valido", "Enter a valid email address"),
            ("Indica il server IMAP", "Enter the IMAP server"),
            ("Inserisci la password", "Enter the password"),

            // ---- preferences window ------------------------------------------
            ("Display", "Display"),
            ("In uscita", "Outgoing"),
            ("Offline", "Offline"),
            ("Posta locale", "Local mail"),
            ("Informazioni", "About"),
            ("Preferenze", "Preferences"),
            ("Carica contenuti remoti", "Load remote content"),
            ("Mostra immagini remote e risorse collegate nei messaggi HTML",
             "Show remote images and linked resources in HTML messages"),
            ("Aspetto dei messaggi", "Message appearance"),
            ("Firme", "Signatures"),
            ("Account", "Account"),
            ("Invio", "Sending"),
            ("Ritardo invio", "Send delay"),
            ("Trattieni i messaggi in Posta in uscita prima dell'invio, così puoi annullarlo",
             "Hold messages in Outbox before delivery, so you can undo a send"),
            ("Scarica i corpi dei messaggi", "Download message bodies"),
            ("Per quanto tempo indietro tenere i messaggi già letti disponibili offline, per ogni account.",
             "How far back to keep already-read messages available offline, per account."),
            ("Nessun account configurato.", "No account configured."),
            ("Cache locale", "Local cache"),
            ("MailView tiene una copia locale di cartelle e messaggi per aprirli all'istante e leggerli offline.",
             "MailView keeps a local copy of folders and messages so they open instantly and can be read offline."),
            ("Posizione", "Location"),
            ("Spazio occupato", "Space used"),
            ("Svuota", "Clear"),
            ("Elimina tutti i dati scaricati", "Delete all downloaded data"),
            ("Cartelle e messaggi verranno riscaricati al bisogno",
             "Folders and messages will be re-downloaded as needed"),
            ("Versione", "Version"),

            // ---- config.rs enum labels ---------------------------------------
            ("Adatta il testo", "Adapt text"),
            ("Adatta lo sfondo", "Adapt background"),
            ("Mantieni il formato del mittente", "Keep sender's format"),
            ("Il testo resta leggibile, lo sfondo è quello del messaggio",
             "Text stays readable, the background is the message's own"),
            ("Lo sfondo segue il tema, i colori del testo restano quelli del mittente",
             "The background follows the theme, text colours stay as the sender set them"),
            ("Il messaggio viene mostrato esattamente come inviato",
             "The message is shown exactly as it was sent"),
            ("Disattivato (invio immediato)", "Off (send immediately)"),
            ("5 secondi", "5 seconds"),
            ("10 secondi", "10 seconds"),
            ("30 secondi", "30 seconds"),
            ("1 minuto", "1 minute"),
            ("2 minuti", "2 minutes"),
            ("5 minuti", "5 minutes"),
            ("Nessuno", "None"),
            ("Ultima settimana", "Last week"),
            ("Ultimo mese", "Last month"),
            ("Ultimo anno", "Last year"),
            ("Tutti i messaggi", "All messages"),
            ("(nessun oggetto)", "(no subject)"),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn italian_is_always_the_identity() {
        assert_eq!(translate(Lang::It, "Rispondi"), "Rispondi");
        // Even a string with no place in the English table at all — Italian
        // mode never touches the lookup table.
        assert_eq!(translate(Lang::It, "una frase mai tradotta"), "una frase mai tradotta");
    }

    #[test]
    fn english_looks_up_the_table() {
        assert_eq!(translate(Lang::En, "Rispondi"), "Reply");
        assert_eq!(translate(Lang::En, "Annulla"), "Cancel");
    }

    #[test]
    fn english_falls_back_to_italian_when_untranslated() {
        // A string that was wrapped in `t(...)` but has no English entry
        // yet must still show *something* readable, not a blank or a panic.
        assert_eq!(translate(Lang::En, "una frase mai tradotta"), "una frase mai tradotta");
    }

    #[test]
    fn single_placeholder_template_substitutes_after_translating() {
        assert_eq!(t1("Salva {}", "report.pdf"), "Salva report.pdf");
    }

    #[test]
    fn weekday_and_month_abbreviations_stay_in_range() {
        assert_eq!(weekday_abbr(chrono::Weekday::Mon), "Lun");
        assert_eq!(weekday_abbr(chrono::Weekday::Sun), "Dom");
        assert_eq!(month_abbr(1), "gen");
        assert_eq!(month_abbr(12), "dic");
        // Out-of-range input (should never happen from chrono itself) must
        // not panic — clamp rather than index out of bounds.
        assert_eq!(month_abbr(0), "gen");
        assert_eq!(month_abbr(13), "dic");
    }

    #[test]
    fn plural_picks_singular_only_at_exactly_one() {
        assert_eq!(plural(0, "messaggio", "messaggi", "message", "messages"), "messaggi");
        assert_eq!(plural(1, "messaggio", "messaggi", "message", "messages"), "messaggio");
        assert_eq!(plural(2, "messaggio", "messaggi", "message", "messages"), "messaggi");
    }

    #[test]
    fn every_english_translation_keeps_the_source_placeholders() {
        // A translated template that drops a `{}` the Italian original had
        // (or vice versa) would silently swallow or duplicate data at the
        // call site — catch that mismatch here instead of at runtime.
        for (it, en) in en_table() {
            let it_braces = it.matches("{}").count();
            let en_braces = en.matches("{}").count();
            assert_eq!(
                it_braces, en_braces,
                "placeholder count mismatch for {it:?} -> {en:?}"
            );
        }
    }
}
