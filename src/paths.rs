//! Remote paths (Nextcloud's, `/Documents/Budget.xlsx`) and the local files they map to.
//!
//! Everything a server sends is hostile: a remote path never leaves the cache folder it's mapped
//! into (`..`, absolute paths, drive letters, NUL, reserved Windows names are refused or
//! rewritten).

use std::path::{Path, PathBuf};

/// Longest remote path accepted.
pub const MAX_REMOTE: usize = 4096;
/// Longest name of one folder or file.
pub const MAX_SEGMENT: usize = 255;

/// The add-in's data folder, shared by the three apps: `CRAFT_NEXTCLOUD_DATA` when set, else
/// `$XDG_DATA_HOME/craft-nextcloud` (`~/.local/share/craft-nextcloud`) on Linux and BSD,
/// `~/Library/Application Support/craft-nextcloud` on macOS, `%APPDATA%\craft-nextcloud` on
/// Windows.
pub fn data_dir() -> Result<PathBuf, String> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(p) = env("CRAFT_NEXTCLOUD_DATA") {
        return Ok(p);
    }
    let base = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local").join("share")))
    };
    base.map(|b| b.join("craft-nextcloud")).ok_or_else(|| "can't find a folder for the add-in's data (no HOME or APPDATA)".to_string())
}

/// A short stable id of a text (FNV-1a, 64 bits, in hex): folder names for accounts and for
/// documents' backups.
pub fn stable_id(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn bad_char(c: char) -> bool {
    c == '\0' || c.is_control()
}

/// `Documents//Budget.xlsx/` → `/Documents/Budget.xlsx`; `""` → `/`. Refuses `.`, `..`,
/// control characters and over-long paths.
pub fn normalize_remote(p: &str) -> Result<String, String> {
    if p.len() > MAX_REMOTE {
        return Err("the path is too long".into());
    }
    let mut out = String::new();
    for seg in p.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg == "." || seg == ".." {
            return Err(format!("the path “{p}” isn't allowed"));
        }
        if seg.chars().any(bad_char) || seg.contains('\\') {
            return Err("the path holds characters that aren't allowed".into());
        }
        if seg.len() > MAX_SEGMENT {
            return Err("a name in the path is too long".into());
        }
        out.push('/');
        out.push_str(seg);
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

/// The folder holding a remote path (`/a/b.xlsx` → `/a`, `/a` → `/`).
pub fn parent_remote(p: &str) -> String {
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p.get(..i).unwrap_or("/").to_string(),
    }
}

/// The last name of a remote path (`/a/b.xlsx` → `b.xlsx`).
pub fn remote_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `dir` + `name`, normalized.
pub fn join_remote(dir: &str, name: &str) -> Result<String, String> {
    if name.contains('/') || name.is_empty() {
        return Err("that name isn't allowed".into());
    }
    normalize_remote(&format!("{}/{name}", dir.trim_end_matches('/')))
}

/// Percent-encodes one path segment (everything but RFC 3986's unreserved characters).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-encodes each segment of a normalized remote path, keeping the slashes.
pub fn encode_path(p: &str) -> String {
    p.split('/').map(encode_segment).collect::<Vec<_>>().join("/")
}

/// Form-encodes a value (`application/x-www-form-urlencoded`).
pub fn form_encode(s: &str) -> String {
    encode_segment(s)
}

/// Decodes `%XX` escapes (invalid ones are kept as they are; invalid UTF-8 is replaced).
pub fn decode_percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1).and_then(|c| hex(*c)), bytes.get(i + 2).and_then(|c| hex(*c)))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

const WINDOWS_RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7",
    "lpt8", "lpt9",
];

/// One remote name made safe as a local file name on every platform: characters Windows
/// refuses become `_`, trailing dots and spaces too, reserved names get a `_` in front.
pub fn safe_segment(seg: &str) -> Result<String, String> {
    if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\0') {
        return Err("a name in the path isn't allowed".into());
    }
    let mut s: String =
        seg.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/') || c.is_control() { '_' } else { c }).collect();
    let trailing = s.chars().rev().take_while(|c| *c == '.' || *c == ' ').count();
    if trailing > 0 {
        let keep: String = s.chars().take(s.chars().count() - trailing).collect();
        s = keep + &"_".repeat(trailing);
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    if WINDOWS_RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    if s.len() > MAX_SEGMENT {
        return Err("a name in the path is too long".into());
    }
    Ok(s)
}

/// Where a remote file is kept under `base`: each segment made safe; the result is always
/// inside `base`.
pub fn safe_local(base: &Path, remote: &str) -> Result<PathBuf, String> {
    let norm = normalize_remote(remote)?;
    let mut out = base.to_path_buf();
    let mut n = 0;
    for seg in norm.split('/').filter(|s| !s.is_empty()) {
        out.push(safe_segment(seg)?);
        n += 1;
    }
    if n == 0 {
        return Err("that's a folder, not a file".into());
    }
    if !out.starts_with(base) || out.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("that path would leave the add-in's folder".into());
    }
    Ok(out)
}

/// A file name's extension in lower case (`Budget.XLSX` → `xlsx`).
pub fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// `Budget.xlsx` → (`Budget`, `.xlsx`).
pub fn split_name(name: &str) -> (&str, String) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    }
}

/// The conflict copy's name: `Budget (conflict laptop 2026-10-15 14.03).xlsx`.
pub fn conflict_name(name: &str, host: &str, when: &str) -> String {
    let (stem, ext) = split_name(name);
    format!("{stem} (conflict {host} {when}){ext}")
}

/// A restored backup's name: `Budget (backup 2026-10-15 14.03).xlsx`.
pub fn backup_copy_name(name: &str, when: &str) -> String {
    let (stem, ext) = split_name(name);
    format!("{stem} (backup {when}){ext}")
}

/// The path to a file that doesn't exist yet: `dir/name`, else `dir/name 2.ext`, `… 3` …
pub fn unique_in(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = split_name(name);
    for i in 2..1000 {
        let p = dir.join(format!("{stem} {i}{ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem} {}{ext}", crate::time::now_ms()))
}

/// The key a local document is known by in the state file: its canonical absolute path.
pub fn doc_key(path: &str) -> String {
    let p = Path::new(path);
    std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_normalize() {
        assert_eq!(normalize_remote("").unwrap(), "/");
        assert_eq!(normalize_remote("Documents//Budget.xlsx/").unwrap(), "/Documents/Budget.xlsx");
        assert!(normalize_remote("/a/../b").is_err());
        assert!(normalize_remote("/a/./b").is_err());
        assert!(normalize_remote("/a\0b").is_err());
        assert!(normalize_remote("/a\\b").is_err());
        assert!(normalize_remote(&"x".repeat(5000)).is_err());
        assert_eq!(parent_remote("/a/b.xlsx"), "/a");
        assert_eq!(parent_remote("/a"), "/");
        assert_eq!(parent_remote("/"), "/");
        assert_eq!(remote_name("/a/b.xlsx"), "b.xlsx");
        assert_eq!(join_remote("/", "x").unwrap(), "/x");
        assert!(join_remote("/", "a/b").is_err());
    }

    #[test]
    fn encoding() {
        assert_eq!(encode_path("/Docs/My Budget #1.xlsx"), "/Docs/My%20Budget%20%231.xlsx");
        assert_eq!(encode_segment("città"), "citt%C3%A0");
        assert_eq!(decode_percent("/Docs/My%20Budget%20%231.xlsx"), "/Docs/My Budget #1.xlsx");
        assert_eq!(decode_percent("citt%C3%A0%zz"), "città%zz");
    }

    #[test]
    fn local_paths_stay_inside() {
        let base = Path::new("/data/files/abc");
        assert_eq!(safe_local(base, "/Docs/Budget.xlsx").unwrap(), base.join("Docs").join("Budget.xlsx"));
        assert!(safe_local(base, "/../etc/passwd").is_err());
        assert!(safe_local(base, "/").is_err());
        assert_eq!(safe_local(base, "/C:/x.xlsx").unwrap(), base.join("C_").join("x.xlsx"));
        assert_eq!(safe_local(base, "/con.txt").unwrap(), base.join("_con.txt"));
        assert_eq!(safe_local(base, "/a?b*.xlsx").unwrap(), base.join("a_b_.xlsx"));
        assert_eq!(safe_local(base, "/trail. ").unwrap(), base.join("trail__"));
        assert!(safe_local(base, "/a\0b").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(extension("Budget.XLSX"), "xlsx");
        assert_eq!(extension(".hidden"), "");
        assert_eq!(extension("noext"), "");
        assert_eq!(conflict_name("Budget.xlsx", "laptop", "2026-10-15 14.03"), "Budget (conflict laptop 2026-10-15 14.03).xlsx");
        assert_eq!(conflict_name("README", "pc", "2026-01-01 00.00"), "README (conflict pc 2026-01-01 00.00)");
        assert_eq!(backup_copy_name("a.b.docx", "2026-10-15 14.03"), "a.b (backup 2026-10-15 14.03).docx");
        assert_eq!(stable_id("x"), stable_id("x"));
        assert_ne!(stable_id("x"), stable_id("y"));
        assert_eq!(stable_id("x").len(), 16);
    }
}
