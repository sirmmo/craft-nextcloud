//! The manifest (`plugin.json`) that `--manifest` prints: one for the three apps.

use serde_json::{Value as Json, json};

/// The manifest.
pub fn manifest() -> Json {
    let cmd = |id: &str, label: &str, description: &str, params: &str| json!({"id": id, "label": label, "description": description, "params": params});
    json!({
        "id": "org.craft.nextcloud",
        "name": "Nextcloud",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Open and save Nextcloud files: edited locally, uploaded when you save (with conflict detection), backed up on your computer, locked while you edit. Needs plugin protocol 1.2.",
        "author": "Marco Montanari and the craft-nextcloud contributors",
        "homepage": "https://github.com/sirmmo/craft-nextcloud",
        "apps": ["gridcraft", "wordcraft", "deckcraft"],
        "run": {"command": "./craft-nextcloud"},
        "events": ["documentOpened", "documentSaved", "documentClosed"],
        "commands": [
            cmd("open", "Open", "Open a file from Nextcloud", "{account?, path?}"),
            cmd("browse", "Browse", "A Nextcloud folder (a file id opens it; a folder id ending in / goes into it)", "{account?, path?, id?, nav?}"),
            cmd("saveTo", "Save to Nextcloud", "Upload this document to a Nextcloud folder and keep it in sync", "{account?, path?, id?, here?, overwrite?}"),
            cmd("syncNow", "Sync Now", "Upload pending changes and check Nextcloud for a newer version", "{}"),
            cmd("openInBrowser", "Collaborate in Nextcloud", "Open this file in Nextcloud in the browser, where others can edit it with you", "{}"),
            cmd("share", "Share Link", "Create a public link to this file", "{}"),
            cmd("backups", "Backups", "This document's local backups", "{}"),
            cmd("connect", "Connect", "Sign in to a Nextcloud server", "{fields?: {server}}"),
            cmd("disconnect", "Disconnect", "Sign out of a Nextcloud account and revoke its app password", "{id?}"),
            cmd("status", "Status", "Accounts, this document's link, last upload and lock", "{}"),
            cmd("settings", "Settings", "Backups, retention, locking and upload on save", "{}"),
            cmd("finishLogin", "Finish Signing In", "Second step of Connect (from its dialog)", "{server, login, token, endpoint, reopen?}"),
            cmd("chooseAccount", "Choose Account", "Pick the account to browse (from the Open and Save dialogs)", "{then, pick?, id?}"),
            cmd("restoreBackup", "Open Backup", "Open a copy of a backup (from the Backups dialog)", "{document, id}"),
            cmd("openBackupsFolder", "Open Backups Folder", "Show the backups in the file manager", "{document?}"),
            cmd("saveSettings", "Save Settings", "Store the settings (from the Settings dialog)", "{fields}"),
            cmd("openUrl", "Open Link", "Open a web link in the browser", "{url} or {fields: {link}}"),
        ],
        "ui": {
            "ribbon": [
                {"tab": "Nextcloud", "group": "Files", "buttons": [
                    {"command": "open", "icon": "folder"},
                    {"command": "saveTo", "icon": "save"},
                    {"command": "syncNow", "icon": "refresh", "size": "small"},
                ]},
                {"tab": "Nextcloud", "group": "Collaborate", "buttons": [
                    {"command": "openInBrowser", "icon": "link"},
                    {"command": "share", "icon": "share"},
                ]},
                {"tab": "Nextcloud", "group": "Backups", "buttons": [
                    {"command": "backups", "icon": "clock"},
                ]},
                {"tab": "Nextcloud", "group": "Account", "buttons": [
                    {"command": "connect", "icon": "lock"},
                    {"command": "status", "icon": "info", "size": "small"},
                    {"command": "settings", "icon": "settings", "size": "small"},
                    {"command": "disconnect", "icon": "delete", "size": "small"},
                ]},
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn buttons_name_commands() {
        let m = super::manifest();
        let ids: Vec<&str> = m["commands"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        for g in m["ui"]["ribbon"].as_array().unwrap() {
            for b in g["buttons"].as_array().unwrap() {
                assert!(ids.contains(&b["command"].as_str().unwrap()));
            }
        }
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len());
    }
}
