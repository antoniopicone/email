//! A self-contained sample mailbox.
//!
//! Run with `--demo` to explore the interface without configuring an account.
//! The demo worker answers the same [`Command`]s as the IMAP worker but never
//! opens a socket, which also makes it what the UI tests and the screenshot
//! tooling drive.

use std::sync::mpsc;

use chrono::{Duration, Local, TimeZone};

use crate::backend::{Command, ConnectionState, Event};
use crate::model::{
    Account, AccountSource, Mailaddr, Mailbox, MailboxKind, Message, MessageSummary,
};

pub const DEMO_ACCOUNT_ID: &str = "demo:gmail";
pub const DEMO_ACCOUNT_ID_WORK: &str = "demo:work";

/// The two accounts the demo shows in the sidebar.
pub fn accounts() -> Vec<Account> {
    vec![
        Account {
            id: DEMO_ACCOUNT_ID.into(),
            display_name: "Antonio Picone".into(),
            email: "antonio.picone@gmail.com".into(),
            imap_host: "imap.gmail.com".into(),
            imap_port: 993,
            imap_user: "antonio.picone@gmail.com".into(),
            use_starttls: false,
            smtp_host: "smtp.gmail.com".into(),
            smtp_port: 587,
            smtp_user: "antonio.picone@gmail.com".into(),
            source: AccountSource::Demo,
            goa_path: Some("/org/gnome/OnlineAccounts/accounts/account_demo".into()),
        },
        Account {
            id: DEMO_ACCOUNT_ID_WORK.into(),
            display_name: "Lavoro".into(),
            email: "info@antoniopicone.it".into(),
            imap_host: "imap.antoniopicone.it".into(),
            imap_port: 993,
            imap_user: "info@antoniopicone.it".into(),
            use_starttls: false,
            smtp_host: "smtp.antoniopicone.it".into(),
            smtp_port: 587,
            smtp_user: "info@antoniopicone.it".into(),
            source: AccountSource::Demo,
            goa_path: None,
        },
    ]
}

fn mailboxes_for(account_id: &str) -> Vec<Mailbox> {
    let gmail = account_id == DEMO_ACCOUNT_ID;
    let spec: &[(&str, &str, MailboxKind, u32, u32)] = if gmail {
        &[
            ("INBOX", "In arrivo", MailboxKind::Inbox, 3, 128),
            ("[Gmail]/Bozze", "Bozze", MailboxKind::Drafts, 0, 2),
            ("[Gmail]/Posta inviata", "Inviata", MailboxKind::Sent, 0, 431),
            ("[Gmail]/Tutti i messaggi", "Archivio", MailboxKind::Archive, 0, 2874),
            ("[Gmail]/Spam", "Indesiderata", MailboxKind::Junk, 2, 17),
            ("[Gmail]/Cestino", "Cestino", MailboxKind::Trash, 0, 9),
            ("Ricevute", "Ricevute", MailboxKind::Other, 0, 64),
            ("Viaggi", "Viaggi", MailboxKind::Other, 1, 23),
        ]
    } else {
        &[
            ("INBOX", "In arrivo", MailboxKind::Inbox, 2, 57),
            ("Drafts", "Bozze", MailboxKind::Drafts, 0, 1),
            ("Sent", "Inviata", MailboxKind::Sent, 0, 212),
            ("Archive", "Archivio", MailboxKind::Archive, 0, 940),
            ("Junk", "Indesiderata", MailboxKind::Junk, 0, 4),
            ("Trash", "Cestino", MailboxKind::Trash, 0, 3),
            ("Clienti", "Clienti", MailboxKind::Other, 1, 88),
        ]
    };

    spec.iter()
        .map(|(path, name, kind, unread, total)| Mailbox {
            account_id: account_id.to_string(),
            path: (*path).to_string(),
            name: (*name).to_string(),
            kind: *kind,
            unread: *unread,
            total: *total,
        })
        .collect()
}

struct Sample {
    from: (&'static str, &'static str),
    subject: &'static str,
    snippet: &'static str,
    body_html: &'static str,
    minutes_ago: i64,
    seen: bool,
    flagged: bool,
    answered: bool,
    attachments: bool,
}

fn samples(account_id: &str) -> Vec<Sample> {
    if account_id == DEMO_ACCOUNT_ID {
        vec![
            Sample {
                from: ("Giulia Ferrari", "giulia.ferrari@studioferrari.it"),
                subject: "Revisione del contratto — versione 3",
                snippet: "Ciao Antonio, ti allego la terza revisione con le modifiche che abbiamo discusso giovedì. Ho evidenziato in giallo le clausole…",
                body_html: r#"<p>Ciao Antonio,</p>
<p>ti allego la <strong>terza revisione</strong> del contratto con le modifiche che abbiamo discusso giovedì. Ho evidenziato le clausole che secondo me vanno ancora limate:</p>
<ul>
<li><b>Art. 4</b> — la penale per ritardo mi sembra sproporzionata rispetto al valore complessivo.</li>
<li><b>Art. 7</b> — manca del tutto la clausola di riservatezza reciproca.</li>
<li><b>Allegato B</b> — i tempi di consegna vanno riallineati alle nuove date.</li>
</ul>
<p>Se riesci a darmi un riscontro entro <b>mercoledì</b> facciamo in tempo a chiudere prima della pausa estiva.</p>
<blockquote>Ricordati anche di firmare digitalmente la lettera di incarico, altrimenti non posso protocollarla.</blockquote>
<p>Grazie mille,<br>Giulia</p>
<p style="color:#888;font-size:12px">Studio Legale Ferrari &amp; Associati — Via Roma 12, Milano</p>"#,
                minutes_ago: 24,
                seen: false,
                flagged: true,
                answered: false,
                attachments: true,
            },
            Sample {
                from: ("GitHub", "notifications@github.com"),
                subject: "[antoniopicone/email] La build è tornata verde (#412)",
                snippet: "The workflow run for commit a1b2c3d completed successfully. All 47 checks passed on branch main.",
                body_html: r#"<p>La pipeline <code>CI / build-and-test</code> è tornata verde.</p>
<table>
<tr><td><b>Commit</b></td><td><code>a1b2c3d</code></td></tr>
<tr><td><b>Branch</b></td><td>main</td></tr>
<tr><td><b>Durata</b></td><td>4m 12s</td></tr>
<tr><td><b>Check</b></td><td>47 superati, 0 falliti</td></tr>
</table>
<p><a href="https://github.com/antoniopicone/email/actions">Apri il run su GitHub</a></p>"#,
                minutes_ago: 96,
                seen: false,
                flagged: false,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Marco Bianchi", "m.bianchi@nordwind.dev"),
                subject: "Re: Architettura del nuovo servizio di sync",
                snippet: "Concordo sulla scelta di tenere il worker separato dal processo principale. Un dubbio però sulla gestione dei token…",
                body_html: r#"<p>Concordo sulla scelta di tenere il worker separato dal processo principale: ci evita di bloccare il main loop durante le fetch lunghe.</p>
<p>Un dubbio però sulla gestione dei token OAuth: se il refresh avviene dentro il worker, cosa succede se due worker provano a rinnovare lo stesso token nello stesso momento?</p>
<blockquote>
<p>&gt; Il piano è delegare tutto a GNOME Online Accounts, che fa già da<br>
&gt; serializzatore centrale per il refresh.</p>
</blockquote>
<p>Ah, se GOA fa da arbitro allora il problema non si pone. Perfetto.</p>
<p>Ci sentiamo lunedì,<br>Marco</p>"#,
                minutes_ago: 210,
                seen: true,
                flagged: false,
                answered: true,
                attachments: false,
            },
            Sample {
                from: ("Trenitalia", "noreply@trenitalia.it"),
                subject: "Il tuo biglietto per Milano Centrale → Roma Termini",
                snippet: "Gentile Cliente, di seguito il riepilogo del tuo viaggio di martedì 19 agosto. Frecciarossa 9520, carrozza 7, posto 12A.",
                body_html: r#"<p>Gentile Cliente,</p>
<p>di seguito il riepilogo del tuo viaggio:</p>
<table>
<tr><td><b>Treno</b></td><td>Frecciarossa 9520</td></tr>
<tr><td><b>Data</b></td><td>martedì 19 agosto</td></tr>
<tr><td><b>Partenza</b></td><td>Milano Centrale — 07:10</td></tr>
<tr><td><b>Arrivo</b></td><td>Roma Termini — 10:05</td></tr>
<tr><td><b>Posto</b></td><td>Carrozza 7, posto 12A</td></tr>
<tr><td><b>PNR</b></td><td>X7K2M9</td></tr>
</table>
<p>Il biglietto in PDF è in allegato. Buon viaggio!</p>"#,
                minutes_ago: 380,
                seen: true,
                flagged: false,
                answered: false,
                attachments: true,
            },
            Sample {
                from: ("Sara Conti", "sara@designcollettivo.it"),
                subject: "Mockup della nuova home — feedback?",
                snippet: "Ho caricato su Figma le tre varianti. La prima è quella più vicina al brief, la terza è un esperimento un po' più audace…",
                body_html: r#"<p>Ciao!</p>
<p>Ho caricato su Figma le tre varianti della home:</p>
<ol>
<li><b>Variante A</b> — la più vicina al brief, griglia a 12 colonne, hero statico.</li>
<li><b>Variante B</b> — stessa struttura ma con hero animato e palette più calda.</li>
<li><b>Variante C</b> — un esperimento più audace, tipografia grande e niente immagini.</li>
</ol>
<p>Personalmente punterei sulla <b>C</b>, ma capisco che sia una scelta di rottura. Fammi sapere che ne pensi entro venerdì così procedo con il prototipo navigabile.</p>
<p>Sara</p>"#,
                minutes_ago: 1_500,
                seen: false,
                flagged: false,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Banca Sella", "comunicazioni@sella.it"),
                subject: "Estratto conto di luglio disponibile",
                snippet: "Il documento è consultabile nell'area riservata. Per motivi di sicurezza non alleghiamo mai file ai messaggi di posta.",
                body_html: r#"<p>Gentile Cliente,</p>
<p>l'estratto conto del mese di <b>luglio 2026</b> è disponibile nella tua area riservata.</p>
<p>Per motivi di sicurezza non alleghiamo mai documenti ai messaggi di posta e non ti chiederemo mai le credenziali via email.</p>
<p><a href="https://www.sella.it/area-riservata">Accedi all'area riservata</a></p>"#,
                minutes_ago: 2_800,
                seen: true,
                flagged: false,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Luca Romano", "luca.romano@politecnico.it"),
                subject: "Seminario su Rust e sistemi embedded — sei dei nostri?",
                snippet: "Stiamo organizzando per fine settembre una giornata sul Politecnico dedicata a Rust in ambito embedded. Ti andrebbe di tenere…",
                body_html: r#"<p>Caro Antonio,</p>
<p>stiamo organizzando per <b>fine settembre</b> una giornata al Politecnico dedicata a Rust in ambito embedded e industriale.</p>
<p>Ti andrebbe di tenere un intervento di circa 40 minuti? Pensavamo a qualcosa sull'integrazione fra Rust e le librerie di sistema, magari partendo dalla tua esperienza con GTK.</p>
<p>Il taglio è divulgativo: in sala ci saranno soprattutto studenti del terzo anno.</p>
<p>Un caro saluto,<br>Luca Romano<br><i>Dipartimento di Elettronica, Informazione e Bioingegneria</i></p>"#,
                minutes_ago: 4_320,
                seen: true,
                flagged: true,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Il Post", "newsletter@ilpost.it"),
                subject: "Le notizie di oggi, in breve",
                snippet: "Buongiorno. Ecco le cose che vale la pena sapere stamattina, in ordine di importanza e senza fronzoli.",
                body_html: r#"<p>Buongiorno.</p>
<p>Ecco le cose che vale la pena sapere stamattina:</p>
<h3>In Italia</h3>
<p>Il Consiglio dei ministri ha approvato il nuovo decreto sulle infrastrutture digitali, che stanzia fondi per la banda ultralarga nelle aree interne.</p>
<h3>Nel mondo</h3>
<p>Continuano i negoziati sul clima a Nairobi: il nodo resta il finanziamento della transizione per i paesi a basso reddito.</p>
<h3>Tecnologia</h3>
<p>GNOME 49 entra in fase di congelamento: la novità principale è il completamento della transizione a GTK4 per le applicazioni di sistema.</p>
<hr>
<p style="font-size:12px;color:#888">Ricevi questa newsletter perché ti sei iscritto su ilpost.it.</p>"#,
                minutes_ago: 7_000,
                seen: true,
                flagged: false,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Elena Ricci", "elena.ricci@cooperativaluce.org"),
                subject: "Grazie per la disponibilità di sabato",
                snippet: "Volevo solo ringraziarti a nome di tutta la cooperativa: senza il tuo aiuto con la rete non saremmo mai riusciti a…",
                body_html: r#"<p>Antonio,</p>
<p>volevo solo ringraziarti a nome di tutta la cooperativa. Senza il tuo aiuto con la configurazione della rete sabato non saremmo mai riusciti ad aprire lo sportello in tempo.</p>
<p>Se ti va, passa a trovarci quando vuoi: il caffè lo offriamo noi.</p>
<p>Un abbraccio,<br>Elena</p>"#,
                minutes_ago: 11_000,
                seen: true,
                flagged: false,
                answered: true,
                attachments: false,
            },
            Sample {
                from: ("Fastweb", "assistenza@fastweb.it"),
                subject: "Intervento tecnico programmato nella tua zona",
                snippet: "Ti informiamo che mercoledì 20 agosto, dalle 02:00 alle 06:00, il servizio potrebbe subire interruzioni per lavori di…",
                body_html: r#"<p>Gentile Cliente,</p>
<p>ti informiamo che <b>mercoledì 20 agosto, dalle 02:00 alle 06:00</b>, il servizio potrebbe subire brevi interruzioni per lavori di potenziamento della rete nella tua zona.</p>
<p>Non è richiesta alcuna azione da parte tua. Ci scusiamo per il disagio.</p>"#,
                minutes_ago: 15_500,
                seen: true,
                flagged: false,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Paolo Greco", "paolo@greco-consulting.eu"),
                subject: "Proposta di collaborazione per il Q4",
                snippet: "Ci siamo conosciuti alla conferenza di Bologna lo scorso mese. Vorrei riprendere il discorso sulla possibilità di…",
                body_html: r#"<p>Buongiorno Antonio,</p>
<p>ci siamo conosciuti alla conferenza di Bologna lo scorso mese — ero quello che ti ha fatto mille domande sul porting a GTK4 durante la pausa caffè.</p>
<p>Vorrei riprendere il discorso: nel <b>Q4</b> avremmo bisogno di una consulenza per modernizzare due applicazioni desktop interne, ancora ferme a GTK2.</p>
<p>Avresti disponibilità per una call conoscitiva nelle prossime due settimane?</p>
<p>Cordiali saluti,<br>Paolo Greco</p>"#,
                minutes_ago: 22_000,
                seen: true,
                flagged: false,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Comune di Milano", "protocollo@comune.milano.it"),
                subject: "Ricevuta di protocollo n. 2026/884512",
                snippet: "La sua istanza è stata protocollata in data odierna. Il numero di protocollo assegnato è 2026/884512.",
                body_html: r#"<p>Si comunica che l'istanza presentata è stata protocollata in data odierna.</p>
<table>
<tr><td><b>Numero di protocollo</b></td><td>2026/884512</td></tr>
<tr><td><b>Oggetto</b></td><td>Richiesta di accesso agli atti</td></tr>
<tr><td><b>Ufficio</b></td><td>Direzione Servizi Civici</td></tr>
</table>
<p>La presente vale come ricevuta ai sensi dell'art. 5 del DPR 445/2000.</p>"#,
                minutes_ago: 34_000,
                seen: true,
                flagged: false,
                answered: false,
                attachments: true,
            },
        ]
    } else {
        vec![
            Sample {
                from: ("Chiara Moretti", "chiara.moretti@acmesrl.it"),
                subject: "Preventivo per il restyling del gestionale",
                snippet: "Buongiorno, come da accordi telefonici le invio la richiesta formale di preventivo per il restyling del nostro gestionale interno.",
                body_html: r#"<p>Buongiorno,</p>
<p>come da accordi telefonici le invio la richiesta formale di preventivo per il restyling del nostro gestionale interno.</p>
<p>Le specifiche di massima:</p>
<ul>
<li>circa 40 maschere da riprogettare;</li>
<li>migrazione del backend da MySQL 5.7 a PostgreSQL;</li>
<li>formazione per 12 utenti finali.</li>
</ul>
<p>Resto in attesa di un suo riscontro.</p>
<p>Cordialmente,<br>Chiara Moretti<br><i>Responsabile IT — ACME S.r.l.</i></p>"#,
                minutes_ago: 55,
                seen: false,
                flagged: false,
                answered: false,
                attachments: true,
            },
            Sample {
                from: ("Studio Commercialista Neri", "amministrazione@studioneri.it"),
                subject: "Fatture da emettere entro fine mese",
                snippet: "Ti ricordo che entro il 31 agosto vanno emesse le fatture relative alle prestazioni di luglio. Mancano ancora due clienti.",
                body_html: r#"<p>Ciao Antonio,</p>
<p>ti ricordo che entro il <b>31 agosto</b> vanno emesse le fatture relative alle prestazioni di luglio.</p>
<p>Al momento mancano ancora:</p>
<ul>
<li>ACME S.r.l. — consulenza di 18 ore</li>
<li>Nordwind GmbH — canone di manutenzione</li>
</ul>
<p>Appena le hai caricate sul gestionale fammi un fischio.</p>"#,
                minutes_ago: 300,
                seen: false,
                flagged: true,
                answered: false,
                attachments: false,
            },
            Sample {
                from: ("Hetzner", "billing@hetzner.com"),
                subject: "Invoice R0012894556 for August 2026",
                snippet: "Dear customer, your invoice for the billing period of August 2026 is now available in your Robot account.",
                body_html: r#"<p>Dear customer,</p>
<p>your invoice for the billing period of <b>August 2026</b> is now available in your account.</p>
<table>
<tr><td><b>Invoice</b></td><td>R0012894556</td></tr>
<tr><td><b>Amount</b></td><td>&euro; 47,60</td></tr>
<tr><td><b>Due date</b></td><td>2026-08-28</td></tr>
</table>
<p>The amount will be collected automatically via SEPA direct debit.</p>"#,
                minutes_ago: 1_100,
                seen: true,
                flagged: false,
                answered: false,
                attachments: true,
            },
            Sample {
                from: ("Nordwind GmbH", "support@nordwind.dev"),
                subject: "Ticket #8841 chiuso — sincronizzazione IMAP",
                snippet: "Il ticket che avevi aperto sulla sincronizzazione IMAP è stato chiuso. La causa era un timeout troppo aggressivo lato proxy.",
                body_html: r#"<p>Il ticket <b>#8841</b> è stato chiuso.</p>
<p><b>Causa:</b> un timeout troppo aggressivo sul proxy in ingresso interrompeva le connessioni IMAP inattive dopo 60 secondi, prima che il client potesse inviare il NOOP di keepalive.</p>
<p><b>Soluzione:</b> il timeout è stato portato a 300 secondi, in linea con la raccomandazione dell'RFC 2177.</p>
<p>Se il problema dovesse ripresentarsi, riapri pure il ticket.</p>"#,
                minutes_ago: 4_000,
                seen: true,
                flagged: false,
                answered: true,
                attachments: false,
            },
            Sample {
                from: ("Registro .it", "noreply@nic.it"),
                subject: "Il dominio antoniopicone.it è stato rinnovato",
                snippet: "Confermiamo l'avvenuto rinnovo del dominio antoniopicone.it per ulteriori 12 mesi, con scadenza 14 agosto 2027.",
                body_html: r#"<p>Confermiamo l'avvenuto rinnovo del dominio <b>antoniopicone.it</b> per ulteriori 12 mesi.</p>
<table>
<tr><td><b>Dominio</b></td><td>antoniopicone.it</td></tr>
<tr><td><b>Nuova scadenza</b></td><td>14 agosto 2027</td></tr>
<tr><td><b>Registrar</b></td><td>Aruba S.p.A.</td></tr>
</table>"#,
                minutes_ago: 9_000,
                seen: true,
                flagged: false,
                answered: false,
                attachments: false,
            },
        ]
    }
}

fn build_summaries(account_id: &str, mailbox: &str) -> Vec<MessageSummary> {
    // Only the inbox carries the sample conversations; other folders show as
    // empty, which is also a useful state to see in the UI.
    if !mailbox.eq_ignore_ascii_case("INBOX") {
        return Vec::new();
    }

    let now = Local::now();
    samples(account_id)
        .into_iter()
        .enumerate()
        .map(|(index, sample)| MessageSummary {
            account_id: account_id.to_string(),
            mailbox: mailbox.to_string(),
            uid: 1000 + index as u32,
            from: Mailaddr::new(sample.from.0, sample.from.1),
            to: vec![Mailaddr::new("Antonio Picone", "info@antoniopicone.it")],
            subject: sample.subject.to_string(),
            snippet: sample.snippet.to_string(),
            date: now - Duration::minutes(sample.minutes_ago),
            seen: sample.seen,
            flagged: sample.flagged,
            answered: sample.answered,
            has_attachments: sample.attachments,
        })
        .collect()
}

fn build_message(account_id: &str, mailbox: &str, uid: u32) -> Option<Message> {
    let index = uid.checked_sub(1000)? as usize;
    let sample = samples(account_id).into_iter().nth(index)?;
    let summary = build_summaries(account_id, mailbox)
        .into_iter()
        .find(|s| s.uid == uid)?;

    Some(Message {
        text: crate::html::to_plain_text(sample.body_html),
        html: Some(crate::html::sanitize(sample.body_html)),
        cc: Vec::new(),
        attachments: if sample.attachments {
            vec![crate::model::Attachment {
                filename: match index % 3 {
                    0 => "contratto-v3.pdf".into(),
                    1 => "biglietto.pdf".into(),
                    _ => "allegato.pdf".into(),
                },
                mime_type: "application/pdf".into(),
                size: 148_211 + (index * 5_000),
                data: Vec::new(),
            }]
        } else {
            Vec::new()
        },
        summary,
    })
}

/// The demo counterpart of the IMAP worker loop.
pub fn run_worker(
    account: Account,
    rx: mpsc::Receiver<Command>,
    events: async_channel::Sender<Event>,
) {
    let emit = |event: Event| {
        let _ = events.send_blocking(event);
    };

    emit(Event::Status {
        account_id: account.id.clone(),
        state: ConnectionState::Online,
        detail: "Dati dimostrativi".into(),
    });

    while let Ok(command) = rx.recv() {
        match command {
            Command::Shutdown => break,

            Command::LoadMailboxes => emit(Event::Mailboxes {
                account_id: account.id.clone(),
                mailboxes: mailboxes_for(&account.id),
            }),

            Command::LoadMessages { mailbox, .. } => emit(Event::Messages {
                account_id: account.id.clone(),
                messages: build_summaries(&account.id, &mailbox),
                mailbox,
                append: false,
            }),

            Command::LoadMessage { mailbox, uid } => {
                match build_message(&account.id, &mailbox, uid) {
                    Some(message) => emit(Event::MessageLoaded {
                        account_id: account.id.clone(),
                        message: Box::new(message),
                    }),
                    None => emit(Event::Error {
                        account_id: account.id.clone(),
                        context: format!("apertura del messaggio {uid}"),
                        detail: "messaggio non presente nei dati dimostrativi".into(),
                    }),
                }
            }

            Command::SetFlag { mailbox, uid, flag, on } => emit(Event::FlagChanged {
                account_id: account.id.clone(),
                mailbox,
                uid,
                flag,
                on,
            }),

            Command::MoveMessage { mailbox, uid, .. } | Command::DeleteMessage { mailbox, uid } => {
                emit(Event::MessageRemoved { account_id: account.id.clone(), mailbox, uid })
            }

            // The demo never opens a socket, so a "send" just reports success.
            Command::Send { .. } => emit(Event::Sent { account_id: account.id.clone() }),
        }
    }
}

/// A fixed timestamp helper, used by tests that need deterministic dates.
#[allow(dead_code)]
pub fn fixed_date() -> chrono::DateTime<Local> {
    Local.with_ymd_and_hms(2026, 8, 17, 9, 30, 0).single().unwrap_or_else(Local::now)
}
