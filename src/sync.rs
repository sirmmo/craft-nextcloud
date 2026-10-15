//! Keeping a linked document and its Nextcloud file together: uploads with conflict detection,
//! the pending queue, and locks.

use std::fs;

use crate::backup;
use crate::dav::{self, Budget, Client, Fail, LockOutcome};
use crate::paths;
use crate::store::{Account, Config, Link, Store};
use crate::time;

/// How an upload went.
#[derive(Clone, Debug, PartialEq)]
pub enum Upload {
    Done,
    /// The server's file had changed: ours went up as `copy` (a name next to `original`).
    Conflict {
        original: String,
        copy: String,
    },
    /// The server's file had been deleted: ours went up again.
    Reuploaded(String),
    /// Not uploaded; kept queued (why).
    Pending(String),
}

pub fn account(store: &Store, id: &str) -> Option<Account> {
    store.accounts().into_iter().find(|a| a.id == id)
}

fn mark_pending(store: &Store, key: &str, reason: &str) -> Upload {
    let _ = store.update_link(key, |l| {
        l.pending = true;
        l.pending_reason = reason.to_string();
    });
    Upload::Pending(reason.to_string())
}

fn pending_reason(f: &Fail, lang: crate::text::Lang) -> String {
    match f {
        Fail::Net(_) => crate::text::tr(lang, "offline").to_string(),
        Fail::Status(423, _) => crate::text::tr(lang, "lockedByOther").to_string(),
        other => other.text(),
    }
}

/// The etag and file id after an upload: from its answer, else asked for.
fn after_upload(c: &Client, remote: &str, u: dav::Uploaded) -> (String, String) {
    if !u.etag.is_empty() && !u.fileid.is_empty() {
        return (u.etag, u.fileid);
    }
    match c.stat(remote) {
        Ok(item) => (if u.etag.is_empty() { item.etag } else { u.etag }, if u.fileid.is_empty() { item.fileid } else { u.fileid }),
        Err(_) => (u.etag, u.fileid),
    }
}

/// Uploads the linked document `key` (its local path) to its Nextcloud file, if the server's
/// version is still the one it's based on. Updates the link; never loses the user's version.
pub fn upload(store: &Store, key: &str, budget: Budget, lang: crate::text::Lang) -> Upload {
    let Some(link) = store.link(key) else { return Upload::Pending("not linked".into()) };
    let Some(acc) = account(store, &link.account) else { return mark_pending(store, key, "the Nextcloud account was disconnected") };
    match fs::metadata(key) {
        Ok(m) if m.len() > dav::MAX_UPLOAD => return mark_pending(store, key, "the file is too large to upload (over 1 GiB)"),
        Ok(m) if m.is_file() => {}
        _ => return mark_pending(store, key, "the local file can't be read"),
    }
    let body = match fs::read(key) {
        Ok(b) => b,
        Err(e) => return mark_pending(store, key, &format!("the local file can't be read: {e}")),
    };
    let c = Client::new(&acc, budget);
    let mut token = link.lock_token.clone();
    for _attempt in 0..2 {
        let etag = (!link.etag.is_empty()).then_some(link.etag.as_str());
        let tok = (!token.is_empty()).then_some(token.as_str());
        match c.upload(&link.remote, body.clone(), etag, false, tok) {
            Ok(u) => {
                let (etag, fileid) = after_upload(&c, &link.remote, u);
                let now = time::now_ms();
                let _ = store.update_link(key, |l| {
                    l.etag = etag;
                    if !fileid.is_empty() {
                        l.fileid = fileid;
                    }
                    l.pending = false;
                    l.pending_reason.clear();
                    l.last_upload_ms = now;
                    l.lock_token = token.clone();
                    if token.is_empty() && l.lock == "locked" {
                        l.lock.clear();
                    }
                });
                if !token.is_empty() {
                    refresh_lock(store, key, &c, &link.remote, &token);
                }
                return Upload::Done;
            }
            Err(Fail::Status(412, _)) => match c.stat(&link.remote) {
                // Same version on the server: the lock token was the problem (expired). Drop it.
                Ok(item) if dav::same_etag(&item.etag, &link.etag) && !token.is_empty() => {
                    token.clear();
                    continue;
                }
                Ok(_) => return conflict(store, key, &link, &c, body, lang),
                Err(Fail::Status(404, _)) => {
                    return match c.upload(&link.remote, body, None, true, None) {
                        Ok(u) => {
                            let (etag, fileid) = after_upload(&c, &link.remote, u);
                            let now = time::now_ms();
                            let _ = store.update_link(key, |l| {
                                l.etag = etag;
                                l.fileid = fileid;
                                l.pending = false;
                                l.pending_reason.clear();
                                l.last_upload_ms = now;
                                l.lock_token.clear();
                                l.lock.clear();
                            });
                            Upload::Reuploaded(paths::remote_name(&link.remote).to_string())
                        }
                        Err(f) => mark_pending(store, key, &pending_reason(&f, lang)),
                    };
                }
                Err(f) => return mark_pending(store, key, &pending_reason(&f, lang)),
            },
            Err(f) => return mark_pending(store, key, &pending_reason(&f, lang)),
        }
    }
    mark_pending(store, key, "the server refused the upload (412)")
}

/// The server's file changed: keep its version in the backups, upload ours as a conflict copy
/// next to it, and link the document to the copy.
fn conflict(store: &Store, key: &str, link: &Link, c: &Client, body: Vec<u8>, lang: crate::text::Lang) -> Upload {
    let now = time::now_ms();
    let name = paths::remote_name(&link.remote).to_string();
    // The server's version, into the backups (best effort: it stays on the server anyway).
    let tmp = store.dir.join(format!(".server-{}-{now}.tmp", std::process::id()));
    if c.download(&link.remote, &tmp).is_ok() {
        let cfg = store.config();
        if let Err(e) = backup::add(store, key, &tmp, backup::SERVER, now, &cfg) {
            eprintln!("craft-nextcloud: couldn't back up the server's version: {e}");
        }
    }
    let _ = fs::remove_file(&tmp);
    let copy_name = paths::conflict_name(&name, &crate::system::hostname(), &time::file_display(now));
    let copy = match paths::join_remote(&paths::parent_remote(&link.remote), &copy_name) {
        Ok(p) => p,
        Err(e) => return mark_pending(store, key, &e),
    };
    match c.upload(&copy, body, None, false, None) {
        Ok(u) => {
            if !link.lock_token.is_empty() {
                let _ = c.unlock(&link.remote, &link.lock_token);
            }
            let (etag, fileid) = after_upload(c, &copy, u);
            let original = link.remote.clone();
            let _ = store.update_link(key, |l| {
                if l.conflict_of.is_empty() {
                    l.conflict_of = original.clone();
                }
                l.remote = copy.clone();
                l.etag = etag;
                l.fileid = fileid;
                l.writable = true;
                l.lock_token.clear();
                l.lock.clear();
                l.pending = false;
                l.pending_reason.clear();
                l.last_upload_ms = now;
            });
            Upload::Conflict { original: name, copy: copy_name }
        }
        Err(f) => mark_pending(store, key, &pending_reason(&f, lang)),
    }
}

fn refresh_lock(store: &Store, key: &str, c: &Client, remote: &str, token: &str) {
    let outcome = c.lock(remote, Some(token));
    let _ = store.update_link(key, |l| match outcome {
        Ok(LockOutcome::Locked(t)) => {
            l.lock_token = t;
            l.lock = "locked".into();
        }
        Ok(LockOutcome::Other) => {
            l.lock_token.clear();
            l.lock = "other".into();
        }
        Ok(LockOutcome::Unavailable(n)) => {
            l.lock_token.clear();
            l.lock = "unavailable".into();
            l.lock_note = n;
        }
        Err(_) => {}
    });
}

/// Locks the linked document's Nextcloud file if the settings say so, it's writable and we
/// don't hold a lock yet. A server without locking is noted, never an error.
pub fn take_lock(store: &Store, cfg: &Config, key: &str, budget: Budget) {
    let Some(link) = store.link(key) else { return };
    if !cfg.lock_while_editing {
        if link.lock_token.is_empty() {
            let _ = store.update_link(key, |l| l.lock = "off".into());
        }
        return;
    }
    if !link.lock_token.is_empty() || !link.writable {
        return;
    }
    let Some(acc) = account(store, &link.account) else { return };
    let c = Client::new(&acc, budget);
    let outcome = c.lock(&link.remote, None);
    let _ = store.update_link(key, |l| match outcome {
        Ok(LockOutcome::Locked(t)) => {
            l.lock_token = t;
            l.lock = "locked".into();
            l.lock_note.clear();
        }
        Ok(LockOutcome::Other) => l.lock = "other".into(),
        Ok(LockOutcome::Unavailable(n)) => {
            l.lock = "unavailable".into();
            l.lock_note = n;
        }
        Err(f) => {
            l.lock.clear();
            l.lock_note = f.text();
        }
    });
}

/// Gives back our lock on the document's Nextcloud file, if we hold one.
pub fn release_lock(store: &Store, key: &str, budget: Budget) {
    let Some(link) = store.link(key) else { return };
    if link.lock_token.is_empty() {
        return;
    }
    if let Some(acc) = account(store, &link.account) {
        let _ = Client::new(&acc, budget).unlock(&link.remote, &link.lock_token);
    }
    let _ = store.update_link(key, |l| {
        l.lock_token.clear();
        if l.lock == "locked" {
            l.lock.clear();
        }
    });
}
