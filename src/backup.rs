//! Local backups: every save (and every version downloaded from Nextcloud) copied to
//! `backups/<document id>/<time>.<ext>`, listed in `index.json`, thinned by the retention rule:
//! keep the newest N, plus the newest of each day for the last D days, and never the last copy.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::store::{self, Config, Store};

/// Backups larger than this aren't taken (2 GiB).
pub const MAX_BACKUP: u64 = 2 << 30;
/// Entries kept in an index at most, whatever the settings.
pub const MAX_ENTRIES: usize = 10_000;

/// Why a backup was taken.
pub const SAVED: &str = "saved";
pub const DOWNLOADED: &str = "downloaded";
pub const SERVER: &str = "server";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    /// The backup's file name in the document's folder.
    pub file: String,
    pub time_ms: i64,
    pub size: u64,
    /// [`SAVED`], [`DOWNLOADED`] or [`SERVER`].
    pub kind: String,
}

/// `index.json`: the document the backups are of, and the backups (oldest first).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Index {
    pub original: String,
    pub name: String,
    pub entries: Vec<Entry>,
}

/// The backups folder of a document (by its key, [`paths::doc_key`]).
pub fn dir_for(store: &Store, key: &str) -> PathBuf {
    store.backups_dir().join(paths::stable_id(key))
}

fn read_index(dir: &Path) -> Index {
    store::read_json(&dir.join("index.json")).ok().flatten().unwrap_or_default()
}

/// The document's backups, newest first (only those whose file is still there).
pub fn list(store: &Store, key: &str) -> Vec<Entry> {
    let dir = dir_for(store, key);
    let mut e: Vec<Entry> = read_index(&dir).entries.into_iter().filter(|e| safe_file(&e.file) && dir.join(&e.file).is_file()).collect();
    e.sort_by(|a, b| b.time_ms.cmp(&a.time_ms).then_with(|| b.file.cmp(&a.file)));
    e
}

/// A backup file name from the index is a plain name (never a path).
pub fn safe_file(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && name != "index.json" && !name.contains(['/', '\\', '\0', ':'])
}

/// Copies `src` (the document as saved, or a version from the server) into the document's
/// backups and applies the retention rule. Returns the new entry.
pub fn add(store: &Store, key: &str, src: &Path, kind: &str, now_ms: i64, cfg: &Config) -> Result<Entry, String> {
    let meta = fs::metadata(src).map_err(|e| format!("can't read {}: {e}", src.display()))?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_BACKUP {
        return Err("too large to back up".into());
    }
    let dir = dir_for(store, key);
    fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let name = Path::new(key).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = paths::extension(&name);
    let suffix = if kind == SERVER { "-server" } else { "" };
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let mut file = format!("{}{suffix}{dot}", crate::time::stamp(now_ms));
    let mut n = 2;
    while dir.join(&file).exists() && n < 1000 {
        file = format!("{}{suffix}-{n}{dot}", crate::time::stamp(now_ms));
        n += 1;
    }
    let dest = dir.join(&file);
    let tmp = store::temp_path(&dest);
    let copied = fs::copy(src, &tmp).and_then(|size| fs::rename(&tmp, &dest).map(|_| size));
    let size = match copied {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(format!("can't copy to the backups: {e}"));
        }
    };
    let entry = Entry { file, time_ms: now_ms, size, kind: kind.to_string() };
    let doomed = store.locked(|| -> Result<Vec<String>, String> {
        let mut index = read_index(&dir);
        index.original = key.to_string();
        index.name = name.clone();
        index.entries.retain(|e| safe_file(&e.file) && dir.join(&e.file).is_file());
        index.entries.push(entry.clone());
        let delete = retain(&index.entries, now_ms, cfg.keep_versions, cfg.keep_daily_days);
        let doomed: Vec<String> = delete.iter().filter_map(|i| index.entries.get(*i).map(|e| e.file.clone())).collect();
        index.entries.retain(|e| !doomed.contains(&e.file));
        index.entries.sort_by_key(|e| e.time_ms);
        store::write_json(&dir.join("index.json"), &index, false)?;
        Ok(doomed)
    })?;
    for f in doomed {
        if safe_file(&f) {
            let _ = fs::remove_file(dir.join(f));
        }
    }
    Ok(entry)
}

/// Which entries the retention rule deletes (indexes into `entries`): everything but the
/// newest `keep` and the newest of each local day for the last `days` days. The newest entry
/// is always kept, and at most [`MAX_ENTRIES`] survive.
pub fn retain(entries: &[Entry], now_ms: i64, keep: u32, days: u32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    let time = |i: &usize| entries.get(*i).map(|e| e.time_ms).unwrap_or(0);
    order.sort_by_key(|i| std::cmp::Reverse(time(i)));
    let keep = usize::try_from(keep.max(1)).unwrap_or(usize::MAX);
    let horizon = now_ms.saturating_sub(i64::from(days).saturating_mul(86_400_000));
    let mut kept_days: Vec<i64> = Vec::new();
    let mut kept = 0usize;
    let mut delete = Vec::new();
    for (rank, i) in order.iter().enumerate() {
        let t = time(i);
        let day = crate::time::local_day(t);
        let daily = days > 0 && t >= horizon && !kept_days.contains(&day);
        if (rank < keep || daily) && kept < MAX_ENTRIES {
            kept += 1;
            if !kept_days.contains(&day) {
                kept_days.push(day);
            }
        } else {
            delete.push(*i);
        }
    }
    delete.sort_unstable();
    delete
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(i: usize, t: i64) -> Entry {
        Entry { file: format!("{i}.xlsx"), time_ms: t, size: 1, kind: SAVED.into() }
    }

    const DAY: i64 = 86_400_000;

    #[test]
    fn keeps_newest_n() {
        let now = 1_000 * DAY;
        let entries: Vec<Entry> = (0..10).map(|i| e(i, now - 10_000 + i64::try_from(i).unwrap())).collect();
        let del = retain(&entries, now, 3, 0);
        assert_eq!(del, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn keeps_one_per_day() {
        let now = 1_000 * DAY + DAY / 2;
        // Two a day for five days, newest last.
        let mut entries = Vec::new();
        for d in 0..5i64 {
            entries.push(e(entries.len(), now - d * DAY - 1000));
            entries.push(e(entries.len(), now - d * DAY - 2000));
        }
        // Keep 1 + one per day for 3 days: today's newest, plus the newest of the 2 days before
        // (days within the horizon), and nothing older.
        let del = retain(&entries, now, 1, 3);
        let kept: Vec<usize> = (0..entries.len()).filter(|i| !del.contains(i)).collect();
        assert!(kept.contains(&0));
        assert!(!kept.contains(&1));
        assert!(kept.contains(&2) && kept.contains(&4));
        assert!(!kept.contains(&8) && !kept.contains(&9));
    }

    #[test]
    fn never_deletes_the_only_copy() {
        let entries = vec![e(0, 5)];
        assert!(retain(&entries, 100 * DAY, 0, 0).is_empty());
        assert!(retain(&[], 0, 1, 1).is_empty());
    }

    #[test]
    fn backups_on_disk() {
        let d = std::env::temp_dir().join(format!("craft-nextcloud-backup-{}-{}", std::process::id(), crate::time::now_ms()));
        let store = Store::at(d.clone()).unwrap();
        let doc = d.join("Budget.xlsx");
        let cfg = Config { keep_versions: 2, keep_daily_days: 0, ..Config::default() };
        let key = doc.to_string_lossy().into_owned();
        for i in 0..4 {
            fs::write(&doc, format!("v{i}")).unwrap();
            add(&store, &key, &doc, SAVED, 1_000_000 + i, &cfg).unwrap();
        }
        let l = list(&store, &key);
        assert_eq!(l.len(), 2);
        assert_eq!(fs::read_to_string(dir_for(&store, &key).join(&l[0].file)).unwrap(), "v3");
        let files = fs::read_dir(dir_for(&store, &key)).unwrap().count();
        assert_eq!(files, 3); // two backups and the index
        let _ = fs::remove_dir_all(&d);
    }
}
