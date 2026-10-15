//! The commands on the Nextcloud tab, and the dialogs they chain (protocol 1.2: a command's
//! answer may hold a `dialog`, whose OK and buttons run another command, or an `open`).

use std::fs;
use std::path::Path;

use serde_json::{Value as Json, json};

use crate::backup;
use crate::dav::{self, Client, Item};
use crate::paths;
use crate::plugin::{App, Fail, Session, fail};
use crate::store::{Account, Config, Link, Store};
use crate::sync::{self, Upload};
use crate::text::{Lang, tr, trf};
use crate::time;

/// Items in one list dialog at most (the host's limit).
const MAX_DIALOG_ITEMS: usize = 10_000;

/// The document the command runs on: `params.document` (set by an earlier step of a dialog
/// chain), else `context.document`.
struct Doc {
    path: Option<String>,
    dirty: bool,
}

fn document(params: &Json, ctx: &Json) -> Doc {
    if let Some(p) = params.get("document").and_then(Json::as_str).filter(|p| !p.is_empty()) {
        return Doc { path: Some(p.to_string()), dirty: false };
    }
    let d = ctx.get("document");
    Doc {
        path: d.and_then(|d| d.get("path")).and_then(Json::as_str).filter(|p| !p.is_empty() && p.len() <= 32_768 && !p.contains('\0')).map(str::to_string),
        dirty: d.and_then(|d| d.get("dirty")).and_then(Json::as_bool).unwrap_or(false),
    }
}

fn str_param<'a>(params: &'a Json, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Json::as_str).filter(|s| !s.is_empty())
}

fn field<'a>(params: &'a Json, key: &str) -> Option<&'a Json> {
    params.get("fields").and_then(|f| f.get(key))
}

/// A message dialog (OK closes it).
fn notice(lang: Lang, text: impl Into<String>) -> Json {
    json!({"dialog": {"title": tr(lang, "title.nextcloud"), "kind": "message", "prompt": clip(&text.into(), 4000)}})
}

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn bytes_text(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.0} KB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MB", f / K / K)
    } else {
        format!("{:.1} GB", f / K / K / K)
    }
}

/// Runs a command.
pub fn run(s: &mut Session, command: &str, params: &Json, ctx: &Json) -> Result<Json, Fail> {
    let lang = s.lang;
    // Everything here talks through dialogs: an app older than protocol 1.2 can't show them.
    if s.features.is_some() && !s.has("dialogs") {
        return Err(fail(tr(lang, "noFeatures")));
    }
    let store = s.store().map_err(fail)?;
    let app = s.app;
    let c = Cmd { lang, app, store, params, ctx };
    match command {
        "connect" => c.connect(),
        "finishLogin" => c.finish_login(),
        "disconnect" => c.disconnect(),
        "open" | "browse" => c.browse(),
        "chooseAccount" => c.choose_account(),
        "saveTo" => c.save_to(),
        "backups" => c.backups(),
        "restoreBackup" => c.restore_backup(),
        "openBackupsFolder" => c.open_backups_folder(),
        "settings" => Ok(c.settings_form()),
        "saveSettings" => c.save_settings(),
        "openInBrowser" => c.open_in_browser(),
        "share" => c.share(),
        "openUrl" => c.open_url(),
        "status" => Ok(c.status()),
        "syncNow" => Ok(c.sync_now()),
        other => Err((-32601, format!("unknown command {other}"))),
    }
}

struct Cmd<'a> {
    lang: Lang,
    app: App,
    store: Store,
    params: &'a Json,
    ctx: &'a Json,
}

impl Cmd<'_> {
    fn t(&self, key: &'static str) -> &'static str {
        tr(self.lang, key)
    }

    fn tf(&self, key: &'static str, args: &[&str]) -> String {
        trf(self.lang, key, args)
    }

    fn error_notice(&self, e: &str) -> Json {
        notice(self.lang, self.tf("error", &[e]))
    }

    // ----- Accounts -----

    fn connect_form(&self, prompt: String, value: &str) -> Json {
        json!({"dialog": {
            "title": self.t("title.connect"),
            "kind": "form",
            "prompt": clip(&prompt, 4000),
            "fields": [{"id": "server", "label": self.t("field.server"), "value": value, "kind": "text"}],
            "command": "connect",
            "okLabel": self.t("btn.connect"),
        }})
    }

    /// `connect`: the form, then (with `fields.server`) Login Flow v2 in the browser.
    fn connect(&self) -> Result<Json, Fail> {
        let Some(input) = field(self.params, "server").and_then(Json::as_str).or_else(|| str_param(self.params, "server")) else {
            return Ok(self.connect_form(self.t("connect.prompt").to_string(), ""));
        };
        let server = match dav::normalize_server(input) {
            Ok(s) => s,
            Err(e) => return Ok(self.connect_form(self.tf("connect.badServer", &[input, &e]), input)),
        };
        let (login, token, endpoint) = match dav::login_start(&server) {
            Ok(x) => x,
            Err(e) => return Ok(self.connect_form(self.tf("connect.badServer", &[input, &e.text()]), input)),
        };
        if let Err(e) = crate::system::open_external(&login, false) {
            eprintln!("craft-nextcloud: couldn't open the browser: {e}");
        }
        Ok(self.waiting_dialog(&server, &login, &token, &endpoint, self.tf("connect.browser", &[&login])))
    }

    fn waiting_dialog(&self, server: &str, login: &str, token: &str, endpoint: &str, prompt: String) -> Json {
        let p = json!({"server": server, "login": login, "token": token, "endpoint": endpoint});
        let mut again = p.clone();
        if let Some(o) = again.as_object_mut() {
            o.insert("reopen".into(), json!(true));
        }
        json!({
            "dialog": {
                "title": self.t("title.connect"),
                "kind": "message",
                "prompt": clip(&prompt, 4000),
                "command": "finishLogin",
                "params": p,
                "okLabel": self.t("btn.continue"),
                "buttons": [{"label": self.t("btn.openAgain"), "command": "finishLogin", "params": again}],
            },
            "result": {"loginUrl": login},
        })
    }

    /// `finishLogin {server, login, token, endpoint, reopen?}`: polls Login Flow v2; saves the
    /// account once the user signed in.
    fn finish_login(&self) -> Result<Json, Fail> {
        let get = |k: &str| str_param(self.params, k).ok_or_else(|| (-32602, format!("`{k}` is required")));
        let (server, login, token, endpoint) = (get("server")?, get("login")?, get("token")?, get("endpoint")?);
        if self.params.get("reopen").and_then(Json::as_bool) == Some(true) {
            let _ = crate::system::open_external(login, false);
            return Ok(self.waiting_dialog(server, login, token, endpoint, self.tf("connect.browser", &[login])));
        }
        let mut answer = dav::login_poll(endpoint, token);
        if matches!(answer, Ok(None)) {
            std::thread::sleep(std::time::Duration::from_secs(1));
            answer = dav::login_poll(endpoint, token);
        }
        let (srv, login_name, password) = match answer {
            Ok(Some(x)) => x,
            Ok(None) => return Ok(self.waiting_dialog(server, login, token, endpoint, self.tf("connect.waiting", &[server]))),
            Err(dav::Fail::Status(410 | 403, _)) => return Ok(self.connect_form(self.t("connect.expired").to_string(), server)),
            Err(e) => {
                return Ok(self.waiting_dialog(
                    server,
                    login,
                    token,
                    endpoint,
                    format!("{}\n\n{}", self.tf("error", &[&e.text()]), self.tf("connect.waiting", &[server])),
                ));
            }
        };
        // The server's own idea of its address wins (it may add a path), if it's a valid one.
        let server = dav::normalize_server(&srv).unwrap_or_else(|_| server.to_string());
        let mut acc = Account {
            id: paths::stable_id(&format!("{server}\n{login_name}")),
            server: server.clone(),
            login_name: login_name.clone(),
            user_id: String::new(),
            app_password: password,
            added_ms: time::now_ms(),
        };
        acc.user_id = Client::new(&acc, dav::COMMAND).user_id().unwrap_or_else(|_| login_name.clone());
        let saved = acc.clone();
        self.store
            .update_accounts(move |list| {
                list.retain(|a| a.id != saved.id);
                list.insert(0, saved);
            })
            .map_err(fail)?;
        Ok(json!({"message": self.tf("connect.done", &[&server, &login_name]), "result": {"account": acc.id}}))
    }

    /// `disconnect`: a list of the accounts; picking one revokes its app password, gives back
    /// its locks and forgets it (its cached files stay).
    fn disconnect(&self) -> Result<Json, Fail> {
        let accounts = self.store.accounts();
        let Some(id) = str_param(self.params, "id") else {
            if accounts.is_empty() {
                return Ok(notice(self.lang, self.t("notConnected")));
            }
            let items: Vec<Json> =
                accounts.iter().map(|a| json!({"id": a.id, "label": clip(&a.label(), 256), "detail": clip(&a.server, 256), "icon": "lock"})).collect();
            return Ok(json!({"dialog": {
                "title": self.t("title.disconnect"), "kind": "list", "prompt": self.t("disconnect.prompt"),
                "items": items, "command": "disconnect", "okLabel": self.t("btn.disconnect"),
            }}));
        };
        let Some(acc) = accounts.into_iter().find(|a| a.id == id) else { return Ok(notice(self.lang, self.t("notConnected"))) };
        let c = Client::new(&acc, dav::EVENT);
        let mine: Vec<(String, Link)> = self.store.state().links.into_iter().filter(|(_, l)| l.account == acc.id).collect();
        for (_, l) in &mine {
            if !l.lock_token.is_empty() {
                let _ = c.unlock(&l.remote, &l.lock_token);
            }
        }
        if let Err(e) = c.revoke() {
            eprintln!("craft-nextcloud: couldn't revoke the app password: {}", e.text());
        }
        let gone = acc.id.clone();
        self.store.update_state(|s| s.links.retain(|_, l| l.account != gone)).map_err(fail)?;
        let gone = acc.id.clone();
        self.store.update_accounts(move |list| list.retain(|a| a.id != gone)).map_err(fail)?;
        Ok(json!({"message": self.tf("disconnected", &[&acc.label()])}))
    }

    /// The account named in `params.account`, else the first.
    fn account(&self) -> Option<Account> {
        let list = self.store.accounts();
        match str_param(self.params, "account") {
            Some(id) => list.iter().find(|a| a.id == id).cloned().or_else(|| list.first().cloned()),
            None => list.first().cloned(),
        }
    }

    /// `chooseAccount {then}`: a list of the accounts; picking one (`pick`) goes on to `then`
    /// (`browse` or `saveTo`) in that account.
    fn choose_account(&self) -> Result<Json, Fail> {
        let then = match str_param(self.params, "then") {
            Some("saveTo") => "saveTo",
            _ => "browse",
        };
        let document = self.params.get("document").cloned().unwrap_or(Json::Null);
        if self.params.get("pick").and_then(Json::as_bool) == Some(true)
            && let Some(id) = str_param(self.params, "id")
        {
            let mut p = json!({"account": id, "path": "/", "document": document});
            if then == "saveTo" {
                return self.with_params(&p, |c| c.save_to());
            }
            if let Some(o) = p.as_object_mut() {
                o.remove("document");
            }
            return self.with_params(&p, |c| c.browse());
        }
        let accounts = self.store.accounts();
        if accounts.is_empty() {
            return Ok(self.connect_form(self.t("notConnected").to_string(), ""));
        }
        let items: Vec<Json> =
            accounts.iter().map(|a| json!({"id": a.id, "label": clip(&a.label(), 256), "detail": clip(&a.server, 256), "icon": "folder"})).collect();
        Ok(json!({"dialog": {
            "title": self.t("title.accounts"), "kind": "list", "prompt": self.t("accounts.prompt"), "items": items,
            "command": "chooseAccount", "params": {"then": then, "pick": true, "document": document},
            "okLabel": self.t("btn.choose"),
            "buttons": [{"label": self.t("btn.addAccount"), "command": "connect"}],
        }}))
    }

    fn with_params(&self, p: &Json, f: impl FnOnce(&Cmd) -> Result<Json, Fail>) -> Result<Json, Fail> {
        let c = Cmd { lang: self.lang, app: self.app, store: self.store.clone(), params: p, ctx: self.ctx };
        f(&c)
    }

    // ----- Browsing and opening -----

    /// A folder's items: folders first, then files (only those this app opens unless
    /// `folders_only`), sorted by name; the folder itself left out.
    fn listing(&self, c: &Client, path: &str, folders_only: bool) -> Result<(Vec<Item>, bool), dav::Fail> {
        let mut items: Vec<Item> = c.propfind(path, 1)?.into_iter().filter(|i| i.path != path && i.path != "/").collect();
        items.retain(|i| i.folder || (!folders_only && self.app.opens(&i.name)));
        items.sort_by(|a, b| b.folder.cmp(&a.folder).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        let truncated = items.len() > MAX_DIALOG_ITEMS;
        items.truncate(MAX_DIALOG_ITEMS);
        Ok((items, truncated))
    }

    fn browse_buttons(&self, acc: &Account, path: &str, then: &str, extra: Option<Json>) -> Vec<Json> {
        let mut buttons = Vec::new();
        if let Some(e) = extra {
            buttons.push(e);
        }
        let document = self.params.get("document").cloned().unwrap_or(Json::Null);
        if path != "/" {
            buttons.push(json!({"label": self.t("btn.up"), "command": then, "params": {"account": acc.id, "path": paths::parent_remote(path), "nav": true, "document": document}}));
        }
        if self.store.accounts().len() > 1 {
            buttons.push(json!({"label": self.t("btn.switchAccount"), "command": "chooseAccount", "params": {"then": then, "document": document}}));
        }
        buttons
    }

    /// `open` / `browse {account?, path?, id?, nav?}`: a list of a folder. An `id` ending in `/`
    /// is a folder to go into; another `id` is a file to open (unless `nav`: a button).
    fn browse(&self) -> Result<Json, Fail> {
        let Some(acc) = self.account() else { return Ok(self.connect_form(self.t("notConnected").to_string(), "")) };
        let nav = self.params.get("nav").and_then(Json::as_bool) == Some(true);
        let mut path = str_param(self.params, "path").unwrap_or("/").to_string();
        if !nav && let Some(id) = str_param(self.params, "id") {
            if id.ends_with('/') {
                path = id.to_string();
            } else {
                return self.open_file(&acc, id);
            }
        }
        let path = paths::normalize_remote(&path).map_err(fail)?;
        let c = Client::new(&acc, dav::COMMAND);
        let (items, truncated) = match self.listing(&c, &path, false) {
            Ok(x) => x,
            Err(e) => return Ok(self.error_notice(&e.text())),
        };
        let list: Vec<Json> = items
            .iter()
            .map(|i| {
                if i.folder {
                    json!({"id": format!("{}/", i.path), "label": clip(&i.name, 256), "detail": self.t("folder"), "icon": "folder"})
                } else {
                    let mut detail = i.size.map(bytes_text).unwrap_or_default();
                    if let Some(m) = i.modified {
                        if !detail.is_empty() {
                            detail.push_str(" · ");
                        }
                        detail.push_str(&time::display(m.saturating_mul(1000)));
                    }
                    json!({"id": i.path, "label": clip(&i.name, 256), "detail": detail, "icon": self.app.file_icon()})
                }
            })
            .collect();
        let place = format!("{} · {path}", acc.label());
        let prompt = if list.is_empty() {
            self.tf("emptyFolder", &[&place])
        } else if truncated {
            self.tf("truncated", &[&place, &MAX_DIALOG_ITEMS.to_string()])
        } else {
            place
        };
        Ok(json!({"dialog": {
            "title": self.t("title.open"), "kind": "list", "prompt": clip(&prompt, 4000), "items": list,
            "command": "browse", "params": {"account": acc.id, "path": path},
            "okLabel": self.t("btn.open"),
            "buttons": self.browse_buttons(&acc, &path, "browse", None),
        }}))
    }

    /// Downloads a Nextcloud file into the cache, links it, backs it up, locks it, and asks the
    /// app to open it.
    fn open_file(&self, acc: &Account, remote: &str) -> Result<Json, Fail> {
        let remote = paths::normalize_remote(remote).map_err(fail)?;
        let name = paths::remote_name(&remote).to_string();
        if !self.app.opens(&name) {
            return Ok(notice(self.lang, self.tf("notOpened", &[&paths::extension(&name)])));
        }
        let local = paths::safe_local(&self.store.files_dir(&acc.id), &remote).map_err(fail)?;
        let local_text = local.to_string_lossy().into_owned();
        let label = clip(&format!("Nextcloud · {}", remote.trim_start_matches('/')), 200);
        let key = paths::doc_key(&local_text);
        // Never overwrite changes that haven't reached the server.
        if let Some(l) = self.store.link(&key)
            && l.pending
            && local.is_file()
        {
            return Ok(json!({"open": {"path": key, "label": label, "readOnly": false}, "message": self.t("opened.pending")}));
        }
        let c = Client::new(acc, dav::COMMAND);
        let item = match c.stat(&remote) {
            Ok(i) => i,
            Err(e) => return Ok(self.error_notice(&e.text())),
        };
        if item.folder {
            let p = json!({"account": acc.id, "path": remote});
            return self.with_params(&p, |c| c.browse());
        }
        let etag = match c.download(&remote, &local) {
            Ok(e) => e,
            Err(e) => return Ok(self.error_notice(&e.text())),
        };
        let key = paths::doc_key(&local_text);
        let etag = if etag.is_empty() { item.etag.clone() } else { etag };
        let cfg = self.store.config();
        if let Err(e) = backup::add(&self.store, &key, &local, backup::DOWNLOADED, time::now_ms(), &cfg) {
            eprintln!("craft-nextcloud: no backup of the download: {e}");
        }
        let writable = item.writable();
        let link = Link { account: acc.id.clone(), remote: remote.clone(), etag, fileid: item.fileid.clone(), writable, ..Link::default() };
        let k = key.clone();
        self.store
            .update_state(move |s| {
                // Keep a lock we still hold on this same file.
                let old = s.links.remove(&k);
                let mut link = link;
                if let Some(o) = old.filter(|o| o.remote == link.remote && !o.lock_token.is_empty()) {
                    link.lock_token = o.lock_token;
                    link.lock = o.lock;
                }
                s.links.insert(k, link);
            })
            .map_err(fail)?;
        sync::take_lock(&self.store, &cfg, &key, dav::COMMAND);
        Ok(json!({"open": {"path": key, "label": label, "readOnly": !writable}}))
    }

    // ----- Saving to Nextcloud -----

    /// `saveTo {account?, path?, id?, here?, overwrite?}`: picks a folder, uploads the current
    /// document there and links it (the local file stays where it is).
    fn save_to(&self) -> Result<Json, Fail> {
        let doc = document(self.params, self.ctx);
        let Some(local) = doc.path.filter(|_| !doc.dirty) else { return Ok(notice(self.lang, self.t("saveFirst"))) };
        let key = paths::doc_key(&local);
        if !Path::new(&key).is_file() {
            return Ok(notice(self.lang, self.t("saveFirst")));
        }
        let Some(acc) = self.account() else { return Ok(self.connect_form(self.t("notConnected").to_string(), "")) };
        let nav = self.params.get("nav").and_then(Json::as_bool) == Some(true);
        let here = self.params.get("here").and_then(Json::as_bool) == Some(true);
        let mut path = str_param(self.params, "path").unwrap_or("/").to_string();
        if !nav
            && !here
            && let Some(id) = str_param(self.params, "id")
        {
            path = id.to_string();
        }
        let path = paths::normalize_remote(&path).map_err(fail)?;
        let c = Client::new(&acc, dav::COMMAND);
        if here {
            return self.upload_new(&acc, &c, &key, &path);
        }
        let (items, truncated) = match self.listing(&c, &path, true) {
            Ok(x) => x,
            Err(e) => return Ok(self.error_notice(&e.text())),
        };
        let list: Vec<Json> =
            items.iter().map(|i| json!({"id": format!("{}/", i.path), "label": clip(&i.name, 256), "detail": self.t("folder"), "icon": "folder"})).collect();
        let place = format!("{} · {path}", acc.label());
        let prompt = if truncated { self.tf("truncated", &[&place, &MAX_DIALOG_ITEMS.to_string()]) } else { self.tf("pickFolder", &[&place]) };
        let save_here =
            json!({"label": self.t("btn.saveHere"), "command": "saveTo", "params": {"account": acc.id, "path": path, "here": true, "document": key}});
        Ok(json!({"dialog": {
            "title": self.t("title.saveTo"), "kind": "list", "prompt": clip(&prompt, 4000), "items": list,
            "command": "saveTo", "params": {"account": acc.id, "path": path, "document": key},
            "okLabel": self.t("btn.open"),
            "buttons": self.browse_buttons(&acc, &path, "saveTo", Some(save_here)),
        }}))
    }

    fn upload_new(&self, acc: &Account, c: &Client, key: &str, folder: &str) -> Result<Json, Fail> {
        let name = Path::new(key).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let remote = paths::join_remote(folder, &name).map_err(fail)?;
        let overwrite = self.params.get("overwrite").and_then(Json::as_bool) == Some(true);
        let size = fs::metadata(key).map(|m| m.len()).unwrap_or(0);
        if size > dav::MAX_UPLOAD {
            return Ok(self.error_notice("the file is too large to upload (over 1 GiB)"));
        }
        let body = fs::read(key).map_err(|e| fail(format!("can't read {key}: {e}")))?;
        let u = match c.upload(&remote, body, None, !overwrite, None) {
            Ok(u) => u,
            Err(dav::Fail::Status(412, _)) => {
                return Ok(json!({"dialog": {
                    "title": self.t("title.saveTo"), "kind": "message",
                    "prompt": self.tf("exists", &[&name, folder]),
                    "command": "saveTo",
                    "params": {"account": acc.id, "path": folder, "here": true, "overwrite": true, "document": key},
                    "okLabel": self.t("btn.replace"),
                }}));
            }
            Err(e) => return Ok(self.error_notice(&e.text())),
        };
        let (etag, fileid) = if u.etag.is_empty() || u.fileid.is_empty() {
            match c.stat(&remote) {
                Ok(i) => (if u.etag.is_empty() { i.etag } else { u.etag }, if u.fileid.is_empty() { i.fileid } else { u.fileid }),
                Err(_) => (u.etag, u.fileid),
            }
        } else {
            (u.etag, u.fileid)
        };
        let link = Link { account: acc.id.clone(), remote: remote.clone(), etag, fileid, writable: true, last_upload_ms: time::now_ms(), ..Link::default() };
        let k = key.to_string();
        let old_lock = self.store.link(key).filter(|l| !l.lock_token.is_empty());
        if let Some(o) = old_lock
            && let Some(a) = sync::account(&self.store, &o.account)
        {
            let _ = Client::new(&a, dav::EVENT).unlock(&o.remote, &o.lock_token);
        }
        self.store
            .update_state(move |s| {
                s.links.insert(k, link);
            })
            .map_err(fail)?;
        sync::take_lock(&self.store, &self.store.config(), key, dav::COMMAND);
        Ok(json!({"message": self.tf("savedTo", &[remote.trim_start_matches('/')]), "result": {"remote": remote}}))
    }

    // ----- Backups -----

    fn doc_key_or_notice(&self) -> Result<String, Json> {
        let doc = document(self.params, self.ctx);
        doc.path.map(|p| paths::doc_key(&p)).ok_or_else(|| notice(self.lang, self.t("saveFirst")))
    }

    /// `backups`: the current document's backups, newest first.
    fn backups(&self) -> Result<Json, Fail> {
        let key = match self.doc_key_or_notice() {
            Ok(k) => k,
            Err(n) => return Ok(n),
        };
        let folder_button = json!({"label": self.t("btn.backupsFolder"), "command": "openBackupsFolder", "params": {"document": key}});
        let entries = backup::list(&self.store, &key);
        if entries.is_empty() {
            return Ok(json!({"dialog": {"title": self.t("title.backups"), "kind": "message", "prompt": self.t("noBackups"), "buttons": [folder_button]}}));
        }
        let items: Vec<Json> = entries
            .iter()
            .take(MAX_DIALOG_ITEMS)
            .map(|e| {
                let kind = match e.kind.as_str() {
                    backup::DOWNLOADED => self.t("kind.downloaded"),
                    backup::SERVER => self.t("kind.server"),
                    _ => self.t("kind.saved"),
                };
                json!({"id": e.file, "label": time::display_seconds(e.time_ms), "detail": format!("{} · {kind}", bytes_text(e.size)), "icon": "clock"})
            })
            .collect();
        let name = Path::new(&key).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(json!({"dialog": {
            "title": self.t("title.backups"), "kind": "list", "prompt": clip(&self.tf("backups.prompt", &[&name]), 4000), "items": items,
            "command": "restoreBackup", "params": {"document": key}, "okLabel": self.t("btn.restore"),
            "buttons": [folder_button],
        }}))
    }

    /// `restoreBackup {document, id}`: copies a backup to `restored/` and opens the copy.
    fn restore_backup(&self) -> Result<Json, Fail> {
        let key = match self.doc_key_or_notice() {
            Ok(k) => k,
            Err(n) => return Ok(n),
        };
        let id = str_param(self.params, "id").ok_or((-32602, "`id` is required".to_string()))?;
        let Some(entry) = backup::list(&self.store, &key).into_iter().find(|e| e.file == id) else {
            return Ok(notice(self.lang, self.t("noBackups")));
        };
        let src = backup::dir_for(&self.store, &key).join(&entry.file);
        let name = Path::new(&key).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| entry.file.clone());
        let dir = self.store.restored_dir();
        fs::create_dir_all(&dir).map_err(|e| fail(format!("can't create {}: {e}", dir.display())))?;
        let copy_name = paths::safe_segment(&paths::backup_copy_name(&name, &time::file_display(entry.time_ms))).map_err(fail)?;
        let dest = paths::unique_in(&dir, &copy_name);
        fs::copy(&src, &dest).map_err(|e| fail(format!("can't copy the backup: {e}")))?;
        let dest = dest.to_string_lossy().into_owned();
        Ok(json!({"open": {"path": dest, "label": clip(&self.tf("backupOf", &[&name, &time::display(entry.time_ms)]), 200), "readOnly": false}}))
    }

    /// `openBackupsFolder {document?}`: the document's backups folder in the file manager.
    fn open_backups_folder(&self) -> Result<Json, Fail> {
        let mut dir = self.store.backups_dir();
        if let Ok(key) = self.doc_key_or_notice() {
            let d = backup::dir_for(&self.store, &key);
            if d.is_dir() {
                dir = d;
            }
        }
        fs::create_dir_all(&dir).map_err(|e| fail(e.to_string()))?;
        let text = dir.to_string_lossy().into_owned();
        crate::system::open_external(&text, true).map_err(fail)?;
        Ok(json!({"result": {"path": text}}))
    }

    // ----- Settings -----

    fn settings_form(&self) -> Json {
        let c = self.store.config();
        json!({"dialog": {
            "title": self.t("title.settings"), "kind": "form",
            "fields": [
                {"id": "backupOnSave", "label": self.t("field.backupOnSave"), "kind": "checkbox", "value": c.backup_on_save},
                {"id": "keepVersions", "label": self.t("field.keepVersions"), "kind": "number", "value": c.keep_versions},
                {"id": "keepDailyDays", "label": self.t("field.keepDailyDays"), "kind": "number", "value": c.keep_daily_days},
                {"id": "lockWhileEditing", "label": self.t("field.lock"), "kind": "checkbox", "value": c.lock_while_editing},
                {"id": "uploadOnSave", "label": self.t("field.upload"), "kind": "checkbox", "value": c.upload_on_save},
            ],
            "command": "saveSettings", "okLabel": self.t("btn.save"),
        }})
    }

    /// `saveSettings {fields}`.
    fn save_settings(&self) -> Result<Json, Fail> {
        let mut c: Config = self.store.config();
        let flag = |k: &str, cur: bool| match field(self.params, k) {
            Some(Json::Bool(b)) => *b,
            Some(Json::String(s)) => matches!(s.as_str(), "true" | "1" | "on" | "yes"),
            Some(Json::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
            _ => cur,
        };
        c.backup_on_save = flag("backupOnSave", c.backup_on_save);
        c.lock_while_editing = flag("lockWhileEditing", c.lock_while_editing);
        c.upload_on_save = flag("uploadOnSave", c.upload_on_save);
        for (k, label, lo, hi) in [("keepVersions", "field.keepVersions", 1u32, 10_000u32), ("keepDailyDays", "field.keepDailyDays", 0, 3650)] {
            let Some(v) = field(self.params, k) else { continue };
            let n = match v {
                Json::Number(n) => n.as_f64(),
                Json::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            let ok = n.filter(|n| n.fract() == 0.0 && *n >= f64::from(lo) && *n <= f64::from(hi)).map(|n| n as u32);
            let Some(n) = ok else {
                let mut form = self.settings_form();
                if let Some(d) = form.get_mut("dialog").and_then(Json::as_object_mut) {
                    d.insert("prompt".into(), json!(self.tf("settings.bad", &[self.t(label), &lo.to_string(), &hi.to_string()])));
                }
                return Ok(form);
            };
            if k == "keepVersions" {
                c.keep_versions = n;
            } else {
                c.keep_daily_days = n;
            }
        }
        self.store.save_config(&c).map_err(fail)?;
        Ok(json!({"message": self.t("settings.saved")}))
    }

    // ----- Sharing and collaborating -----

    /// The current document's link and account, or a notice saying why there's none.
    fn linked(&self) -> Result<(String, Link, Account), Json> {
        let key = self.doc_key_or_notice()?;
        let link = self.store.link(&key).ok_or_else(|| notice(self.lang, self.t("notLinked")))?;
        let acc = sync::account(&self.store, &link.account).ok_or_else(|| notice(self.lang, self.t("notConnected")))?;
        Ok((key, link, acc))
    }

    /// `openInBrowser`: the file in Nextcloud's web interface, where others can co-edit it.
    fn open_in_browser(&self) -> Result<Json, Fail> {
        let (key, link, acc) = match self.linked() {
            Ok(x) => x,
            Err(n) => return Ok(n),
        };
        let mut fileid = link.fileid.clone();
        if fileid.is_empty() {
            match Client::new(&acc, dav::COMMAND).stat(&link.remote) {
                Ok(i) => {
                    fileid = i.fileid.clone();
                    let f = i.fileid;
                    let _ = self.store.update_link(&key, |l| l.fileid = f);
                }
                Err(e) => return Ok(self.error_notice(&e.text())),
            }
        }
        let url = if fileid.chars().all(|c| c.is_ascii_alphanumeric()) && !fileid.is_empty() {
            format!("{}/index.php/f/{}", acc.server, fileid)
        } else {
            format!("{}/index.php/apps/files/?dir={}", acc.server, paths::form_encode(&paths::parent_remote(&link.remote)))
        };
        if let Err(e) = crate::system::open_external(&url, false) {
            return Ok(self.error_notice(&e));
        }
        Ok(json!({"message": self.t("browserOpened"), "result": {"url": url}}))
    }

    /// `share`: a public link to the file, shown in a form so it can be copied.
    fn share(&self) -> Result<Json, Fail> {
        let (_, link, acc) = match self.linked() {
            Ok(x) => x,
            Err(n) => return Ok(n),
        };
        let url = match Client::new(&acc, dav::COMMAND).share_link(&link.remote) {
            Ok(u) => u,
            Err(e) => return Ok(self.error_notice(&e.text())),
        };
        Ok(json!({
            "dialog": {
                "title": self.t("title.share"), "kind": "form",
                "prompt": clip(&self.tf("share.prompt", &[paths::remote_name(&link.remote)]), 4000),
                "fields": [{"id": "link", "label": self.t("field.link"), "kind": "text", "value": url}],
                "command": "openUrl", "okLabel": self.t("btn.openLink"),
            },
            "result": {"url": url},
        }))
    }

    /// `openUrl {url}` (or `fields.link`): an http(s) link in the browser.
    fn open_url(&self) -> Result<Json, Fail> {
        let url =
            field(self.params, "link").and_then(Json::as_str).or_else(|| str_param(self.params, "url")).ok_or((-32602, "`url` is required".to_string()))?;
        crate::system::open_external(url.trim(), false).map_err(fail)?;
        Ok(json!({}))
    }

    // ----- Status and sync -----

    fn status(&self) -> Json {
        let mut lines = vec![self.t("status.accounts").to_string()];
        let accounts = self.store.accounts();
        if accounts.is_empty() {
            lines.push(self.t("status.none").to_string());
        }
        for a in &accounts {
            lines.push(format!("  {}", a.label()));
        }
        lines.push(String::new());
        let doc = document(self.params, self.ctx);
        match doc.path.map(|p| paths::doc_key(&p)) {
            None => lines.push(self.t("status.noDocument").to_string()),
            Some(key) => match self.store.link(&key) {
                None => lines.push(self.t("status.notLinked").to_string()),
                Some(l) => {
                    let acc = accounts.iter().find(|a| a.id == l.account).map(Account::label).unwrap_or_else(|| "?".into());
                    lines.push(self.tf("status.linked", &[l.remote.trim_start_matches('/'), &acc]));
                    if !l.conflict_of.is_empty() {
                        lines.push(self.tf("status.conflictOf", &[l.conflict_of.trim_start_matches('/')]));
                    }
                    let last = if l.last_upload_ms > 0 { time::display(l.last_upload_ms) } else { self.t("status.never").to_string() };
                    lines.push(self.tf("status.lastUpload", &[&last]));
                    if l.pending {
                        lines.push(self.tf("status.pending", &[&l.pending_reason]));
                    }
                    lines.push(match l.lock.as_str() {
                        "locked" => self.t("status.lock.locked").to_string(),
                        "unavailable" => self.tf("status.lock.unavailable", &[&l.lock_note]),
                        "other" => self.t("status.lock.other").to_string(),
                        "off" => self.t("status.lock.off").to_string(),
                        _ => self.t("status.lock.none").to_string(),
                    });
                }
            },
        }
        let c = self.store.config();
        let on = |b: bool| if b { self.t("on") } else { self.t("off") };
        lines.push(String::new());
        lines.push(self.tf("status.settings", &[on(c.backup_on_save), on(c.upload_on_save), on(c.lock_while_editing)]));
        json!({"dialog": {"title": self.t("title.status"), "kind": "message", "prompt": clip(&lines.join("\n"), 4000)}})
    }

    /// `syncNow`: uploads every pending document (the current one first), and checks whether
    /// the current one changed on the server.
    fn sync_now(&self) -> Json {
        let current = document(self.params, self.ctx).path.map(|p| paths::doc_key(&p));
        let mut keys: Vec<String> = self.store.state().links.into_iter().filter(|(_, l)| l.pending).map(|(k, _)| k).collect();
        if let Some(cur) = &current {
            keys.retain(|k| k != cur);
            if self.store.link(cur).is_some_and(|l| l.pending) {
                keys.insert(0, cur.clone());
            }
        }
        let start = std::time::Instant::now();
        let (mut done, mut left) = (0usize, 0usize);
        let mut notes = Vec::new();
        for k in keys.iter().take(50) {
            // Stay well inside the app's 60 s.
            if start.elapsed() > std::time::Duration::from_secs(35) {
                left += 1;
                continue;
            }
            match sync::upload(&self.store, k, dav::EVENT, self.lang) {
                Upload::Done | Upload::Reuploaded(_) => done += 1,
                u @ Upload::Conflict { .. } => {
                    done += 1;
                    notes.push(crate::events::upload_message(self.lang, u));
                }
                Upload::Pending(_) => left += 1,
            }
        }
        left += keys.len().saturating_sub(50);
        if let Some(cur) = &current
            && let Some(l) = self.store.link(cur)
            && !l.pending
            && !keys.contains(cur)
            && let Some(acc) = sync::account(&self.store, &l.account)
        {
            match Client::new(&acc, dav::EVENT).stat(&l.remote) {
                Ok(i) if !dav::same_etag(&i.etag, &l.etag) => notes.push(self.tf("changedRemote", &[paths::remote_name(&l.remote)])),
                Ok(_) => notes.push(self.t("upToDate").to_string()),
                Err(e) => notes.push(self.tf("error", &[&e.text()])),
            }
        } else if current.as_ref().is_some_and(|c| self.store.link(c).is_none()) && keys.is_empty() {
            notes.push(self.t("notLinked").to_string());
        }
        if done > 0 {
            notes.insert(0, self.tf("synced", &[&done.to_string()]));
        }
        if left > 0 {
            notes.push(self.tf("stillPending", &[&left.to_string()]));
        }
        if notes.is_empty() {
            notes.push(self.t("upToDate").to_string());
        }
        json!({"message": clip(&notes.join(" · "), 4000), "result": {"uploaded": done, "pending": left}})
    }
}
