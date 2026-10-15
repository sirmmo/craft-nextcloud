//! The add-in's own words, translated. The app passes its UI language as `host.language`
//! (`it`, `it-IT`…); other languages fall back to English.
//!
//! To add a language: add its tag to [`LANGS`] and one more column to every row of [`TABLE`]
//! (the compiler counts the columns).

/// The languages, in the order of [`TABLE`]'s columns.
pub const LANGS: [&str; 2] = ["en", "it"];

/// A language: an index into [`LANGS`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lang(usize);

impl Lang {
    pub const EN: Lang = Lang(0);

    /// The language of a BCP 47 tag (`it-IT` → Italian); English when it isn't known.
    pub fn from_tag(tag: &str) -> Lang {
        let primary = tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        Lang(LANGS.iter().position(|l| *l == primary).unwrap_or(0))
    }
}

/// `key` → its text in each language. `{0}`, `{1}`… are filled in by [`trf`].
static TABLE: &[(&str, [&str; LANGS.len()])] = &[
    // Titles
    ("title.connect", ["Connect to Nextcloud", "Connetti a Nextcloud"]),
    ("title.open", ["Open from Nextcloud", "Apri da Nextcloud"]),
    ("title.saveTo", ["Save to Nextcloud", "Salva su Nextcloud"]),
    ("title.accounts", ["Nextcloud Accounts", "Account Nextcloud"]),
    ("title.disconnect", ["Disconnect from Nextcloud", "Disconnetti da Nextcloud"]),
    ("title.backups", ["Backups", "Copie di backup"]),
    ("title.settings", ["Nextcloud Settings", "Impostazioni Nextcloud"]),
    ("title.share", ["Share Link", "Link di condivisione"]),
    ("title.status", ["Nextcloud Status", "Stato di Nextcloud"]),
    ("title.nextcloud", ["Nextcloud", "Nextcloud"]),
    // Buttons
    ("btn.continue", ["Continue", "Continua"]),
    ("btn.openAgain", ["Open Sign-in Page Again", "Riapri la pagina di accesso"]),
    ("btn.open", ["Open", "Apri"]),
    ("btn.up", ["Up", "Su"]),
    ("btn.switchAccount", ["Switch Account", "Cambia account"]),
    ("btn.addAccount", ["Add Account", "Aggiungi account"]),
    ("btn.saveHere", ["Save in This Folder", "Salva in questa cartella"]),
    ("btn.replace", ["Replace", "Sostituisci"]),
    ("btn.restore", ["Open Copy", "Apri copia"]),
    ("btn.backupsFolder", ["Open Backups Folder", "Apri cartella dei backup"]),
    ("btn.save", ["Save", "Salva"]),
    ("btn.openLink", ["Open Link", "Apri link"]),
    ("btn.connect", ["Connect", "Connetti"]),
    ("btn.disconnect", ["Disconnect", "Disconnetti"]),
    ("btn.choose", ["Choose", "Scegli"]),
    // Fields
    ("field.server", ["Server address", "Indirizzo del server"]),
    ("field.backupOnSave", ["Back up every document I save", "Fai un backup di ogni documento che salvo"]),
    ("field.keepVersions", ["Versions to keep per document", "Versioni da tenere per documento"]),
    ("field.keepDailyDays", ["Also keep one per day for (days)", "Tieni anche una al giorno per (giorni)"]),
    ("field.lock", ["Lock Nextcloud files while I edit them", "Blocca i file di Nextcloud mentre li modifico"]),
    ("field.upload", ["Upload to Nextcloud when I save", "Carica su Nextcloud quando salvo"]),
    ("field.link", ["Link", "Link"]),
    // Prompts and messages
    (
        "connect.prompt",
        ["Your Nextcloud's address, for example https://cloud.example.com", "L'indirizzo del tuo Nextcloud, per esempio https://cloud.example.com"],
    ),
    (
        "connect.browser",
        [
            "Finish signing in in your browser, then click Continue.\n\nIf the browser didn't open, go to:\n{0}",
            "Completa l'accesso nel browser, poi fai clic su Continua.\n\nSe il browser non si è aperto, vai a:\n{0}",
        ],
    ),
    (
        "connect.waiting",
        [
            "Still waiting for you to sign in to {0}. Finish signing in in your browser, then click Continue.",
            "Sto ancora aspettando l'accesso a {0}. Completa l'accesso nel browser, poi fai clic su Continua.",
        ],
    ),
    ("connect.expired", ["The sign-in has expired. Click Connect to start again.", "L'accesso è scaduto. Fai clic su Connetti per ricominciare."]),
    ("connect.done", ["Connected to {0} as {1}", "Connesso a {0} come {1}"]),
    ("connect.badServer", ["“{0}” isn't a Nextcloud address: {1}", "“{0}” non è un indirizzo di Nextcloud: {1}"]),
    ("notConnected", ["You aren't connected to Nextcloud yet. Connect first.", "Non sei ancora connesso a Nextcloud. Connettiti prima."]),
    ("saveFirst", ["Save the document first.", "Salva prima il documento."]),
    (
        "notLinked",
        [
            "This document isn't stored in Nextcloud. Open it from Nextcloud, or use Save to Nextcloud first.",
            "Questo documento non è su Nextcloud. Aprilo da Nextcloud o usa prima Salva su Nextcloud.",
        ],
    ),
    (
        "noFeatures",
        [
            "This add-in needs an app with plugin protocol 1.2 (dialogs, open, events).",
            "Questo componente aggiuntivo richiede un'app con il protocollo dei plugin 1.2 (finestre, apertura, eventi).",
        ],
    ),
    ("folder", ["Folder", "Cartella"]),
    ("emptyFolder", ["{0} · this folder has nothing this app opens", "{0} · questa cartella non contiene nulla che questa app apra"]),
    ("truncated", ["{0} · showing the first {1}", "{0} · mostro i primi {1}"]),
    ("pickFolder", ["{0} · choose a folder, then Save in This Folder", "{0} · scegli una cartella, poi Salva in questa cartella"]),
    ("notOpened", ["This app doesn't open “{0}” files.", "Questa app non apre i file “{0}”."]),
    (
        "opened.pending",
        [
            "Opened your copy: its changes haven't reached Nextcloud yet (upload pending).",
            "Aperta la tua copia: le modifiche non sono ancora su Nextcloud (caricamento in sospeso).",
        ],
    ),
    ("exists", ["“{0}” already exists in {1}. Replace it?", "“{0}” esiste già in {1}. Vuoi sostituirlo?"]),
    ("savedTo", ["Saved to Nextcloud: {0}", "Salvato su Nextcloud: {0}"]),
    ("saved", ["Saved to Nextcloud", "Salvato su Nextcloud"]),
    ("pending", ["Saved locally; Nextcloud upload pending", "Salvato in locale; caricamento su Nextcloud in sospeso"]),
    ("pending.reason", ["Saved locally; Nextcloud upload pending ({0})", "Salvato in locale; caricamento su Nextcloud in sospeso ({0})"]),
    (
        "conflict",
        [
            "“{0}” changed on Nextcloud since you opened it. Your version was uploaded next to it as “{1}”; the server's version is unchanged and a copy is in your backups. Your next saves go to “{1}”.",
            "“{0}” è cambiato su Nextcloud da quando l'hai aperto. La tua versione è stata caricata accanto come “{1}”; la versione del server non è cambiata e una copia è nei tuoi backup. I prossimi salvataggi andranno in “{1}”.",
        ],
    ),
    (
        "reuploaded",
        [
            "“{0}” had been deleted on Nextcloud; your version was uploaded again.",
            "“{0}” era stato eliminato da Nextcloud; la tua versione è stata caricata di nuovo.",
        ],
    ),
    (
        "changedRemote",
        [
            "“{0}” changed on Nextcloud since you last opened it. Open it again from Nextcloud to get the latest version.",
            "“{0}” è cambiato su Nextcloud dall'ultima apertura. Riaprilo da Nextcloud per avere la versione più recente.",
        ],
    ),
    (
        "hasPending",
        [
            "This document has changes that haven't reached Nextcloud yet. Save it or use Sync Now.",
            "Questo documento ha modifiche non ancora su Nextcloud. Salvalo o usa Sincronizza ora.",
        ],
    ),
    ("lockedByOther", ["locked on Nextcloud by someone else", "bloccato su Nextcloud da un'altra persona"]),
    ("offline", ["can't reach Nextcloud", "Nextcloud non raggiungibile"]),
    ("uploadOff", ["upload on save is off", "il caricamento al salvataggio è disattivato"]),
    ("upToDate", ["Up to date with Nextcloud", "Allineato con Nextcloud"]),
    ("synced", ["Uploaded {0} pending document(s) to Nextcloud", "Caricati su Nextcloud {0} documenti in sospeso"]),
    ("stillPending", ["{0} document(s) still waiting to upload", "{0} documenti ancora in attesa di caricamento"]),
    ("noBackups", ["There are no backups of this document yet.", "Non ci sono ancora backup di questo documento."]),
    ("backups.prompt", ["Backups of {0}, newest first", "Backup di {0}, dal più recente"]),
    ("kind.saved", ["Saved", "Salvato"]),
    ("kind.downloaded", ["Downloaded from Nextcloud", "Scaricato da Nextcloud"]),
    ("kind.server", ["Server's version (conflict)", "Versione del server (conflitto)"]),
    ("backupOf", ["Backup of {0} · {1}", "Backup di {0} · {1}"]),
    ("settings.saved", ["Settings saved", "Impostazioni salvate"]),
    ("settings.bad", ["“{0}” must be a whole number from {1} to {2}.", "“{0}” deve essere un numero intero da {1} a {2}."]),
    ("browserOpened", ["Opened in your browser", "Aperto nel browser"]),
    ("share.prompt", ["Anyone with this link can open “{0}”:", "Chiunque abbia questo link può aprire “{0}”:"]),
    ("disconnected", ["Disconnected {0}", "Disconnesso {0}"]),
    (
        "disconnect.prompt",
        ["Choose the account to disconnect (its app password is revoked)", "Scegli l'account da disconnettere (la sua password per app viene revocata)"],
    ),
    ("accounts.prompt", ["Choose an account", "Scegli un account"]),
    ("error", ["Nextcloud: {0}", "Nextcloud: {0}"]),
    // Status
    ("status.accounts", ["Accounts:", "Account:"]),
    ("status.none", ["  none", "  nessuno"]),
    ("status.notLinked", ["This document isn't stored in Nextcloud.", "Questo documento non è su Nextcloud."]),
    ("status.noDocument", ["No saved document.", "Nessun documento salvato."]),
    ("status.linked", ["This document: {0} on {1}", "Questo documento: {0} su {1}"]),
    ("status.conflictOf", ["(your conflict copy of {0})", "(la tua copia in conflitto di {0})"]),
    ("status.lastUpload", ["Last upload: {0}", "Ultimo caricamento: {0}"]),
    ("status.never", ["never", "mai"]),
    ("status.pending", ["Upload pending: {0}", "Caricamento in sospeso: {0}"]),
    ("status.lock.locked", ["Lock: locked by you while you edit", "Blocco: bloccato da te mentre lo modifichi"]),
    ("status.lock.unavailable", ["Lock: not available on this server ({0})", "Blocco: non disponibile su questo server ({0})"]),
    ("status.lock.other", ["Lock: locked by someone else", "Blocco: bloccato da un'altra persona"]),
    ("status.lock.off", ["Lock: off", "Blocco: disattivato"]),
    ("status.lock.none", ["Lock: none", "Blocco: nessuno"]),
    ("status.settings", ["Backups: {0} · Upload on save: {1} · Locking: {2}", "Backup: {0} · Caricamento al salvataggio: {1} · Blocco: {2}"]),
    ("on", ["on", "attivo"]),
    ("off", ["off", "disattivo"]),
];

/// The text of `key` in `lang` (the key itself if it's missing, so a slip shows, never crashes).
pub fn tr(lang: Lang, key: &'static str) -> &'static str {
    TABLE.iter().find(|(k, _)| *k == key).and_then(|(_, row)| row.get(lang.0).or_else(|| row.first())).copied().unwrap_or(key)
}

/// [`tr`] with `{0}`, `{1}`… replaced by `args`.
pub fn trf(lang: Lang, key: &'static str, args: &[&str]) -> String {
    let mut s = tr(lang, key).to_string();
    for (i, a) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), a);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        let it = Lang::from_tag("it-IT");
        assert_eq!(tr(it, "btn.continue"), "Continua");
        assert_eq!(tr(Lang::from_tag("fr"), "btn.continue"), "Continue");
        assert_eq!(trf(Lang::EN, "connect.done", &["https://c", "me"]), "Connected to https://c as me");
        assert_eq!(tr(Lang::EN, "no.such.key"), "no.such.key");
    }

    #[test]
    fn keys_are_unique_and_filled() {
        for (i, (k, row)) in TABLE.iter().enumerate() {
            assert!(TABLE.iter().skip(i + 1).all(|(k2, _)| k2 != k), "duplicate key {k}");
            assert!(row.iter().all(|t| !t.is_empty()), "empty text for {k}");
        }
    }
}
