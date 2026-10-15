//! The add-in end to end: the `craft-nextcloud` binary driven over its standard input and
//! output exactly as an app drives it, against a fake Nextcloud (WebDAV, Login Flow v2, OCS)
//! served by the test.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value as Json, json};

// ----- The fake Nextcloud -----

#[derive(Default)]
struct Cloud {
    base: String,
    files: BTreeMap<String, (Vec<u8>, u64, u64)>,
    folders: BTreeSet<String>,
    locks: BTreeMap<String, String>,
    lock_supported: bool,
    offline: bool,
    poll_pending: u32,
    next: u64,
    revoked: bool,
    log: Vec<String>,
}

impl Cloud {
    fn put_file(&mut self, path: &str, body: &[u8]) {
        self.next += 1;
        let id = self.files.get(path).map(|f| f.2).unwrap_or(1000 + self.next);
        self.files.insert(path.to_string(), (body.to_vec(), self.next, id));
    }
    fn etag(&self, path: &str) -> String {
        self.files.get(path).map(|f| format!("\"e{}\"", f.1)).unwrap_or_default()
    }
}

type Shared = Arc<Mutex<Cloud>>;

struct Req {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(s: &mut BufReader<TcpStream>) -> Option<Req> {
    let mut line = String::new();
    s.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        s.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if let Some(n) = headers.get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.resize(n, 0);
        s.read_exact(&mut body).ok()?;
    } else if headers.get("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        loop {
            let mut size = String::new();
            s.read_line(&mut size).ok()?;
            let n = usize::from_str_radix(size.trim(), 16).ok()?;
            let mut chunk = vec![0; n + 2];
            s.read_exact(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    }
    let path = target.split('?').next().unwrap_or("").to_string();
    Some(Req { method, path: decode(&path), headers, body })
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 3 <= b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(s: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &[u8]) {
    let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
}

fn entry_xml(cloud: &Cloud, path: &str) -> String {
    let href = format!("/remote.php/dav/files/alice{}", path.replace(' ', "%20"));
    if cloud.folders.contains(path) {
        format!(
            "<d:response><d:href>{href}/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getetag>\"dir\"</d:getetag><oc:permissions>RGDNVCK</oc:permissions></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        )
    } else {
        let (body, _, id) = &cloud.files[path];
        format!(
            "<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>{}</d:getcontentlength><d:getlastmodified>Wed, 14 Oct 2026 18:02:00 GMT</d:getlastmodified><d:getetag>{}</d:getetag><oc:fileid>{id}</oc:fileid><oc:permissions>RGDNVW</oc:permissions></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
            body.len(),
            cloud.etag(path)
        )
    }
}

fn handle(cloud: &Shared, mut stream: TcpStream) {
    let Ok(clone) = stream.try_clone() else { return };
    let mut reader = BufReader::new(clone);
    let Some(req) = read_request(&mut reader) else { return };
    let mut c = cloud.lock().unwrap();
    c.log.push(format!("{} {} if-match={:?} if={:?}", req.method, req.path, req.headers.get("if-match"), req.headers.get("if")));
    if c.offline {
        return; // Drop the connection without answering.
    }
    let base = c.base.clone();
    let auth_ok = req.headers.get("authorization").map(String::as_str) == Some(&format!("Basic {}", craft_nextcloud::dav::base64(b"alice:pw-123")));
    let json_ok = |s: &mut TcpStream, v: Json| respond(s, 200, &[("Content-Type", "application/json".into())], v.to_string().as_bytes());
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/index.php/login/v2") => {
            json_ok(&mut stream, json!({"poll": {"token": "tok-1", "endpoint": format!("{base}/login/v2/poll")}, "login": format!("{base}/login/v2/flow/abc")}))
        }
        ("POST", "/login/v2/poll") => {
            assert_eq!(String::from_utf8_lossy(&req.body), "token=tok-1");
            if c.poll_pending > 0 {
                c.poll_pending -= 1;
                respond(&mut stream, 404, &[], b"[]");
            } else {
                json_ok(&mut stream, json!({"server": base, "loginName": "alice", "appPassword": "pw-123"}));
            }
        }
        (_, p) if p.starts_with("/ocs/") && !auth_ok => respond(&mut stream, 401, &[], b""),
        ("GET", "/ocs/v2.php/cloud/user") => json_ok(&mut stream, json!({"ocs": {"data": {"id": "alice"}}})),
        ("POST", "/ocs/v2.php/apps/files_sharing/api/v1/shares") => {
            let body = String::from_utf8_lossy(&req.body).to_string();
            assert!(body.contains("shareType=3"), "{body}");
            json_ok(&mut stream, json!({"ocs": {"data": {"url": format!("{base}/s/AbC123")}}}))
        }
        ("DELETE", "/ocs/v2.php/core/apppassword") => {
            c.revoked = true;
            json_ok(&mut stream, json!({"ocs": {"data": []}}))
        }
        (m, p) if p.starts_with("/remote.php/dav/files/alice") => {
            if !auth_ok {
                return respond(&mut stream, 401, &[], b"");
            }
            let path = p.trim_start_matches("/remote.php/dav/files/alice").trim_end_matches('/').to_string();
            let path = if path.is_empty() { "/".to_string() } else { path };
            let header = |k: &str| req.headers.get(k).cloned().unwrap_or_default();
            match m {
                "PROPFIND" => {
                    let mut out = String::from("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\">");
                    if c.folders.contains(&path) {
                        out.push_str(&entry_xml(&c, &path));
                        if header("depth") == "1" {
                            let prefix = if path == "/" { "/".to_string() } else { format!("{path}/") };
                            let children: Vec<String> = c
                                .folders
                                .iter()
                                .chain(c.files.keys())
                                .filter(|k| k.starts_with(&prefix) && k.len() > prefix.len() && !k[prefix.len()..].contains('/'))
                                .cloned()
                                .collect();
                            for ch in children {
                                out.push_str(&entry_xml(&c, &ch));
                            }
                        }
                    } else if c.files.contains_key(&path) {
                        out.push_str(&entry_xml(&c, &path));
                    } else {
                        return respond(&mut stream, 404, &[], b"");
                    }
                    out.push_str("</d:multistatus>");
                    respond(&mut stream, 207, &[("Content-Type", "application/xml".into())], out.as_bytes());
                }
                "GET" => match c.files.get(&path) {
                    Some((body, _, _)) => {
                        let body = body.clone();
                        respond(&mut stream, 200, &[("ETag", c.etag(&path))], &body)
                    }
                    None => respond(&mut stream, 404, &[], b""),
                },
                "PUT" => {
                    let exists = c.files.contains_key(&path);
                    let if_match = header("if-match");
                    if !if_match.is_empty() && if_match != c.etag(&path) {
                        return respond(&mut stream, 412, &[], b"");
                    }
                    if header("if-none-match") == "*" && exists {
                        return respond(&mut stream, 412, &[], b"");
                    }
                    let iff = header("if");
                    if let Some(tok) = c.locks.get(&path) {
                        if !iff.contains(tok.as_str()) {
                            return respond(&mut stream, 423, &[], b"");
                        }
                    } else if !iff.is_empty() {
                        return respond(&mut stream, 412, &[], b"");
                    }
                    c.put_file(&path, &req.body);
                    let (etag, id) = (c.etag(&path), c.files[&path].2);
                    respond(
                        &mut stream,
                        if exists { 204 } else { 201 },
                        &[("ETag", etag.clone()), ("OC-ETag", etag), ("OC-FileId", format!("{id:08}ocabc"))],
                        b"",
                    )
                }
                "LOCK" => {
                    if !c.lock_supported {
                        return respond(&mut stream, 405, &[], b"");
                    }
                    let iff = header("if");
                    if !iff.is_empty() {
                        match c.locks.get(&path) {
                            Some(t) if iff.contains(t.as_str()) => {
                                let t = t.clone();
                                respond(&mut stream, 200, &[("Lock-Token", format!("<{t}>"))], b"")
                            }
                            _ => respond(&mut stream, 412, &[], b""),
                        }
                    } else if c.locks.contains_key(&path) {
                        respond(&mut stream, 423, &[], b"")
                    } else {
                        c.next += 1;
                        let t = format!("opaquelocktoken:t{}", c.next);
                        c.locks.insert(path.clone(), t.clone());
                        respond(&mut stream, 200, &[("Lock-Token", format!("<{t}>"))], b"<d:prop xmlns:d=\"DAV:\"/>")
                    }
                }
                "UNLOCK" => {
                    let tok = header("lock-token");
                    match c.locks.get(&path) {
                        Some(t) if tok == format!("<{t}>") => {
                            c.locks.remove(&path);
                            respond(&mut stream, 204, &[], b"")
                        }
                        _ => respond(&mut stream, 409, &[], b""),
                    }
                }
                _ => respond(&mut stream, 405, &[], b""),
            }
        }
        _ => respond(&mut stream, 404, &[], b""),
    }
}

fn start_cloud() -> Shared {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut c = Cloud { base, lock_supported: true, ..Cloud::default() };
    c.folders.insert("/".into());
    c.folders.insert("/Documents".into());
    c.folders.insert("/Documents/Reports".into());
    c.put_file("/Documents/Budget.xlsx", b"budget v1");
    c.put_file("/Documents/Notes.docx", b"notes");
    c.put_file("/Documents/Deck.pptx", b"deck");
    c.put_file("/Readme.md", b"readme");
    let shared = Arc::new(Mutex::new(c));
    let s2 = shared.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let s3 = s2.clone();
            std::thread::spawn(move || handle(&s3, stream));
        }
    });
    shared
}

// ----- The app -----

struct App {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    data: PathBuf,
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("craft-nextcloud-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

impl App {
    fn start(data: &Path, host: &str, language: &str) -> App {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_craft-nextcloud"));
        cmd.env("CRAFT_NEXTCLOUD_DATA", data)
            .env("CRAFT_NEXTCLOUD_BROWSER", "none")
            .env("CRAFT_NEXTCLOUD_HOSTNAME", "testbox")
            .env("CRAFT_HOST", host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            cmd.env_remove(k);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut app = App { child, stdin, stdout, next: 0, data: data.to_path_buf() };
        let r = app.request(
            "initialize",
            json!({"protocolVersion": 1, "host": {"id": host, "name": host, "version": "1.0", "language": language, "features": ["dialogs", "open", "events", "document"]}, "plugin": {"id": "org.craft.nextcloud", "dir": "."}}),
        );
        assert_eq!(r["result"]["protocolVersion"], 1);
        app
    }

    fn request(&mut self, method: &str, params: Json) -> Json {
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let v: Json = serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON: {line}"));
        assert_eq!(v["id"], self.next);
        v
    }

    fn run(&mut self, command: &str, params: Json, doc: Option<&Path>, dirty: bool) -> Json {
        let document = match doc {
            Some(p) => json!({"path": p.to_string_lossy(), "name": p.file_name().unwrap().to_string_lossy(), "dirty": dirty}),
            None => Json::Null,
        };
        let r = self.request("commands/run", json!({"command": command, "params": params, "context": {"workbook": "Book1", "document": document}}));
        assert!(r.get("error").is_none(), "{command} failed: {r}");
        r["result"].clone()
    }

    fn event(&mut self, name: &str, path: &Path) -> Json {
        let fmt = path.extension().unwrap().to_string_lossy().to_lowercase();
        let r = self
            .request(&format!("events/{name}"), json!({"path": path.to_string_lossy(), "name": path.file_name().unwrap().to_string_lossy(), "format": fmt}));
        assert!(r.get("error").is_none(), "{name} failed: {r}");
        r["result"].clone()
    }

    fn state(&self) -> Json {
        serde_json::from_slice(&std::fs::read(self.data.join("state.json")).unwrap()).unwrap()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "method": "shutdown"}));
        let _ = self.child.wait();
    }
}

/// Connects through the dialog chain: connect form → server → browser message → finishLogin.
fn connect(app: &mut App, cloud: &Shared) {
    let form = app.run("connect", json!({}), None, false);
    assert_eq!(form["dialog"]["kind"], "form");
    assert_eq!(form["dialog"]["command"], "connect");
    let base = cloud.lock().unwrap().base.clone();
    cloud.lock().unwrap().poll_pending = 2;
    let msg = app.run("connect", json!({"fields": {"server": format!("{base}/")}}), None, false);
    let d = &msg["dialog"];
    assert_eq!(d["kind"], "message");
    assert_eq!(d["command"], "finishLogin");
    assert!(msg["result"]["loginUrl"].as_str().unwrap().contains("/login/v2/flow/"));
    // The user clicks Continue too early: still waiting (two polls, one second apart).
    let again = app.run("finishLogin", d["params"].clone(), None, false);
    assert_eq!(again["dialog"]["command"], "finishLogin");
    assert!(again["dialog"]["prompt"].as_str().unwrap().contains("Still waiting"), "{again}");
    let done = app.run("finishLogin", d["params"].clone(), None, false);
    assert!(done["message"].as_str().unwrap().contains("as alice"), "{done}");
}

fn first_link(app: &App) -> (String, Json) {
    let st = app.state();
    let (k, v) = st["links"].as_object().unwrap().iter().next().unwrap();
    (k.clone(), v.clone())
}

#[test]
fn connect_browse_open_save_conflict_offline_and_close() {
    let cloud = start_cloud();
    let data = temp_dir("main");
    let mut app = App::start(&data, "gridcraft", "en");

    // Nothing connected: Open asks to connect.
    let r = app.run("open", json!({}), None, false);
    assert_eq!(r["dialog"]["command"], "connect");

    connect(&mut app, &cloud);
    let accounts: Json = serde_json::from_slice(&std::fs::read(data.join("accounts.json")).unwrap()).unwrap();
    assert_eq!(accounts["accounts"][0]["userId"], "alice");
    assert_eq!(accounts["accounts"][0]["appPassword"], "pw-123");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(data.join("accounts.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // Browse: folders first, then only the files GridCraft opens.
    let root = app.run("open", json!({}), None, false);
    let items = root["dialog"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{root}"); // Documents (Readme.md is WordCraft's)
    assert_eq!(items[0]["id"], "/Documents/");
    let docs = app.run("browse", json!({"account": root["dialog"]["params"]["account"], "path": "/", "id": "/Documents/"}), None, false);
    let labels: Vec<&str> = docs["dialog"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["Reports", "Budget.xlsx"]);
    let buttons = docs["dialog"]["buttons"].as_array().unwrap();
    assert_eq!(buttons[0]["label"], "Up");
    // Up with a selection still goes up.
    let up = app.run("browse", json!({"account": buttons[0]["params"]["account"], "path": "/", "nav": true, "id": "/Documents/Budget.xlsx"}), None, false);
    assert_eq!(up["dialog"]["items"][0]["id"], "/Documents/");

    // Open the file: downloaded, linked, backed up, locked.
    let opened = app.run("browse", json!({"account": docs["dialog"]["params"]["account"], "path": "/Documents", "id": "/Documents/Budget.xlsx"}), None, false);
    let local = PathBuf::from(opened["open"]["path"].as_str().unwrap());
    assert_eq!(opened["open"]["label"], "Nextcloud · Documents/Budget.xlsx");
    assert_eq!(opened["open"]["readOnly"], false);
    assert!(local.starts_with(data.canonicalize().unwrap().join("files")));
    assert_eq!(std::fs::read(&local).unwrap(), b"budget v1");
    let (_, link) = first_link(&app);
    assert_eq!(link["remote"], "/Documents/Budget.xlsx");
    assert_eq!(link["lock"], "locked");
    assert!(cloud.lock().unwrap().locks.contains_key("/Documents/Budget.xlsx"));

    // The app opens it: nothing changed, no message.
    assert_eq!(app.event("documentOpened", &local), json!({}));

    // Saved: uploaded with If-Match and the lock token.
    std::fs::write(&local, b"budget v2").unwrap();
    let r = app.event("documentSaved", &local);
    assert_eq!(r["message"], "Saved to Nextcloud");
    {
        let c = cloud.lock().unwrap();
        assert_eq!(c.files["/Documents/Budget.xlsx"].0, b"budget v2");
        let put = c.log.iter().rev().find(|l| l.starts_with("PUT")).unwrap();
        assert!(put.contains("if-match=Some(\"\\\"e") && put.contains("opaquelocktoken"), "{put}");
    }

    // Status says linked and locked.
    let st = app.run("status", json!({}), Some(&local), false);
    let text = st["dialog"]["prompt"].as_str().unwrap();
    assert!(text.contains("Documents/Budget.xlsx") && text.contains("locked by you"), "{text}");

    // Offline: the upload is queued, and goes up with Sync Now.
    cloud.lock().unwrap().offline = true;
    std::fs::write(&local, b"budget v3").unwrap();
    let r = app.event("documentSaved", &local);
    assert!(r["message"].as_str().unwrap().starts_with("Saved locally; Nextcloud upload pending"), "{r}");
    assert_eq!(first_link(&app).1["pending"], true);
    // Reopening from Nextcloud keeps the local copy while it's pending.
    let reopen = app.run("browse", json!({"path": "/Documents", "id": "/Documents/Budget.xlsx"}), None, false);
    assert_eq!(std::fs::read(&local).unwrap(), b"budget v3");
    assert!(reopen["message"].as_str().unwrap().contains("pending"));
    cloud.lock().unwrap().offline = false;
    let r = app.run("syncNow", json!({}), Some(&local), false);
    assert_eq!(r["result"]["uploaded"], 1, "{r}");
    assert_eq!(cloud.lock().unwrap().files["/Documents/Budget.xlsx"].0, b"budget v3");
    assert_eq!(first_link(&app).1["pending"], false);

    // Someone else changes it on the server (after taking our lock away): conflict.
    {
        let mut c = cloud.lock().unwrap();
        c.locks.clear();
        c.put_file("/Documents/Budget.xlsx", b"their version");
    }
    // Reopened: the app is told the server has a newer version.
    let r = app.event("documentOpened", &local);
    assert!(r["message"].as_str().unwrap().contains("changed on Nextcloud"), "{r}");
    std::fs::write(&local, b"my version").unwrap();
    let r = app.event("documentSaved", &local);
    let msg = r["message"].as_str().unwrap();
    assert!(msg.contains("Budget (conflict testbox "), "{msg}");
    {
        let c = cloud.lock().unwrap();
        assert_eq!(c.files["/Documents/Budget.xlsx"].0, b"their version");
        let copy = c.files.keys().find(|k| k.contains("(conflict testbox")).unwrap().clone();
        assert!(copy.starts_with("/Documents/Budget (conflict testbox 20") && copy.ends_with(").xlsx"), "{copy}");
        assert_eq!(c.files[&copy].0, b"my version");
    }
    let (key, link) = first_link(&app);
    assert_eq!(link["conflictOf"], "/Documents/Budget.xlsx");
    // The server's version is in the backups.
    let backups = app.run("backups", json!({}), Some(Path::new(&key)), false);
    let items = backups["dialog"]["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["detail"].as_str().unwrap().contains("Server's version")), "{backups}");
    assert!(items.iter().any(|i| i["detail"].as_str().unwrap().contains("Downloaded")));
    // Next save goes to the conflict copy.
    std::fs::write(&local, b"my version 2").unwrap();
    assert_eq!(app.event("documentSaved", &local)["message"], "Saved to Nextcloud");

    // Restore a backup: a copy in restored/, opened.
    let first = items.last().unwrap()["id"].clone();
    let mut p = backups["dialog"]["params"].clone();
    p["id"] = first;
    let restored = app.run("restoreBackup", p, None, false);
    let rp = PathBuf::from(restored["open"]["path"].as_str().unwrap());
    assert!(rp.starts_with(data.join("restored")));
    assert!(rp.file_name().unwrap().to_string_lossy().starts_with("Budget (backup "));
    assert_eq!(std::fs::read(&rp).unwrap(), b"budget v1");

    // Share and collaborate.
    let share = app.run("share", json!({}), Some(&local), false);
    assert!(share["result"]["url"].as_str().unwrap().ends_with("/s/AbC123"));
    assert_eq!(share["dialog"]["fields"][0]["value"], share["result"]["url"]);
    let web = app.run("openInBrowser", json!({}), Some(&local), false);
    assert!(web["result"]["url"].as_str().unwrap().contains("/index.php/f/"), "{web}");

    // Closing gives back the lock.
    let st = app.state();
    let token = st["links"][&key]["lockToken"].as_str().unwrap().to_string();
    if token.is_empty() {
        // The conflict copy wasn't locked yet: the app opening it locks it.
        app.event("documentOpened", &local);
    }
    assert!(!app.state()["links"][&key]["lockToken"].as_str().unwrap().is_empty());
    app.event("documentClosed", &local);
    assert!(cloud.lock().unwrap().locks.is_empty());
    assert_eq!(app.state()["links"][&key]["lockToken"], "");
    assert!(cloud.lock().unwrap().log.iter().any(|l| l.starts_with("UNLOCK")));

    // Disconnect: the app password is revoked and the account forgotten.
    let list = app.run("disconnect", json!({}), None, false);
    let id = list["dialog"]["items"][0]["id"].clone();
    let r = app.run("disconnect", json!({"id": id}), None, false);
    assert!(r["message"].as_str().unwrap().starts_with("Disconnected"));
    assert!(cloud.lock().unwrap().revoked);
    let accounts: Json = serde_json::from_slice(&std::fs::read(data.join("accounts.json")).unwrap()).unwrap();
    assert!(accounts["accounts"].as_array().unwrap().is_empty());
    drop(app);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn save_to_nextcloud_links_the_users_own_file_and_retention_applies() {
    let cloud = start_cloud();
    cloud.lock().unwrap().lock_supported = false;
    let data = temp_dir("saveto");
    let mine = temp_dir("saveto-mine");
    let mut app = App::start(&data, "wordcraft", "en");
    connect(&mut app, &cloud);

    // A new document: save it first.
    let r = app.run("saveTo", json!({}), None, false);
    assert!(r["dialog"]["prompt"].as_str().unwrap().contains("Save the document first"));
    let doc = mine.join("Letter.docx");
    std::fs::write(&doc, b"dear").unwrap();
    let r = app.run("saveTo", json!({}), Some(&doc), true);
    assert!(r["dialog"]["prompt"].as_str().unwrap().contains("Save the document first"));

    // A folder list, then Save in This Folder (with a folder selected: it saves in the shown one).
    let r = app.run("saveTo", json!({}), Some(&doc), false);
    let d = &r["dialog"];
    assert_eq!(d["items"].as_array().unwrap().len(), 1, "{r}"); // only folders
    let into =
        app.run("saveTo", json!({"account": d["params"]["account"], "path": "/", "document": d["params"]["document"], "id": "/Documents/"}), Some(&doc), false);
    let here = into["dialog"]["buttons"][0].clone();
    assert_eq!(here["label"], "Save in This Folder");
    let mut p = here["params"].clone();
    p["id"] = json!("/Documents/Reports/");
    let saved = app.run("saveTo", p.clone(), Some(&doc), false);
    assert_eq!(saved["result"]["remote"], "/Documents/Letter.docx", "{saved}");
    assert_eq!(cloud.lock().unwrap().files["/Documents/Letter.docx"].0, b"dear");
    assert!(doc.is_file(), "the user's file stays where it is");
    // Saving again where it exists asks before replacing.
    let again = app.run("saveTo", here["params"].clone(), Some(&doc), false);
    assert_eq!(again["dialog"]["okLabel"], "Replace");
    app.run("saveTo", again["dialog"]["params"].clone(), Some(&doc), false);

    // Locking isn't available on this server: noted, not an error.
    let st = app.run("status", json!({}), Some(&doc), false);
    assert!(st["dialog"]["prompt"].as_str().unwrap().contains("not available on this server (405)"), "{st}");

    // Retention: two versions, no dailies.
    let r = app.run("settings", json!({}), None, false);
    assert_eq!(r["dialog"]["fields"].as_array().unwrap().len(), 5);
    let bad = app.run("saveSettings", json!({"fields": {"keepVersions": 0}}), None, false);
    assert!(bad["dialog"]["prompt"].as_str().unwrap().contains("whole number"));
    let ok = app.run(
        "saveSettings",
        json!({"fields": {"backupOnSave": true, "keepVersions": 2, "keepDailyDays": 0, "lockWhileEditing": true, "uploadOnSave": true}}),
        None,
        false,
    );
    assert_eq!(ok["message"], "Settings saved");
    for i in 0..4 {
        std::fs::write(&doc, format!("dear {i}")).unwrap();
        assert_eq!(app.event("documentSaved", &doc)["message"], "Saved to Nextcloud");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(cloud.lock().unwrap().files["/Documents/Letter.docx"].0, b"dear 3");
    let b = app.run("backups", json!({}), Some(&doc), false);
    assert_eq!(b["dialog"]["items"].as_array().unwrap().len(), 2, "{b}");

    // A document that isn't linked is still backed up on save.
    let other = mine.join("Other.docx");
    std::fs::write(&other, b"x").unwrap();
    assert_eq!(app.event("documentSaved", &other), json!({}));
    let b = app.run("backups", json!({}), Some(&other), false);
    assert_eq!(b["dialog"]["items"].as_array().unwrap().len(), 1);
    drop(app);
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&mine);
}

#[test]
fn italian_and_old_hosts() {
    let data = temp_dir("lang");
    let mut app = App::start(&data, "deckcraft", "it-IT");
    let r = app.run("connect", json!({}), None, false);
    assert_eq!(r["dialog"]["title"], "Connetti a Nextcloud");
    let r = app.run("saveTo", json!({}), None, false);
    assert_eq!(r["dialog"]["prompt"], "Salva prima il documento.");
    // An app without dialogs gets an error, not a dialog it can't show.
    let r = app.request("initialize", json!({"protocolVersion": 1, "host": {"id": "deckcraft", "features": []}}));
    assert_eq!(r["result"]["protocolVersion"], 1);
    let r = app.request("commands/run", json!({"command": "open", "params": {}, "context": {}}));
    assert!(r["error"]["message"].as_str().unwrap().contains("1.2"));
    // Unknown methods and broken input never stop it.
    writeln!(app.stdin, "not json").unwrap();
    let r = app.request("nope", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    drop(app);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn manifest_prints() {
    let out = Command::new(env!("CARGO_BIN_EXE_craft-nextcloud")).arg("--manifest").output().unwrap();
    let m: Json = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(m["id"], "org.craft.nextcloud");
    assert_eq!(m["apps"], json!(["gridcraft", "wordcraft", "deckcraft"]));
    assert_eq!(m["events"], json!(["documentOpened", "documentSaved", "documentClosed"]));
    let on_disk: Json = serde_json::from_slice(&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.json")).unwrap()).unwrap();
    assert_eq!(on_disk, m, "plugin.json is out of date: run craft-nextcloud --manifest > plugin.json");
}
