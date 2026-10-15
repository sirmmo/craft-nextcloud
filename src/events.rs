//! Document events (protocol 1.2): `events/documentOpened`, `events/documentSaved`,
//! `events/documentClosed`. They answer `{}` or `{"message"}` within their budget (30 s, 30 s,
//! 5 s) and never stop the app's open, save or close.

use serde_json::{Value as Json, json};

use crate::backup;
use crate::dav::{self, Client};
use crate::paths;
use crate::plugin::{Fail, Session, fail};
use crate::sync::{self, Upload};
use crate::text::{tr, trf};
use crate::time;

fn path_of(params: &Json) -> Option<String> {
    params.get("path").and_then(Json::as_str).filter(|p| !p.is_empty() && p.len() <= 32_768 && !p.contains('\0')).map(str::to_string)
}

fn message(text: String) -> Json {
    json!({"message": text})
}

/// A save succeeded: back it up, then upload it if it's linked.
pub fn saved(s: &mut Session, params: &Json) -> Result<Json, Fail> {
    let Some(path) = path_of(params) else { return Ok(json!({})) };
    let store = s.store().map_err(fail)?;
    let cfg = store.config();
    let key = paths::doc_key(&path);
    let link = store.link(&key);
    if (cfg.backup_on_save || link.is_some())
        && let Err(e) = backup::add(&store, &key, std::path::Path::new(&key), backup::SAVED, time::now_ms(), &cfg)
    {
        eprintln!("craft-nextcloud: no backup of {key}: {e}");
    }
    let Some(_) = link else { return Ok(json!({})) };
    let lang = s.lang;
    if !cfg.upload_on_save {
        let _ = store.update_link(&key, |l| {
            l.pending = true;
            l.pending_reason = tr(lang, "uploadOff").to_string();
        });
        return Ok(json!({}));
    }
    Ok(message(upload_message(lang, sync::upload(&store, &key, dav::EVENT, lang))))
}

/// The status-bar message for an upload's outcome.
pub fn upload_message(lang: crate::text::Lang, u: Upload) -> String {
    match u {
        Upload::Done => tr(lang, "saved").to_string(),
        Upload::Conflict { original, copy } => trf(lang, "conflict", &[&original, &copy]),
        Upload::Reuploaded(name) => trf(lang, "reuploaded", &[&name]),
        Upload::Pending(r) if r.is_empty() => tr(lang, "pending").to_string(),
        Upload::Pending(r) => trf(lang, "pending.reason", &[&r]),
    }
}

/// A document was opened: if it's linked, say when the server has a newer version, and lock it.
pub fn opened(s: &mut Session, params: &Json) -> Result<Json, Fail> {
    let Some(path) = path_of(params) else { return Ok(json!({})) };
    let store = s.store().map_err(fail)?;
    let key = paths::doc_key(&path);
    let Some(link) = store.link(&key) else { return Ok(json!({})) };
    let cfg = store.config();
    let lang = s.lang;
    let mut note = None;
    if link.pending {
        note = Some(tr(lang, "hasPending").to_string());
    } else if let Some(acc) = sync::account(&store, &link.account) {
        let c = Client::new(&acc, dav::EVENT);
        match c.stat(&link.remote) {
            Ok(item) => {
                if !link.etag.is_empty() && !dav::same_etag(&item.etag, &link.etag) {
                    note = Some(trf(lang, "changedRemote", &[paths::remote_name(&link.remote)]));
                }
                let writable = item.writable();
                let _ = store.update_link(&key, |l| {
                    l.writable = writable;
                    if !item.fileid.is_empty() {
                        l.fileid = item.fileid.clone();
                    }
                });
            }
            Err(e) => eprintln!("craft-nextcloud: couldn't check {}: {}", link.remote, e.text()),
        }
    }
    // Don't lock a file someone else changed: the user is about to reopen it.
    if note.is_none() || link.pending {
        sync::take_lock(&store, &cfg, &key, dav::EVENT);
    }
    Ok(note.map(message).unwrap_or_else(|| json!({})))
}

/// A document was closed: give back our lock. Fast (5 s budget).
pub fn closed(s: &mut Session, params: &Json) -> Result<Json, Fail> {
    let Some(path) = path_of(params) else { return Ok(json!({})) };
    let store = s.store().map_err(fail)?;
    sync::release_lock(&store, &paths::doc_key(&path), dav::CLOSE);
    Ok(json!({}))
}
