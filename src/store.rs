//! The add-in's files in its data folder, shared by GridCraft, WordCraft and DeckCraft (which
//! may run the add-in at the same time):
//!
//! - `accounts.json`: the accounts and their app passwords (owner-only on Unix);
//! - `state.json`: the documents linked to Nextcloud files (etag, file id, lock, pending upload);
//! - `settings.json`: the settings;
//! - `files/<account>/<remote path>`: files opened from Nextcloud;
//! - `backups/<document id>/`: backups, with `index.json`;
//! - `restored/`: backups opened again.
//!
//! Every write goes to a temporary file that is then renamed over the old one. Read-modify-write
//! happens under `.lock` (a file created exclusively), re-reading the file first, so two apps
//! never undo each other's changes. Network calls are never made while holding it.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Largest JSON file read.
const MAX_JSON: u64 = 64 << 20;

/// The settings (`settings.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub backup_on_save: bool,
    pub keep_versions: u32,
    pub keep_daily_days: u32,
    pub lock_while_editing: bool,
    pub upload_on_save: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { backup_on_save: true, keep_versions: 50, keep_daily_days: 30, lock_while_editing: true, upload_on_save: true }
    }
}

/// A Nextcloud account (`accounts.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Account {
    /// A stable id from the server and login name: the account's cache folder.
    pub id: String,
    /// `https://cloud.example.com` (no trailing slash).
    pub server: String,
    /// The name used to sign in (with the app password).
    pub login_name: String,
    /// The user id in WebDAV paths (`/remote.php/dav/files/<user id>/`).
    pub user_id: String,
    pub app_password: String,
    pub added_ms: i64,
}

impl Account {
    /// `alice@cloud.example.com`.
    pub fn label(&self) -> String {
        let host = self.server.split("://").nth(1).unwrap_or(&self.server);
        if self.login_name.contains('@') { format!("{} ({host})", self.login_name) } else { format!("{}@{host}", self.login_name) }
    }
}

/// The accounts file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Accounts {
    pub accounts: Vec<Account>,
}

/// A local document linked to a Nextcloud file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Link {
    pub account: String,
    /// The remote path (`/Documents/Budget.xlsx`).
    pub remote: String,
    /// The server's etag of the version the local file is based on.
    pub etag: String,
    pub fileid: String,
    /// Whether the user may write the remote file (Nextcloud's `oc:permissions` has `W`).
    pub writable: bool,
    /// The WebDAV lock token while we hold a lock.
    pub lock_token: String,
    /// `locked`, `unavailable`, `other` (someone else's lock), `off`, or empty.
    pub lock: String,
    /// Why locking isn't available (the server's answer).
    pub lock_note: String,
    /// The local file has changes not uploaded yet.
    pub pending: bool,
    pub pending_reason: String,
    pub last_upload_ms: i64,
    /// The remote path this is a conflict copy of, after a conflict.
    pub conflict_of: String,
}

/// The state file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// By the local document's canonical path.
    pub links: BTreeMap<String, Link>,
}

/// The data folder.
#[derive(Clone, Debug)]
pub struct Store {
    pub dir: PathBuf,
}

/// Holds `.lock` until dropped.
struct DirLock {
    path: Option<PathBuf>,
}

impl Drop for DirLock {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = fs::remove_file(p);
        }
    }
}

impl Store {
    /// The platform's data folder, created if needed.
    pub fn open() -> Result<Store, String> {
        Store::at(crate::paths::data_dir()?)
    }

    /// A data folder, created if needed (owner-only on Unix).
    pub fn at(dir: PathBuf) -> Result<Store, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        restrict_dir(&dir);
        Ok(Store { dir })
    }

    pub fn files_dir(&self, account: &str) -> PathBuf {
        self.dir.join("files").join(account)
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.dir.join("backups")
    }

    pub fn restored_dir(&self) -> PathBuf {
        self.dir.join("restored")
    }

    /// Takes `.lock`: waits up to 5 s; a lock older than 30 s was left by a process that died
    /// and is broken. Never fails: after waiting, it goes ahead without (better than losing the
    /// user's change).
    fn lock(&self) -> DirLock {
        let path = self.dir.join(".lock");
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return DirLock { path: Some(path) };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale =
                        fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(30));
                    if stale {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed() > Duration::from_secs(5) {
                        return DirLock { path: None };
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return DirLock { path: None },
            }
        }
    }

    fn read<T: DeserializeOwned + Default>(&self, name: &str) -> T {
        let path = self.dir.join(name);
        match read_json(&path) {
            Ok(Some(v)) => v,
            Ok(None) => T::default(),
            Err(_) => {
                // Keep the broken file for the user; start again.
                let _ = fs::rename(&path, self.dir.join(format!("{name}.broken-{}", crate::time::now_ms())));
                T::default()
            }
        }
    }

    pub fn config(&self) -> Config {
        self.read("settings.json")
    }

    pub fn save_config(&self, c: &Config) -> Result<(), String> {
        let _l = self.lock();
        write_json(&self.dir.join("settings.json"), c, false)
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.read::<Accounts>("accounts.json").accounts
    }

    /// Changes the accounts under the lock.
    pub fn update_accounts<T>(&self, f: impl FnOnce(&mut Vec<Account>) -> T) -> Result<T, String> {
        let _l = self.lock();
        let mut a: Accounts = self.read("accounts.json");
        let out = f(&mut a.accounts);
        write_json(&self.dir.join("accounts.json"), &a, true)?;
        Ok(out)
    }

    pub fn state(&self) -> State {
        self.read("state.json")
    }

    /// The link of a local document.
    pub fn link(&self, key: &str) -> Option<Link> {
        self.state().links.remove(key)
    }

    /// Changes the state under the lock.
    pub fn update_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T, String> {
        let _l = self.lock();
        let mut s: State = self.read("state.json");
        let out = f(&mut s);
        write_json(&self.dir.join("state.json"), &s, false)?;
        Ok(out)
    }

    /// Changes one link (if it's still there) under the lock.
    pub fn update_link(&self, key: &str, f: impl FnOnce(&mut Link)) -> Result<(), String> {
        self.update_state(|s| {
            if let Some(l) = s.links.get_mut(key) {
                f(l);
            }
        })
    }

    /// Runs `f` under the lock (for files other than the three above: backup indexes).
    pub fn locked<T>(&self, f: impl FnOnce() -> T) -> T {
        let _l = self.lock();
        f()
    }
}

/// Reads a JSON file: `Ok(None)` when it doesn't exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if meta.len() > MAX_JSON {
        return Err("file too large".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map(Some).map_err(|e| e.to_string())
}

/// Writes JSON atomically (temporary file, then rename). `private`: owner-only on Unix.
pub fn write_json<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes, private)
}

/// Writes a file atomically.
pub fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    let dir = path.parent().ok_or("no folder")?;
    fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let tmp = temp_path(path);
    let result = (|| -> std::io::Result<()> {
        let mut f = new_file(&tmp, private)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("can't write {}: {e}", path.display()));
    }
    Ok(())
}

/// A temporary name next to `path`.
pub fn temp_path(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = N.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.tmp-{}-{n}", std::process::id()))
}

/// Creates a new file (truncating), owner-only on Unix when `private`.
pub fn new_file(path: &Path, private: bool) -> std::io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if private {
            o.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    o.open(path)
}

fn restrict_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("craft-nextcloud-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn state_round_trips_and_merges() {
        let d = temp("state");
        let s = Store::at(d.clone()).unwrap();
        assert_eq!(s.config(), Config::default());
        s.update_state(|st| {
            st.links.insert("/a".into(), Link { remote: "/x.xlsx".into(), ..Link::default() });
        })
        .unwrap();
        s.update_link("/a", |l| l.etag = "\"e1\"".into()).unwrap();
        s.update_link("/missing", |l| l.etag = "nope".into()).unwrap();
        let st = s.state();
        assert_eq!(st.links.len(), 1);
        assert_eq!(st.links.get("/a").map(|l| l.etag.as_str()), Some("\"e1\""));
        // A broken file is set aside, never a crash.
        fs::write(d.join("state.json"), b"{not json").unwrap();
        assert!(s.state().links.is_empty());
        assert!(!d.join(".lock").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn accounts_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = temp("accounts");
        let s = Store::at(d.clone()).unwrap();
        s.update_accounts(|a| a.push(Account { id: "x".into(), app_password: "secret".into(), ..Account::default() })).unwrap();
        let mode = fs::metadata(d.join("accounts.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(s.accounts().len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_locks_are_broken() {
        let d = temp("stale");
        let s = Store::at(d.clone()).unwrap();
        fs::write(d.join(".lock"), b"1").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(120);
        let f = fs::File::options().write(true).open(d.join(".lock")).unwrap();
        f.set_modified(old).unwrap();
        drop(f);
        let start = Instant::now();
        s.save_config(&Config::default()).unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        let _ = fs::remove_dir_all(&d);
    }
}
