//! The add-in: JSON-RPC 2.0 over standard input and output, one message per line (the craft
//! plugin protocol 1, with 1.2's dialogs, `open` and document events).

use std::io::{BufRead, Read, Write};

use serde_json::{Value as Json, json};

use crate::store::Store;
use crate::text::Lang;

/// Longest line read.
const MAX_LINE: u64 = 64 << 20;

/// The app running the add-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum App {
    #[default]
    GridCraft,
    WordCraft,
    DeckCraft,
}

impl App {
    /// From `host.id` / `CRAFT_HOST`; GridCraft when it's something else.
    pub fn from_id(id: &str) -> App {
        match id.to_ascii_lowercase().as_str() {
            "wordcraft" => App::WordCraft,
            "deckcraft" => App::DeckCraft,
            _ => App::GridCraft,
        }
    }

    /// The file types the app opens.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            App::GridCraft => &["xlsx", "xlsm", "xltx", "xltm", "csv", "tsv", "ods"],
            App::WordCraft => &["docx", "docm", "dotx", "dotm", "odt", "rtf", "txt", "md", "html"],
            App::DeckCraft => &["pptx", "pptm", "potx", "potm", "ppsx", "ppsm", "odp"],
        }
    }

    /// The icon of the app's documents in lists.
    pub fn file_icon(self) -> &'static str {
        match self {
            App::GridCraft => "workbook",
            App::WordCraft => "document",
            App::DeckCraft => "presentation",
        }
    }

    pub fn opens(self, name: &str) -> bool {
        self.extensions().contains(&crate::paths::extension(name).as_str())
    }
}

/// What the add-in knows about the app it's talking to.
#[derive(Debug, Default)]
pub struct Session {
    pub app: App,
    pub lang: Lang,
    /// `host.features` (`None`: the app didn't send any, so it's older than protocol 1.2).
    pub features: Option<Vec<String>>,
    store: Option<Store>,
}

impl Session {
    pub fn new() -> Session {
        Session { app: App::from_id(&std::env::var("CRAFT_HOST").unwrap_or_default()), ..Session::default() }
    }

    pub fn has(&self, feature: &str) -> bool {
        self.features.as_ref().is_some_and(|f| f.iter().any(|x| x == feature))
    }

    /// The data folder (opened once).
    pub fn store(&mut self) -> Result<Store, String> {
        if let Some(s) = &self.store {
            return Ok(s.clone());
        }
        let s = Store::open()?;
        self.store = Some(s.clone());
        Ok(s)
    }
}

/// A request's error: (code, message).
pub type Fail = (i64, String);

pub fn fail(message: impl Into<String>) -> Fail {
    (1, message.into())
}

/// Answers one request.
pub fn handle(s: &mut Session, method: &str, params: &Json) -> Result<Json, Fail> {
    match method {
        "initialize" => {
            let host = params.get("host");
            if let Some(id) = host.and_then(|h| h.get("id")).and_then(Json::as_str) {
                s.app = App::from_id(id);
            }
            if let Some(l) = host.and_then(|h| h.get("language")).and_then(Json::as_str) {
                s.lang = Lang::from_tag(l);
            }
            s.features = host
                .and_then(|h| h.get("features"))
                .and_then(Json::as_array)
                .map(|a| a.iter().filter_map(Json::as_str).take(64).map(|x| x.chars().take(64).collect()).collect());
            Ok(json!({"protocolVersion": 1}))
        }
        "commands/run" => {
            let command = params.get("command").and_then(Json::as_str).unwrap_or("");
            let p = params.get("params").cloned().filter(Json::is_object).unwrap_or_else(|| json!({}));
            let ctx = params.get("context").cloned().unwrap_or(Json::Null);
            crate::commands::run(s, command, &p, &ctx)
        }
        "events/documentOpened" => crate::events::opened(s, params),
        "events/documentSaved" => crate::events::saved(s, params),
        "events/documentClosed" => crate::events::closed(s, params),
        "functions/call" => Err((-32601, "craft-nextcloud has no functions".into())),
        _ => Err((-32601, format!("unknown method {method}"))),
    }
}

fn send<W: Write>(out: &mut W, msg: &Json) -> bool {
    writeln!(out, "{msg}").and_then(|_| out.flush()).is_ok()
}

/// Serves the app until its stream closes or it says `shutdown`.
pub fn serve<R: BufRead, W: Write>(mut input: R, mut output: W) {
    let mut session = Session::new();
    loop {
        let mut line = Vec::new();
        match (&mut input).take(MAX_LINE).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_slice::<Json>(&line) else {
            if !String::from_utf8_lossy(&line).trim().is_empty() {
                eprintln!("craft-nextcloud: ignored a line that isn't JSON");
            }
            continue;
        };
        let Some(method) = msg.get("method").and_then(Json::as_str).map(str::to_string) else { continue };
        let Some(id) = msg.get("id").cloned() else {
            if method == "shutdown" || method == "exit" {
                return;
            }
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(Json::Null);
        // A bug here must cost the request, never the add-in.
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(&mut session, &method, &params)))
            .unwrap_or_else(|_| Err(fail("craft-nextcloud hit an internal error")));
        let reply = match answer {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        if !send(&mut output, &reply) {
            return;
        }
    }
}
