//! The computer: opening links and folders in the user's apps, the computer's name, small
//! helper programs. Nothing here writes to standard output: that's the app's channel.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Runs a small program and returns its output (at most 4 KiB), `None` when it fails.
pub fn run_quiet(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let bytes = out.stdout.get(..out.stdout.len().min(4096))?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Opens a web link in the browser (`http`/`https` only) or a folder in the file manager.
///
/// `CRAFT_NEXTCLOUD_BROWSER` names another program to use; `none` opens nothing (tests,
/// headless runs).
pub fn open_external(target: &str, is_folder: bool) -> Result<(), String> {
    if !is_folder {
        let lower = target.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return Err("only http and https links are opened".into());
        }
    }
    if target.chars().any(|c| c.is_control()) || target.len() > 8192 {
        return Err("that address can't be opened".into());
    }
    let (program, args): (String, Vec<String>) = match std::env::var("CRAFT_NEXTCLOUD_BROWSER") {
        Ok(p) if p.trim() == "none" => return Ok(()),
        Ok(p) if !p.trim().is_empty() => (p, vec![target.to_string()]),
        _ => {
            if cfg!(target_os = "macos") {
                ("open".into(), vec![target.to_string()])
            } else if cfg!(windows) {
                if is_folder {
                    ("explorer".into(), vec![target.to_string()])
                } else {
                    // Not `cmd /c start`: cmd would read `&` in the link as a command separator.
                    ("rundll32".into(), vec!["url.dll,FileProtocolHandler".into(), target.to_string()])
                }
            } else {
                ("xdg-open".into(), vec![target.to_string()])
            }
        }
    };
    let child = Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start {program}: {e}"))?;
    // Reap it in the background so it doesn't linger.
    let _ = std::thread::Builder::new().name("opener".into()).spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// The computer's name, for conflict copies (`Budget (conflict laptop 2026-10-15 14.03).xlsx`):
/// letters, digits, `-` and `_` only, at most 40 characters.
pub fn hostname() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let raw = ["CRAFT_NEXTCLOUD_HOSTNAME", "COMPUTERNAME", "HOSTNAME"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok().filter(|v| !v.trim().is_empty()))
            .or_else(|| if cfg!(windows) { None } else { run_quiet("hostname", &[]) })
            .unwrap_or_default();
        let first = raw.trim().split('.').next().unwrap_or("").to_string();
        let clean: String = first.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(40).collect();
        if clean.is_empty() { "computer".to_string() } else { clean }
    })
    .clone()
}
