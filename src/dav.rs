//! Nextcloud over HTTP: WebDAV (`PROPFIND`, `GET`, `PUT`, `LOCK`, `UNLOCK`), Login Flow v2 and
//! the OCS API (user id, share links, revoking an app password).
//!
//! Every request has a connect timeout and a total timeout, responses are read up to a limit,
//! redirects aren't followed (a WebDAV write must never land somewhere else), and paths are
//! percent-encoded per segment.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use quick_xml::events::Event;
use serde_json::Value as Json;
use ureq::Agent;
use ureq::http::{self, Method, Response};

use crate::paths;
use crate::store::Account;

/// Largest PROPFIND or JSON answer read (32 MiB).
const MAX_XML: u64 = 32 << 20;
/// Largest file downloaded (2 GiB).
pub const MAX_DOWNLOAD: u64 = 2 << 30;
/// Largest file uploaded (it's read whole, so the server gets a length: 1 GiB).
pub const MAX_UPLOAD: u64 = 1 << 30;
/// Items read from one folder at most.
pub const MAX_ITEMS: usize = 10_000;

/// How long requests may take.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub connect: Duration,
    pub total: Duration,
}

/// Commands (the app waits up to 60 s).
pub const COMMAND: Budget = Budget { connect: Duration::from_secs(10), total: Duration::from_secs(50) };
/// `documentSaved`/`documentOpened` (30 s).
pub const EVENT: Budget = Budget { connect: Duration::from_secs(10), total: Duration::from_secs(25) };
/// `documentClosed` (5 s).
pub const CLOSE: Budget = Budget { connect: Duration::from_secs(2), total: Duration::from_secs(4) };

/// Why a request failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Fail {
    /// The server couldn't be reached (or the connection broke): worth retrying later.
    Net(String),
    /// The server answered with this status.
    Status(u16, String),
    /// The answer made no sense.
    Bad(String),
}

impl Fail {
    pub fn text(&self) -> String {
        match self {
            Fail::Net(m) => format!("can't reach the server ({m})"),
            Fail::Status(401, _) => "the server refused the account's password (401): connect again".into(),
            Fail::Status(403, _) => "not allowed (403)".into(),
            Fail::Status(404, _) => "not found on the server (404)".into(),
            Fail::Status(507, _) => "the account is out of space (507)".into(),
            Fail::Status(c, m) if (300..400).contains(c) => format!("the server redirected ({c}) to {m}: use that address"),
            Fail::Status(c, m) if m.is_empty() => format!("the server answered {c}"),
            Fail::Status(c, m) => format!("the server answered {c}: {m}"),
            Fail::Bad(m) => format!("unexpected answer from the server ({m})"),
        }
    }

    /// Worth keeping the upload queued and retrying.
    pub fn retryable(&self) -> bool {
        match self {
            Fail::Net(_) => true,
            Fail::Status(c, _) => *c >= 500 || *c == 408 || *c == 429 || *c == 423,
            Fail::Bad(_) => false,
        }
    }
}

fn agent(b: Budget) -> Agent {
    Agent::config_builder()
        .timeout_connect(Some(b.connect))
        .timeout_global(Some(b.total))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .allow_non_standard_methods(true)
        .user_agent(user_agent())
        .build()
        .new_agent()
}

fn user_agent() -> String {
    let app = match std::env::var("CRAFT_HOST").unwrap_or_default().as_str() {
        "wordcraft" => "WordCraft",
        "deckcraft" => "DeckCraft",
        "gridcraft" => "GridCraft",
        _ => "craft apps",
    };
    format!("craft-nextcloud/{} ({app})", env!("CARGO_PKG_VERSION"))
}

/// Base64 (standard alphabet, padded), for Basic authentication.
pub fn base64(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let ch = |i: u32| char::from(A.get(usize::try_from(i & 63).unwrap_or(0)).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ch(n >> 18));
        out.push(ch(n >> 12));
        out.push(if chunk.len() > 1 { ch(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { ch(n) } else { '=' });
    }
    out
}

/// `cloud.example.com/` → `https://cloud.example.com`; refuses what isn't an http(s) address.
pub fn normalize_server(input: &str) -> Result<String, String> {
    let mut s = input.trim().to_string();
    if s.is_empty() {
        return Err("type the server's address".into());
    }
    if s.len() > 2048 || s.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("that address isn't valid".into());
    }
    if !s.contains("://") {
        s = format!("https://{s}");
    }
    let lower = s.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("the address must start with https://".into());
    }
    if let Some(q) = s.find(['?', '#']) {
        s.truncate(q);
    }
    while s.ends_with('/') {
        s.pop();
    }
    for tail in ["/index.php", "/login", "/apps/files"] {
        if s.to_ascii_lowercase().ends_with(tail) {
            s.truncate(s.len() - tail.len());
        }
    }
    while s.ends_with('/') {
        s.pop();
    }
    let host = s.split("://").nth(1).unwrap_or("");
    if host.is_empty() || host.starts_with('/') || host.contains('@') {
        return Err("the address has no server name".into());
    }
    s.parse::<http::Uri>().map_err(|_| "that address isn't valid".to_string())?;
    Ok(s)
}

/// One entry of a folder listing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    /// Normalized remote path (`/Documents/Budget.xlsx`).
    pub path: String,
    pub name: String,
    pub size: Option<u64>,
    /// Seconds since the epoch.
    pub modified: Option<i64>,
    pub etag: String,
    pub folder: bool,
    pub fileid: String,
    pub permissions: String,
}

impl Item {
    /// Nextcloud's permissions: `W` (or `NV`/`CK` for folders) means writable. Unknown means yes.
    pub fn writable(&self) -> bool {
        self.permissions.is_empty() || self.permissions.contains('W') || self.permissions.contains('C') || self.permissions.contains('K')
    }
}

/// What a `LOCK` gave.
#[derive(Clone, Debug, PartialEq)]
pub enum LockOutcome {
    Locked(String),
    /// Someone else holds a lock (423).
    Other,
    /// The server doesn't lock (405, 501, 412…), with why.
    Unavailable(String),
}

/// An uploaded file's new etag and id (empty when the server didn't say).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Uploaded {
    pub etag: String,
    pub fileid: String,
}

/// Compares two etags ignoring quotes and `W/`.
pub fn same_etag(a: &str, b: &str) -> bool {
    let n = |s: &str| s.trim().trim_start_matches("W/").trim_matches('"').to_string();
    !a.is_empty() && n(a) == n(b)
}

fn quoted(etag: &str) -> String {
    let e = etag.trim().trim_start_matches("W/");
    if e.starts_with('"') { e.to_string() } else { format!("\"{e}\"") }
}

fn header(r: &Response<ureq::Body>, name: &str) -> String {
    r.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_string()
}

fn read_body(r: &mut Response<ureq::Body>, limit: u64) -> Result<Vec<u8>, Fail> {
    r.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| Fail::Net(e.to_string()))
}

fn net(e: ureq::Error) -> Fail {
    Fail::Net(e.to_string())
}

/// A short reason from an error answer (OCS's message, or the start of the text).
fn reason(r: &mut Response<ureq::Body>) -> String {
    let location = header(r, "location");
    if (300..400).contains(&r.status().as_u16()) {
        return location;
    }
    let body = read_body(r, 64 << 10).unwrap_or_default();
    let text = String::from_utf8_lossy(&body);
    if let Some(m) = xml_text(&text, "message").filter(|m| !m.is_empty()) {
        return m.chars().take(200).collect();
    }
    if let Ok(j) = serde_json::from_slice::<Json>(&body)
        && let Some(m) = j.pointer("/ocs/meta/message").and_then(Json::as_str)
    {
        return m.chars().take(200).collect();
    }
    String::new()
}

/// A client for one account.
pub struct Client {
    agent: Agent,
    server: String,
    user: String,
    auth: String,
}

impl Client {
    pub fn new(acc: &Account, budget: Budget) -> Client {
        let user = if acc.user_id.is_empty() { acc.login_name.clone() } else { acc.user_id.clone() };
        Client {
            agent: agent(budget),
            server: acc.server.trim_end_matches('/').to_string(),
            user,
            auth: format!("Basic {}", base64(format!("{}:{}", acc.login_name, acc.app_password).as_bytes())),
        }
    }

    /// `<server>/remote.php/dav/files/<user>/<path>`.
    pub fn dav_url(&self, remote: &str) -> String {
        format!("{}/remote.php/dav/files/{}{}", self.server, paths::encode_segment(&self.user), paths::encode_path(remote))
    }

    fn builder(&self, method: &str, url: &str) -> Result<http::request::Builder, Fail> {
        let m = Method::from_bytes(method.as_bytes()).map_err(|_| Fail::Bad("method".into()))?;
        Ok(http::Request::builder().method(m).uri(url).header("Authorization", &self.auth))
    }

    fn run<B: ureq::AsSendBody>(&self, req: Result<http::Request<B>, http::Error>) -> Result<Response<ureq::Body>, Fail> {
        let req = req.map_err(|e| Fail::Bad(e.to_string()))?;
        self.agent.run(req).map_err(net)
    }

    /// `PROPFIND` with depth 0 (the item itself) or 1 (a folder and what's in it).
    pub fn propfind(&self, remote: &str, depth: u8) -> Result<Vec<Item>, Fail> {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns"><d:prop><d:displayname/><d:getcontentlength/><d:getlastmodified/><d:getetag/><d:resourcetype/><oc:fileid/><oc:permissions/></d:prop></d:propfind>"#;
        let mut url = self.dav_url(remote);
        if depth > 0 && !url.ends_with('/') {
            url.push('/');
        }
        let req = self
            .builder("PROPFIND", &url)?
            .header("Depth", if depth == 0 { "0" } else { "1" })
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body);
        let mut r = self.run(req)?;
        let status = r.status().as_u16();
        if status != 207 {
            return Err(Fail::Status(status, reason(&mut r)));
        }
        let bytes = read_body(&mut r, MAX_XML)?;
        let text = String::from_utf8_lossy(&bytes);
        parse_multistatus(&text, &self.user).map_err(Fail::Bad)
    }

    /// The item at a path (`PROPFIND` depth 0).
    pub fn stat(&self, remote: &str) -> Result<Item, Fail> {
        self.propfind(remote, 0)?.into_iter().next().ok_or_else(|| Fail::Bad("empty listing".into()))
    }

    /// Downloads a file to `dest` (through a temporary file next to it). Returns its etag.
    pub fn download(&self, remote: &str, dest: &Path) -> Result<String, Fail> {
        let req = self.builder("GET", &self.dav_url(remote))?.body(());
        let mut r = self.run(req)?;
        let status = r.status().as_u16();
        if status != 200 {
            return Err(Fail::Status(status, reason(&mut r)));
        }
        let etag = {
            let oc = header(&r, "oc-etag");
            if oc.is_empty() { header(&r, "etag") } else { oc }
        };
        let dir = dest.parent().ok_or_else(|| Fail::Bad("no folder".into()))?;
        std::fs::create_dir_all(dir).map_err(|e| Fail::Bad(format!("can't create {}: {e}", dir.display())))?;
        let tmp = crate::store::temp_path(dest);
        let result = (|| -> Result<(), Fail> {
            let mut f = crate::store::new_file(&tmp, false).map_err(|e| Fail::Bad(e.to_string()))?;
            let mut reader = r.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
            let mut buf = vec![0u8; 64 << 10];
            loop {
                let n = reader.read(&mut buf).map_err(|e| Fail::Net(e.to_string()))?;
                if n == 0 {
                    break;
                }
                f.write_all(buf.get(..n).unwrap_or(&[])).map_err(|e| Fail::Bad(format!("can't write the file: {e}")))?;
            }
            f.sync_all().map_err(|e| Fail::Bad(e.to_string()))?;
            drop(f);
            std::fs::rename(&tmp, dest).map_err(|e| Fail::Bad(format!("can't write {}: {e}", dest.display())))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map(|_| etag)
    }

    /// Uploads `body` to `remote`. `if_match`: only if the server's version has this etag
    /// (412 otherwise); `create_only`: only if nothing is there; `lock`: our lock token.
    pub fn upload(&self, remote: &str, body: Vec<u8>, if_match: Option<&str>, create_only: bool, lock: Option<&str>) -> Result<Uploaded, Fail> {
        let mut b = self.builder("PUT", &self.dav_url(remote))?.header("Content-Type", "application/octet-stream");
        if let Some(e) = if_match.filter(|e| !e.is_empty()) {
            b = b.header("If-Match", quoted(e));
        }
        if create_only {
            b = b.header("If-None-Match", "*");
        }
        if let Some(t) = lock.filter(|t| !t.is_empty()) {
            b = b.header("If", format!("(<{t}>)"));
        }
        let mut r = self.run(b.body(body))?;
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Fail::Status(status, reason(&mut r)));
        }
        let oc = header(&r, "oc-etag");
        let etag = if oc.is_empty() { header(&r, "etag") } else { oc };
        Ok(Uploaded { etag, fileid: header(&r, "oc-fileid") })
    }

    /// `LOCK` (exclusive write, 30 minutes); with `refresh`, renews that lock instead.
    pub fn lock(&self, remote: &str, refresh: Option<&str>) -> Result<LockOutcome, Fail> {
        let url = self.dav_url(remote);
        let b = self.builder("LOCK", &url)?.header("Timeout", "Second-1800");
        let req = match refresh.filter(|t| !t.is_empty()) {
            Some(t) => b.header("If", format!("(<{t}>)")).body(String::new()),
            None => b.header("Content-Type", "application/xml; charset=utf-8").header("Depth", "0").body(
                r#"<?xml version="1.0" encoding="utf-8"?>
<d:lockinfo xmlns:d="DAV:"><d:lockscope><d:exclusive/></d:lockscope><d:locktype><d:write/></d:locktype><d:owner>craft-nextcloud</d:owner></d:lockinfo>"#
                    .to_string(),
            ),
        };
        let mut r = self.run(req)?;
        let status = r.status().as_u16();
        match status {
            200 | 201 => {
                let mut token = header(&r, "lock-token").trim_start_matches('<').trim_end_matches('>').to_string();
                if token.is_empty() {
                    let body = read_body(&mut r, 1 << 20).unwrap_or_default();
                    token = xml_text(&String::from_utf8_lossy(&body), "href").unwrap_or_default();
                }
                if token.is_empty() {
                    token = refresh.unwrap_or("").to_string();
                }
                if token.is_empty() || token.len() > 1024 || token.chars().any(|c| c.is_control() || c == '>' || c == '<') {
                    return Ok(LockOutcome::Unavailable("no lock token".into()));
                }
                Ok(LockOutcome::Locked(token))
            }
            423 => Ok(LockOutcome::Other),
            401 => Err(Fail::Status(401, String::new())),
            _ => Ok(LockOutcome::Unavailable(format!("{status}"))),
        }
    }

    /// `UNLOCK`.
    pub fn unlock(&self, remote: &str, token: &str) -> Result<(), Fail> {
        let req = self.builder("UNLOCK", &self.dav_url(remote))?.header("Lock-Token", format!("<{token}>")).body(());
        let mut r = self.run(req)?;
        let status = r.status().as_u16();
        if (200..300).contains(&status) || status == 409 || status == 404 {
            return Ok(());
        }
        Err(Fail::Status(status, reason(&mut r)))
    }

    fn ocs(&self, method: &str, path: &str, form: Option<String>) -> Result<Json, Fail> {
        let url = format!("{}/ocs/v2.php/{path}", self.server);
        let url = if url.contains('?') { format!("{url}&format=json") } else { format!("{url}?format=json") };
        let b = self.builder(method, &url)?.header("OCS-APIRequest", "true").header("Accept", "application/json");
        let mut r = match form {
            Some(f) => self.run(b.header("Content-Type", "application/x-www-form-urlencoded").body(f))?,
            None => self.run(b.body(()))?,
        };
        let status = r.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(Fail::Status(status, reason(&mut r)));
        }
        let bytes = read_body(&mut r, MAX_XML)?;
        if bytes.is_empty() {
            return Ok(Json::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| Fail::Bad("not JSON".into()))
    }

    /// The account's user id (WebDAV paths use it; it may differ from the login name).
    pub fn user_id(&self) -> Result<String, Fail> {
        let j = self.ocs("GET", "cloud/user", None)?;
        j.pointer("/ocs/data/id")
            .and_then(Json::as_str)
            .filter(|s| !s.is_empty() && s.len() < 512)
            .map(str::to_string)
            .ok_or_else(|| Fail::Bad("no user id".into()))
    }

    /// A public share link (share type 3) to a file.
    pub fn share_link(&self, remote: &str) -> Result<String, Fail> {
        let form = format!("path={}&shareType=3", paths::form_encode(remote));
        let j = self.ocs("POST", "apps/files_sharing/api/v1/shares", Some(form))?;
        let url = j.pointer("/ocs/data/url").and_then(Json::as_str).unwrap_or("");
        let lower = url.to_ascii_lowercase();
        if url.len() < 4096 && (lower.starts_with("https://") || lower.starts_with("http://")) {
            Ok(url.to_string())
        } else {
            Err(Fail::Bad("no link in the answer".into()))
        }
    }

    /// Revokes the app password this client signs in with.
    pub fn revoke(&self) -> Result<(), Fail> {
        self.ocs("DELETE", "core/apppassword", None).map(|_| ())
    }
}

/// Login Flow v2, step 1: `(login page, poll token, poll endpoint)`.
pub fn login_start(server: &str) -> Result<(String, String, String), Fail> {
    let a = agent(COMMAND);
    let req = http::Request::builder().method(Method::POST).uri(format!("{server}/index.php/login/v2")).body(()).map_err(|e| Fail::Bad(e.to_string()))?;
    let mut r = a.run(req).map_err(net)?;
    let status = r.status().as_u16();
    if status != 200 {
        return Err(Fail::Status(status, reason(&mut r)));
    }
    let j: Json = serde_json::from_slice(&read_body(&mut r, 1 << 20)?).map_err(|_| Fail::Bad("not a Nextcloud server".into()))?;
    let get = |p: &str| j.pointer(p).and_then(Json::as_str).filter(|s| !s.is_empty() && s.len() < 4096).map(str::to_string);
    let login = get("/login").ok_or_else(|| Fail::Bad("not a Nextcloud server".into()))?;
    let token = get("/poll/token").ok_or_else(|| Fail::Bad("no poll token".into()))?;
    let endpoint = get("/poll/endpoint").ok_or_else(|| Fail::Bad("no poll endpoint".into()))?;
    for u in [&login, &endpoint] {
        let l = u.to_ascii_lowercase();
        if !(l.starts_with("https://") || l.starts_with("http://")) {
            return Err(Fail::Bad("bad address in the answer".into()));
        }
    }
    Ok((login, token, endpoint))
}

/// Login Flow v2, step 2: `Some((server, login name, app password))` once the user signed in,
/// `None` while waiting.
pub fn login_poll(endpoint: &str, token: &str) -> Result<Option<(String, String, String)>, Fail> {
    let a = agent(COMMAND);
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("token={}", paths::form_encode(token)))
        .map_err(|e| Fail::Bad(e.to_string()))?;
    let mut r = a.run(req).map_err(net)?;
    let status = r.status().as_u16();
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(Fail::Status(status, reason(&mut r)));
    }
    let j: Json = serde_json::from_slice(&read_body(&mut r, 1 << 20)?).map_err(|_| Fail::Bad("not JSON".into()))?;
    let get = |k: &str| j.get(k).and_then(Json::as_str).filter(|s| !s.is_empty() && s.len() < 4096).map(str::to_string);
    match (get("server"), get("loginName"), get("appPassword")) {
        (Some(s), Some(l), Some(p)) => Ok(Some((s, l, p))),
        _ => Err(Fail::Bad("incomplete sign-in answer".into())),
    }
}

/// The text of the first element named `local` (any namespace).
fn xml_text(xml: &str, local: &str) -> Option<String> {
    let mut r = quick_xml::Reader::from_str(xml);
    let mut inside = false;
    let mut out = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == local.as_bytes() => inside = true,
            Ok(Event::End(e)) if inside && e.local_name().as_ref() == local.as_bytes() => return Some(out.trim().to_string()),
            Ok(Event::Text(t)) if inside => out.push_str(&t.decode().ok()?),
            Ok(Event::GeneralRef(g)) if inside => out.push_str(&entity(&g.decode().ok()?)),
            Ok(Event::CData(t)) if inside => out.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// `amp` → `&`, `#233` → `é`.
fn entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        n => {
            let code = if let Some(h) = n.strip_prefix("#x").or_else(|| n.strip_prefix("#X")) {
                u32::from_str_radix(h, 16).ok()
            } else {
                n.strip_prefix('#').and_then(|d| d.parse().ok())
            };
            code.and_then(char::from_u32).map(String::from).unwrap_or_default()
        }
    }
}

/// Parses a WebDAV multistatus answer into items (at most [`MAX_ITEMS`] + 1), keeping only
/// properties reported with status 200. Paths are made relative to the user's files
/// (`/remote.php/dav/files/<user>/`).
pub fn parse_multistatus(xml: &str, user: &str) -> Result<Vec<Item>, String> {
    let mut r = quick_xml::Reader::from_str(xml);
    let mut items = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    // Per response:
    let mut href = String::new();
    let mut item = Item::default();
    // Per propstat:
    let mut props = Item::default();
    let mut status_ok = true;
    let mut depth_guard = 0usize;
    loop {
        let ev = r.read_event().map_err(|e| format!("bad XML: {e}"))?;
        match ev {
            Event::Start(e) => {
                depth_guard += 1;
                if depth_guard > 64 {
                    return Err("XML nested too deep".into());
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                match name.as_str() {
                    "response" => {
                        href.clear();
                        item = Item::default();
                    }
                    "propstat" => {
                        props = Item::default();
                        status_ok = true;
                    }
                    "collection" if stack.iter().any(|s| s == "resourcetype") => props.folder = true,
                    _ => {}
                }
                stack.push(name);
                text.clear();
            }
            Event::Empty(e) if e.local_name().as_ref() == b"collection" && stack.iter().any(|s| s == "resourcetype") => props.folder = true,
            Event::Text(t) => text.push_str(&t.decode().map_err(|e| e.to_string())?),
            Event::GeneralRef(g) => text.push_str(&entity(&g.decode().map_err(|e| e.to_string())?)),
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::End(_) => {
                depth_guard = depth_guard.saturating_sub(1);
                let name = stack.pop().unwrap_or_default();
                let in_prop = stack.last().map(String::as_str) == Some("prop");
                let value = text.trim().to_string();
                match name.as_str() {
                    "href" if stack.last().map(String::as_str) == Some("response") => href = value,
                    "status" if stack.last().map(String::as_str) == Some("propstat") => {
                        status_ok = value.split_whitespace().nth(1).is_some_and(|c| c.starts_with('2'));
                    }
                    "displayname" if in_prop => props.name = value,
                    "getcontentlength" if in_prop => props.size = value.parse().ok(),
                    "getlastmodified" if in_prop => props.modified = crate::time::parse_http_date(&value),
                    "getetag" if in_prop => props.etag = value,
                    "fileid" if in_prop => props.fileid = value.chars().take(64).collect(),
                    "permissions" if in_prop => props.permissions = value.chars().take(32).collect(),
                    "propstat" if status_ok => {
                        if !props.name.is_empty() {
                            item.name = std::mem::take(&mut props.name);
                        }
                        item.size = item.size.or(props.size);
                        item.modified = item.modified.or(props.modified);
                        if !props.etag.is_empty() {
                            item.etag = std::mem::take(&mut props.etag);
                        }
                        if !props.fileid.is_empty() {
                            item.fileid = std::mem::take(&mut props.fileid);
                        }
                        if !props.permissions.is_empty() {
                            item.permissions = std::mem::take(&mut props.permissions);
                        }
                        item.folder |= props.folder;
                    }
                    "response" => {
                        if let Some(path) = relative_path(&href, user) {
                            let mut it = std::mem::take(&mut item);
                            it.path = path;
                            let base = paths::remote_name(&it.path).to_string();
                            if it.name.is_empty() || it.name.contains('/') || it.name.chars().any(char::is_control) {
                                it.name = base;
                            }
                            items.push(it);
                            if items.len() > MAX_ITEMS {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(items)
}

/// `/remote.php/dav/files/alice/Docs/My%20File.xlsx` (or a full URL) → `/Docs/My File.xlsx`.
/// `None` when it isn't under the user's files or isn't a valid path.
pub fn relative_path(href: &str, user: &str) -> Option<String> {
    let path = match href.find("://") {
        Some(i) => {
            let rest = href.get(i + 3..)?;
            rest.get(rest.find('/')?..)?
        }
        None => href,
    };
    let decoded = paths::decode_percent(path);
    let marker = format!("/remote.php/dav/files/{user}");
    let at = decoded.find(&marker)?;
    let rest = decoded.get(at + marker.len()..)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    paths::normalize_remote(rest).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns">
 <d:response><d:href>/remote.php/dav/files/alice/Documents/</d:href>
  <d:propstat><d:prop><d:getlastmodified>Wed, 14 Oct 2026 18:02:00 GMT</d:getlastmodified><d:resourcetype><d:collection/></d:resourcetype><d:getetag>&quot;f1&quot;</d:getetag><oc:fileid>10</oc:fileid><oc:permissions>RGDNVCK</oc:permissions></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getcontentlength/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/files/alice/Documents/My%20Budget%20%26%20Co.xlsx</d:href>
  <d:propstat><d:prop><d:displayname>My Budget &amp; Co.xlsx</d:displayname><d:getcontentlength>12345</d:getcontentlength><d:resourcetype/><d:getetag>"e2"</d:getetag><oc:fileid>11</oc:fileid><oc:permissions>RGDNVW</oc:permissions></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/files/alice/Documents/Reports/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
 <d:response><d:href>/remote.php/dav/files/bob/../../etc/passwd</d:href>
  <d:propstat><d:prop><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;

    #[test]
    fn parses_propfind() {
        let items = parse_multistatus(LISTING, "alice").unwrap();
        assert_eq!(items.len(), 3);
        let a = &items[0];
        assert_eq!(a.path, "/Documents");
        assert!(a.folder);
        assert_eq!(a.etag, "\"f1\"");
        assert_eq!(a.size, None);
        assert!(a.modified.is_some());
        let b = &items[1];
        assert_eq!(b.path, "/Documents/My Budget & Co.xlsx");
        assert_eq!(b.name, "My Budget & Co.xlsx");
        assert_eq!(b.size, Some(12345));
        assert!(!b.folder);
        assert_eq!(b.fileid, "11");
        assert!(b.writable());
        assert_eq!(items[2].name, "Reports");
        assert!(items[2].folder);
        assert!(parse_multistatus("<d:multistatus xmlns:d='DAV:'><d:response>", "alice").is_ok() || parse_multistatus("<<<", "a").is_err());
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative_path("https://c.example/nc/remote.php/dav/files/alice/A/b.xlsx", "alice").as_deref(), Some("/A/b.xlsx"));
        assert_eq!(relative_path("/remote.php/dav/files/alice/", "alice").as_deref(), Some("/"));
        assert_eq!(relative_path("/remote.php/dav/files/alice2/x", "alice"), None);
        assert_eq!(relative_path("/remote.php/dav/files/alice/a/../../x", "alice"), None);
    }

    #[test]
    fn servers_and_etags() {
        assert_eq!(normalize_server("cloud.example.com/").unwrap(), "https://cloud.example.com");
        assert_eq!(normalize_server("https://x.org/nc/index.php/").unwrap(), "https://x.org/nc");
        assert_eq!(normalize_server("http://127.0.0.1:8080").unwrap(), "http://127.0.0.1:8080");
        assert!(normalize_server("ftp://x").is_err());
        assert!(normalize_server("").is_err());
        assert!(normalize_server("https://").is_err());
        assert!(same_etag("\"a\"", "a"));
        assert!(same_etag("W/\"a\"", "\"a\""));
        assert!(!same_etag("", ""));
        assert_eq!(quoted("abc"), "\"abc\"");
        assert_eq!(base64(b"alice:secret"), "YWxpY2U6c2VjcmV0");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn entities() {
        assert_eq!(entity("#233"), "é");
        assert_eq!(entity("#x41"), "A");
        assert_eq!(entity("bogus"), "");
        assert_eq!(xml_text("<s:error xmlns:s='x'><s:message>No &amp; way</s:message></s:error>", "message").as_deref(), Some("No & way"));
    }
}
