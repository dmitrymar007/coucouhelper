// Chat through opencode and whichever providers the user has set up in it:
// OpenCode Zen, OpenRouter, any OpenAI-compatible endpoint…
//
// Like the Claude Code chat: one hidden `opencode serve`, started with the
// first message and kept alive, so only the first message pays for the start.
// It listens on 127.0.0.1 only, behind a random password. Each turn listens to
// its event stream, sends the message, and streams the answer's text to the
// island word by word until the session is idle again. It is stopped on demand,
// after a while idle, and with the app.
//
// It runs as harmless as the Claude Code chat: its own agent, defined through
// OPENCODE_CONFIG_CONTENT, with web search and no other tool — no file access,
// no edits, no commands, no MCP tools — no external plugins (`--pure`), no
// project config, nothing from ~/.claude. The user's own opencode config is
// read for providers and models, never written.
//
// opencode saves every session in its own database, so the history list and
// "Continue" work from there, with the CLI, whether the server runs or not.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::claude::{self, ChatContext, ChatReply};
use crate::claude_cli::{self, SessionInfo, StreamUpdate, TranscriptMessage};
use crate::platform;

/// The agent the chat runs as, defined in `config()`.
const AGENT: &str = "coucou";

/// Longest silence inside one answer before the turn is presumed stuck.
const SILENCE_LIMIT: Duration = Duration::from_secs(180);

/// The chat's agent and nothing else of the user's config changed. Tools: web
/// search only, for the same reason as the Claude Code chat — a prompt
/// injected through a web page or a dropped file must find nothing to read and
/// nowhere to send it. `"*": false` also switches off every MCP tool.
fn config() -> String {
    json!({
        "agent": {
            AGENT: {
                "mode": "primary",
                "description": "Coucou's island chat",
                "prompt": claude::SYSTEM_PROMPT,
                "tools": { "*": false, "websearch": true },
                "permission": { "*": "deny", "websearch": "allow" },
            }
        },
        "share": "disabled",
        "autoupdate": false,
    })
    .to_string()
}

/// Environment of every opencode the chat starts. OPENCODE_ENABLE_EXA gives
/// any provider the websearch tool, not only OpenCode Zen.
fn environment() -> Vec<(&'static str, String)> {
    vec![
        ("OPENCODE_CONFIG_CONTENT", config()),
        ("OPENCODE_ENABLE_EXA", "1".into()),
        ("OPENCODE_DISABLE_PROJECT_CONFIG", "1".into()),
        ("OPENCODE_DISABLE_CLAUDE_CODE", "1".into()),
        ("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1".into()),
        ("OPENCODE_DISABLE_AUTOUPDATE", "1".into()),
        ("OPENCODE_DISABLE_SHARE", "1".into()),
        (claude_cli::CHAT_ENV, "1".into()),
    ]
}

/// The `opencode` executable: on PATH, or where its installer puts it — an
/// app started from the desktop does not always get ~/.opencode/bin on PATH.
pub fn find_opencode() -> Option<PathBuf> {
    platform::find_on_path("opencode").or_else(|| {
        let home = platform::home_dir();
        [home.join(".opencode/bin/opencode"), home.join(".local/bin/opencode")]
            .into_iter()
            .find(|p| p.is_file())
    })
}

fn command(exe: &PathBuf) -> Command {
    let dir = claude_cli::work_dir();
    let mut cmd = Command::new(exe);
    // opencode takes its working directory from $PWD, not from the process's
    // own: without it every session would belong to wherever Coucou started.
    cmd.current_dir(&dir)
        .env("PWD", &dir)
        .envs(environment())
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    claude_cli::die_with_parent(&mut cmd);
    cmd
}

/// opencode's session ids: "ses_" and 26 letters or digits, nothing that
/// could pass for a flag or a path.
pub fn is_session_id(id: &str) -> bool {
    id.len() == 30 && id.starts_with("ses_") && id[4..].chars().all(|c| c.is_ascii_alphanumeric())
}

// ── One turn ──────────────────────────────────────────────────────────────────

/// What the island shows when the turn failed: opencode's own words, unless
/// they mean no provider is set up for the model.
pub fn friendly_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let no_provider = ["api key", "apikey", "unauthorized", "401", "provider not found", "model not found", "providermodelnotfound"]
        .iter()
        .any(|k| lower.contains(k));
    if no_provider {
        return format!(
            "opencode has no working provider for this model ({}). Run `opencode auth login` in a terminal, or pick another model in Settings → Chat.",
            raw.trim().chars().take(120).collect::<String>()
        );
    }
    let raw = raw.trim();
    if raw.is_empty() {
        "opencode stopped without an answer.".into()
    } else {
        raw.chars().take(300).collect()
    }
}

/// What the first message says besides the question. A dropped file travels
/// as a file part; its name and the window go in the text.
fn first_message(query: &str, context: Option<&ChatContext>) -> (String, Option<String>) {
    match context {
        Some(ChatContext::File { name, path }) => (format!("File: {name}\n\n{query}"), Some(path.clone())),
        Some(ChatContext::Window { app_name, title, url }) => {
            let mut text = format!("Context — App: {app_name}, Window: {title}");
            if let Some(url) = url {
                text.push_str(&format!(", URL: {url}"));
            }
            (format!("{text}\n\n{query}"), None)
        }
        None => (query.to_string(), None),
    }
}

/// Longest PDF text sent with a question, in characters.
const MAX_PDF_TEXT: usize = 200_000;

/// The text of a PDF, through `pdftotext`. Providers that speak the
/// OpenAI-compatible API (AnyModel, OpenRouter…) take no documents, so the
/// chat sends a PDF's text instead of the file. None when there is no
/// pdftotext or no text to get (a scan), and the file goes as it is.
async fn pdf_text(path: &str) -> Option<String> {
    if !path.to_lowercase().ends_with(".pdf") {
        return None;
    }
    let exe = platform::find_on_path("pdftotext")?;
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(exe).args(["-enc", "UTF-8", "-layout", "--", path, "-"]).stdin(Stdio::null()).output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || text.is_empty() {
        return None;
    }
    let cut: String = text.chars().take(MAX_PDF_TEXT).collect();
    let note = if cut.len() < text.len() { "\n[… the rest of the PDF was cut]" } else { "" };
    Some(format!("{cut}{note}"))
}

/// "provider/model" → opencode's model object. The model part may itself
/// contain slashes ("openrouter/minimax/minimax-m3:free").
pub fn model_ref(model: &str) -> Option<Value> {
    let (provider, id) = model.split_once('/')?;
    (!provider.is_empty() && !id.is_empty()).then(|| json!({ "providerID": provider, "modelID": id }))
}

fn mime_of(path: &str) -> &'static str {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "text/plain",
    }
}

/// The body of one `prompt_async`.
pub fn prompt_body(model: &str, text: &str, file: Option<&str>) -> Value {
    let mut parts = Vec::new();
    if let Some(path) = file {
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        parts.push(json!({ "type": "file", "mime": mime_of(path), "filename": name, "url": format!("file://{path}") }));
    }
    parts.push(json!({ "type": "text", "text": text }));
    let mut body = json!({ "agent": AGENT, "parts": parts });
    if let Some(m) = model_ref(model) {
        body["model"] = m;
    }
    body
}

/// One turn's view of the event stream: which parts are the answer's text,
/// which are tools, and when the session is done.
#[derive(Default)]
pub struct TurnState {
    session: String,
    assistant: HashSet<String>,
    kinds: HashMap<String, String>,
    /// The answer's text parts, in order, with their text so far.
    texts: Vec<(String, String)>,
    busy: bool,
    pub done: bool,
    pub error: Option<String>,
}

impl TurnState {
    pub fn new(session: &str) -> Self {
        Self { session: session.to_string(), ..Default::default() }
    }

    fn mine(&self, id: Option<&str>) -> bool {
        id == Some(self.session.as_str())
    }

    fn has_text(&self) -> bool {
        self.texts.iter().any(|(_, t)| !t.is_empty())
    }

    /// One event from `/event`; returns what the island should show of it.
    pub fn feed(&mut self, event: &Value) -> Vec<StreamUpdate> {
        let p = &event["properties"];
        match event["type"].as_str().unwrap_or("") {
            "message.updated" => {
                let info = &p["info"];
                if self.mine(info["sessionID"].as_str()) && info["role"] == "assistant" {
                    if let Some(id) = info["id"].as_str() {
                        self.assistant.insert(id.to_string());
                    }
                    if let Some(err) = info.get("error").filter(|e| e.is_object()) {
                        self.error = Some(error_text(err));
                    }
                }
                vec![]
            }
            "message.part.updated" => {
                let part = &p["part"];
                if !self.mine(part["sessionID"].as_str())
                    || !part["messageID"].as_str().is_some_and(|m| self.assistant.contains(m))
                {
                    return vec![];
                }
                let (Some(id), Some(kind)) = (part["id"].as_str(), part["type"].as_str()) else { return vec![] };
                let new = self.kinds.insert(id.to_string(), kind.to_string()).is_none();
                match kind {
                    "text" => {
                        let text = part["text"].as_str().unwrap_or("").to_string();
                        match self.texts.iter_mut().find(|(pid, _)| pid == id) {
                            Some((_, t)) => {
                                // The part's final text is the truth; deltas were the preview.
                                if !text.is_empty() {
                                    *t = text;
                                }
                            }
                            None => self.texts.push((id.to_string(), text)),
                        }
                        vec![]
                    }
                    "tool" if new => vec![StreamUpdate::Tool { name: part["tool"].as_str().unwrap_or("tool").to_string() }],
                    _ => vec![],
                }
            }
            "message.part.delta" => {
                if !self.mine(p["sessionID"].as_str()) || p["field"] != "text" {
                    return vec![];
                }
                let (Some(id), Some(delta)) = (p["partID"].as_str(), p["delta"].as_str()) else { return vec![] };
                if self.kinds.get(id).map(String::as_str) != Some("text") || delta.is_empty() {
                    return vec![];
                }
                let others_have_text = self.texts.iter().any(|(pid, t)| pid != id && !t.is_empty());
                let Some((_, t)) = self.texts.iter_mut().find(|(pid, _)| pid == id) else { return vec![] };
                let mut piece = String::new();
                if t.is_empty() && others_have_text {
                    piece.push_str("\n\n");
                }
                piece.push_str(delta);
                t.push_str(delta);
                vec![StreamUpdate::Text { text: piece }]
            }
            "session.status" => {
                if self.mine(p["sessionID"].as_str()) {
                    match p["status"]["type"].as_str() {
                        Some("busy") => self.busy = true,
                        Some("idle") if self.busy || self.error.is_some() || self.has_text() => self.done = true,
                        _ => {}
                    }
                }
                vec![]
            }
            "session.idle" => {
                if self.mine(p["sessionID"].as_str()) && (self.busy || self.error.is_some() || self.has_text()) {
                    self.done = true;
                }
                vec![]
            }
            "session.error" => {
                if self.mine(p["sessionID"].as_str()) || p["sessionID"].is_null() {
                    if let Some(err) = p.get("error").filter(|e| e.is_object()) {
                        self.error = Some(error_text(err));
                    }
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// The whole answer: the text parts, one paragraph apart.
    pub fn answer(&self) -> String {
        self.texts
            .iter()
            .map(|(_, t)| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

fn error_text(err: &Value) -> String {
    if err["name"] == "MessageAbortedError" {
        return "Stopped.".into();
    }
    err["data"]["message"]
        .as_str()
        .or(err["message"].as_str())
        .or(err["name"].as_str())
        .unwrap_or("opencode could not answer.")
        .to_string()
}

/// Splits an event-stream buffer into complete events' JSON, leaving any
/// unfinished one in `buf`.
pub fn take_events(buf: &mut Vec<u8>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(end) = buf.windows(2).position(|w| w == b"\n\n") {
        let frame: Vec<u8> = buf.drain(..end + 2).collect();
        let frame = String::from_utf8_lossy(&frame);
        let data: String = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            out.push(v);
        }
    }
    out
}

/// The running server: where it listens and the password it wants.
#[derive(Clone)]
struct Server {
    url: String,
    password: String,
    stderr: Arc<Mutex<String>>,
}

#[derive(Default)]
pub struct OpenCodeChat {
    /// Held for a whole turn: one turn at a time.
    turn: tokio::sync::Mutex<()>,
    child: Mutex<Option<Child>>,
    server: Mutex<Option<Server>>,
    session: Mutex<Option<String>>,
    /// Bumped by every turn and stop; an idle timer only fires if it is unchanged.
    generation: AtomicU64,
}

impl OpenCodeChat {
    fn alive(&self) -> bool {
        self.child.lock().unwrap().as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    pub fn status(&self) -> claude_cli::CliStatus {
        claude_cli::CliStatus { running: self.alive(), session_id: self.session.lock().unwrap().clone() }
    }

    /// Ends the server, and with it any answer being written. The
    /// conversation goes on with the next message, on a new server.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        *self.server.lock().unwrap() = None;
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
            crate::log::line("chat: opencode stopped");
        }
    }

    /// A new conversation. The server stays: it serves any session.
    pub fn reset(&self) {
        *self.session.lock().unwrap() = None;
    }

    /// Continue session `id` with the next message; returns what was said so
    /// far, for the island to show.
    pub async fn resume(&self, id: &str) -> Result<Vec<TranscriptMessage>, String> {
        let messages = transcript(id).await?;
        *self.session.lock().unwrap() = Some(id.to_string());
        Ok(messages)
    }

    /// Stops the server after `idle` without a new turn. Zero: never.
    pub fn stop_when_idle(self: &Arc<Self>, idle: Duration) {
        if idle.is_zero() {
            return;
        }
        let me = self.clone();
        let generation = self.generation.load(Ordering::Relaxed);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(idle).await;
            if me.generation.load(Ordering::Relaxed) == generation {
                crate::log::line("chat: opencode idle");
                me.stop();
            }
        });
    }

    async fn server(&self) -> Result<Server, String> {
        if self.alive() {
            if let Some(server) = self.server.lock().unwrap().clone() {
                return Ok(server);
            }
        }
        self.stop();
        let exe = find_opencode().ok_or(
            "opencode is not installed: `opencode` is nowhere on PATH. Install it, or pick another chat backend in Settings.",
        )?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("No free port for opencode: {e}"))?;
        let password = claude_cli::new_session_id();

        let mut cmd = command(&exe);
        cmd.args(["serve", "--pure", "--hostname", "127.0.0.1", &format!("--port={port}")])
            .env("OPENCODE_SERVER_PASSWORD", &password);
        let mut child = cmd.spawn().map_err(|e| format!("Could not start opencode: {e}"))?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr")?;
        *self.child.lock().unwrap() = Some(child);

        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = stderr.clone();
        tauri::async_runtime::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok(n) = stderr_pipe.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut tail = sink.lock().unwrap();
                tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                if tail.len() > 4096 {
                    let cut = tail.len() - 4096;
                    let cut = (cut..tail.len()).find(|&i| tail.is_char_boundary(i)).unwrap_or(0);
                    tail.drain(..cut);
                }
            }
        });

        // "opencode server listening on http://127.0.0.1:PORT", then nothing
        // that matters: the rest of stdout is drained so it never blocks.
        let mut lines = BufReader::new(stdout).lines();
        let url = tokio::time::timeout(Duration::from_secs(30), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(at) = line.find("http://") {
                    return Some(line[at..].trim().to_string());
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        tauri::async_runtime::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let Some(url) = url else {
            let tail = stderr.lock().unwrap().clone();
            self.stop();
            return Err(friendly_error(if tail.trim().is_empty() { "opencode did not start." } else { &tail }));
        };
        crate::log::line("chat: opencode started");
        let server = Server { url, password, stderr };
        *self.server.lock().unwrap() = Some(server.clone());
        Ok(server)
    }

    /// One chat turn. `on_update` gets the answer as it is written, and the
    /// tools it uses.
    pub async fn send(
        &self,
        model: &str,
        query: String,
        context: Option<ChatContext>,
        on_update: impl Fn(StreamUpdate),
    ) -> Result<(ChatReply, String), String> {
        let _turn = self.turn.lock().await;
        self.generation.fetch_add(1, Ordering::Relaxed);
        platform::ensure_private_dir(&claude_cli::work_dir()).map_err(|e| format!("Chat folder: {e}"))?;
        let server = self.server().await?;
        let result = self.turn(&server, model, &query, context.as_ref(), &on_update).await;
        if let Err(err) = &result {
            crate::log::line(format!("chat: opencode failed: {}", err.chars().take(160).collect::<String>()));
            if !self.alive() {
                let tail = server.stderr.lock().unwrap().clone();
                if !tail.trim().is_empty() {
                    return Err(friendly_error(&tail));
                }
            }
        }
        result.map_err(|e| friendly_error(&e))
    }

    async fn turn(
        &self,
        server: &Server,
        model: &str,
        query: &str,
        context: Option<&ChatContext>,
        on_update: &impl Fn(StreamUpdate),
    ) -> Result<(ChatReply, String), String> {
        let client = reqwest::Client::builder().no_proxy().build().map_err(|e| e.to_string())?;
        let request = |method: reqwest::Method, path: &str| {
            client
                .request(method, format!("{}{path}", server.url))
                .basic_auth("opencode", Some(&server.password))
                .timeout(Duration::from_secs(30))
        };

        let existing = self.session.lock().unwrap().clone();
        let (mut text, mut file) = if existing.is_none() { first_message(query, context) } else { (query.to_string(), None) };
        if let Some(pdf) = file.clone() {
            if let Some(body) = pdf_text(&pdf).await {
                text = format!("Text of the PDF:\n<<<\n{body}\n>>>\n\n{text}");
                file = None;
            }
        }
        let session = match existing {
            Some(id) => id,
            None => {
                let title: String = query.lines().next().unwrap_or("").chars().take(80).collect();
                let created: Value = request(reqwest::Method::POST, "/session")
                    .json(&json!({ "title": title }))
                    .send()
                    .await
                    .map_err(|e| format!("opencode: {e}"))?
                    .error_for_status()
                    .map_err(|e| format!("opencode: {e}"))?
                    .json()
                    .await
                    .map_err(|e| format!("opencode: {e}"))?;
                let id = created["id"].as_str().filter(|id| is_session_id(id)).ok_or("opencode made no session.")?;
                *self.session.lock().unwrap() = Some(id.to_string());
                id.to_string()
            }
        };

        // Listen first, so nothing of the answer is missed.
        let mut events = client
            .get(format!("{}/event", server.url))
            .basic_auth("opencode", Some(&server.password))
            .send()
            .await
            .map_err(|e| format!("opencode: {e}"))?
            .error_for_status()
            .map_err(|e| format!("opencode: {e}"))?;
        let mut buf: Vec<u8> = Vec::new();
        let mut state = TurnState::new(&session);
        let connected = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.chunk().await {
                    Ok(Some(chunk)) => {
                        buf.extend_from_slice(&chunk);
                        let got = take_events(&mut buf);
                        if got.iter().any(|e| e["type"] == "server.connected") {
                            return true;
                        }
                    }
                    _ => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        if !connected {
            return Err("opencode is not answering.".into());
        }

        request(reqwest::Method::POST, &format!("/session/{session}/prompt_async"))
            .json(&prompt_body(model, &text, file.as_deref()))
            .send()
            .await
            .map_err(|e| format!("opencode: {e}"))?
            .error_for_status()
            .map_err(|e| format!("opencode: {e}"))?;

        while !state.done {
            let chunk = tokio::time::timeout(SILENCE_LIMIT, events.chunk())
                .await
                .map_err(|_| "opencode stopped answering.".to_string())?
                .map_err(|_| "opencode stopped.".to_string())?
                .ok_or("opencode stopped.")?;
            buf.extend_from_slice(&chunk);
            for event in take_events(&mut buf) {
                for update in state.feed(&event) {
                    on_update(update);
                }
            }
        }

        if let Some(err) = state.error {
            return Err(err);
        }
        let answer = state.answer();
        if answer.is_empty() {
            return Err("No response text.".into());
        }
        Ok((ChatReply { text: answer }, session))
    }
}

// ── Saved sessions ────────────────────────────────────────────────────────────

async fn output(args: &[&str]) -> Result<Vec<u8>, String> {
    let exe = find_opencode().ok_or("opencode is not installed.")?;
    let mut cmd = command(&exe);
    cmd.args(args).stderr(Stdio::null());
    let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| "opencode took too long to answer.".to_string())?
        .map_err(|e| format!("Could not start opencode: {e}"))?;
    if !out.status.success() {
        return Err("opencode could not read its sessions.".into());
    }
    Ok(out.stdout)
}

/// The chat's own conversations, newest first: those opencode ran in the
/// chat's working directory.
pub async fn list_sessions(limit: usize) -> Vec<SessionInfo> {
    let Ok(out) = output(&["session", "list", "--format", "json", "--max-count", "100"]).await else {
        return Vec::new();
    };
    let dir = claude_cli::work_dir();
    let dir = dir.to_string_lossy();
    parse_session_list(&out, &dir).into_iter().take(limit).collect()
}

pub fn parse_session_list(json: &[u8], dir: &str) -> Vec<SessionInfo> {
    let Ok(Value::Array(rows)) = serde_json::from_slice::<Value>(json) else { return Vec::new() };
    let mut sessions: Vec<SessionInfo> = rows
        .iter()
        .filter(|r| r["directory"].as_str() == Some(dir))
        .filter_map(|r| {
            let id = r["id"].as_str().filter(|id| is_session_id(id))?;
            Some(SessionInfo {
                id: id.to_string(),
                title: r["title"].as_str().unwrap_or("").chars().take(80).collect(),
                modified: r["updated"].as_u64().unwrap_or(0) / 1000,
                messages: None,
            })
        })
        .collect();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

async fn transcript(id: &str) -> Result<Vec<TranscriptMessage>, String> {
    if !is_session_id(id) {
        return Err("That conversation is gone.".into());
    }
    let out = output(&["export", id]).await.map_err(|_| "That conversation is gone.".to_string())?;
    // `export` may print a line of its own before the JSON.
    let text = String::from_utf8_lossy(&out);
    let start = text.find('{').ok_or("That conversation is gone.")?;
    Ok(parse_export(&text[start..]))
}

/// The visible conversation in `opencode export`: what the user typed and
/// the answers' text, without reasoning, tools, attachments or the context
/// line the first message starts with.
pub fn parse_export(json: &str) -> Vec<TranscriptMessage> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    let Some(messages) = v["messages"].as_array() else { return Vec::new() };
    messages
        .iter()
        .filter_map(|m| {
            let role = m["info"]["role"].as_str().filter(|r| *r == "user" || *r == "assistant")?;
            let texts: Vec<&str> = m["parts"]
                .as_array()?
                .iter()
                .filter(|p| p["type"] == "text" && p["synthetic"].as_bool() != Some(true))
                .filter_map(|p| p["text"].as_str())
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .collect();
            let mut content = if role == "user" { texts.last()?.to_string() } else { texts.join("\n\n") };
            if role == "user" && (content.starts_with("Context — ") || content.starts_with("File: ")) {
                if let Some((_, rest)) = content.split_once("\n\n") {
                    content = rest.to_string();
                }
            }
            (!content.is_empty()).then(|| TranscriptMessage { role: role.to_string(), content })
        })
        .collect()
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Install {
    pub path: Option<String>,
    pub version: Option<String>,
}

pub async fn install_status() -> Install {
    let Some(exe) = find_opencode() else { return Install::default() };
    let version = output(&["--version"])
        .await
        .ok()
        .map(|v| String::from_utf8_lossy(&v).trim().to_string())
        .filter(|v| !v.is_empty());
    Install { path: Some(exe.to_string_lossy().to_string()), version }
}

/// Every model opencode can use with the providers set up: "provider/model".
pub async fn models() -> Vec<String> {
    let Ok(out) = output(&["models"]).await else { return Vec::new() };
    String::from_utf8_lossy(&out)
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(' '))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to the real `opencode`: run by hand with
    /// `COUCOU_TEST_OPENCODE_MODEL=provider/model cargo test --lib -- --ignored real_opencode`.
    #[test]
    #[ignore]
    fn real_opencode_answers_continues_and_lists() {
        let model = std::env::var("COUCOU_TEST_OPENCODE_MODEL").unwrap_or_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let chat = OpenCodeChat::default();
            let pieces = Mutex::new(0usize);
            let (reply, id) = chat
                .send(&model, "Remember the number 42. Answer with one word: ok.".into(), None, |u| {
                    if matches!(u, StreamUpdate::Text { .. }) {
                        *pieces.lock().unwrap() += 1;
                    }
                })
                .await
                .expect("first turn");
            assert!(*pieces.lock().unwrap() > 0, "the answer must stream");
            assert!(chat.status().running);
            assert!(!reply.text.is_empty());
            assert!(is_session_id(&id), "{id}");

            let (reply, again) = chat.send(&model, "Which number? Digits only.".into(), None, |_| {}).await.expect("second turn");
            assert_eq!(again, id);
            assert!(reply.text.contains("42"), "{}", reply.text);

            chat.stop();
            assert!(!chat.status().running);
            let listed = list_sessions(12).await;
            assert!(listed.iter().any(|s| s.id == id), "{id} not in {listed:?}");
            let fresh = OpenCodeChat::default();
            let shown = fresh.resume(&id).await.expect("transcript");
            assert_eq!(shown.len(), 4, "{shown:?}");
            assert_eq!(shown[0].content, "Remember the number 42. Answer with one word: ok.");
        });
    }

    #[test]
    fn a_pdfs_text_comes_through_pdftotext() {
        if platform::find_on_path("pdftotext").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("coucou-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("s.pdf");
        // A one-page PDF saying "Secret word: blueberry".
        let stream = b"BT /F1 18 Tf 20 50 Td (Secret word: blueberry) Tj ET";
        let objs: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
            [format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(), stream.to_vec(), b"\nendstream".to_vec()].concat(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
            out.extend(o);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).into_bytes());
        for o in offs {
            out.extend(format!("{o:010} 00000 n \n").into_bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).into_bytes());
        std::fs::write(&pdf, out).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let text = rt.block_on(pdf_text(pdf.to_str().unwrap())).expect("text");
        assert!(text.contains("Secret word: blueberry"), "{text}");
        assert!(rt.block_on(pdf_text("/x/not-a-pdf.txt")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn session_ids_are_opencodes_and_nothing_else() {
        assert!(is_session_id("ses_ef92c1b5bffeIrOHp90IEeCzYH"));
        assert!(!is_session_id("ses_../../etc/passwdxxxxxxxxxxx"));
        assert!(!is_session_id("--session=ses_ef92c1b5bffeIrOH"));
        assert!(!is_session_id("f2032dcc-af80-4924-ba0b-3231991b3a4d"));
    }



    /// Events recorded from a real `opencode serve` turn, trimmed.
    fn recorded_turn(session: &str) -> Vec<Value> {
        let s = session;
        [
            json!({"type":"server.connected","properties":{}}),
            json!({"type":"message.updated","properties":{"info":{"id":"msg_user","role":"user","sessionID":s}}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_u","type":"text","messageID":"msg_user","sessionID":s,"text":"Назови цвета"}}}),
            json!({"type":"session.status","properties":{"sessionID":s,"status":{"type":"busy"}}}),
            json!({"type":"message.updated","properties":{"info":{"id":"msg_a","role":"assistant","sessionID":s}}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_r","type":"reasoning","messageID":"msg_a","sessionID":s,"text":""}}}),
            json!({"type":"message.part.delta","properties":{"sessionID":s,"messageID":"msg_a","partID":"prt_r","field":"text","delta":"We need"}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_t1","type":"text","messageID":"msg_a","sessionID":s,"text":""}}}),
            json!({"type":"message.part.delta","properties":{"sessionID":s,"messageID":"msg_a","partID":"prt_t1","field":"text","delta":"Крас"}}),
            json!({"type":"message.part.delta","properties":{"sessionID":"ses_otherotherotherotherother1","partID":"prt_t1","field":"text","delta":"чужое"}}),
            json!({"type":"message.part.delta","properties":{"sessionID":s,"messageID":"msg_a","partID":"prt_t1","field":"text","delta":"ный."}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_tool","type":"tool","tool":"websearch","messageID":"msg_a","sessionID":s,"state":{"status":"running"}}}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_tool","type":"tool","tool":"websearch","messageID":"msg_a","sessionID":s,"state":{"status":"completed"}}}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_t2","type":"text","messageID":"msg_a","sessionID":s,"text":""}}}),
            json!({"type":"message.part.delta","properties":{"sessionID":s,"messageID":"msg_a","partID":"prt_t2","field":"text","delta":"Синий."}}),
            json!({"type":"message.part.updated","properties":{"part":{"id":"prt_t1","type":"text","messageID":"msg_a","sessionID":s,"text":"Красный."}}}),
            json!({"type":"session.status","properties":{"sessionID":s,"status":{"type":"idle"}}}),
            json!({"type":"session.idle","properties":{"sessionID":s}}),
        ]
        .into()
    }

    #[test]
    fn a_turn_streams_the_answer_and_nothing_else() {
        let s = "ses_ef9067a48ffe99L20IIr3j3IPl";
        let mut state = TurnState::new(s);
        let mut updates = Vec::new();
        let events = recorded_turn(s);
        for (i, event) in events.iter().enumerate() {
            updates.extend(state.feed(event));
            // Done on the idle status, not a moment before.
            assert_eq!(state.done, i >= events.len() - 2, "event {i}");
        }
        assert!(state.done);
        assert_eq!(state.error, None);
        assert_eq!(
            updates,
            [
                StreamUpdate::Text { text: "Крас".into() },
                StreamUpdate::Text { text: "ный.".into() },
                StreamUpdate::Tool { name: "websearch".into() },
                StreamUpdate::Text { text: "\n\nСиний.".into() },
            ]
        );
        assert_eq!(state.answer(), "Красный.\n\nСиний.");
    }

    #[test]
    fn a_failed_turn_says_why() {
        let s = "ses_ef9067a48ffe99L20IIr3j3IPl";
        let mut state = TurnState::new(s);
        state.feed(&json!({"type":"session.status","properties":{"sessionID":s,"status":{"type":"busy"}}}));
        state.feed(&json!({"type":"session.error","properties":{"sessionID":s,"error":{"name":"APIError","data":{"message":"Insufficient balance"}}}}));
        state.feed(&json!({"type":"session.idle","properties":{"sessionID":s}}));
        assert!(state.done);
        assert_eq!(state.error.as_deref(), Some("Insufficient balance"));

        let mut idle_first = TurnState::new(s);
        idle_first.feed(&json!({"type":"session.idle","properties":{"sessionID":s}}));
        assert!(!idle_first.done, "an idle before the turn started is not its end");
    }

    #[test]
    fn the_event_stream_is_cut_into_whole_events() {
        let mut buf = b"data: {\"type\":\"a\"}\n\ndata: {\"type\":\"b\",\"x\":\"\xd0\xbf".to_vec();
        let got = take_events(&mut buf);
        assert_eq!(got, [json!({"type":"a"})]);
        buf.extend_from_slice(b"\xd1\x80\"}\n\n");
        assert_eq!(take_events(&mut buf), [json!({"type":"b","x":"пр"})]);
        assert!(buf.is_empty());
    }

    #[test]
    fn models_and_prompts_take_opencodes_shape() {
        assert_eq!(model_ref("anymodel/am/kimi-k3"), Some(json!({"providerID":"anymodel","modelID":"am/kimi-k3"})));
        assert_eq!(model_ref(""), None);
        assert_eq!(model_ref("nomodel"), None);
        let body = prompt_body("", "Кратко?", Some("/x/inbox/a b.pdf"));
        assert_eq!(body["agent"], AGENT);
        assert!(body.get("model").is_none());
        assert_eq!(body["parts"][0], json!({"type":"file","mime":"application/pdf","filename":"a b.pdf","url":"file:///x/inbox/a b.pdf"}));
        assert_eq!(body["parts"][1], json!({"type":"text","text":"Кратко?"}));
    }

    #[test]
    fn the_agent_gets_web_search_and_nothing_else() {
        let c: Value = serde_json::from_str(&config()).unwrap();
        let agent = &c["agent"][AGENT];
        assert_eq!(agent["tools"], json!({ "*": false, "websearch": true }));
        assert_eq!(agent["permission"]["*"], "deny");
        assert_eq!(agent["prompt"], claude::SYSTEM_PROMPT);
    }

    #[test]
    fn the_first_message_carries_the_context() {
        let window = ChatContext::Window { app_name: "Firefox".into(), title: "Docs".into(), url: None };
        assert_eq!(first_message("Что это?", Some(&window)), ("Context — App: Firefox, Window: Docs\n\nЧто это?".into(), None));
        let file = ChatContext::File { name: "a.txt".into(), path: "/x/a.txt".into() };
        assert_eq!(first_message("Кратко?", Some(&file)), ("File: a.txt\n\nКратко?".into(), Some("/x/a.txt".into())));
    }

    #[test]
    fn only_the_chats_own_sessions_are_listed() {
        let json = br#"[
          {"id":"ses_aaaaaaaaaaaaaaaaaaaaaaaaaa","title":"Old","updated":1000000,"directory":"/c"},
          {"id":"ses_bbbbbbbbbbbbbbbbbbbbbbbbbb","title":"Elsewhere","updated":3000000,"directory":"/proj"},
          {"id":"ses_cccccccccccccccccccccccccc","title":"New","updated":2000000,"directory":"/c"},
          {"id":"../evil","title":"x","updated":4000000,"directory":"/c"}
        ]"#;
        let got = parse_session_list(json, "/c");
        let titles: Vec<&str> = got.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["New", "Old"]);
        assert_eq!(got[0].modified, 2000);
        assert_eq!(got[0].messages, None);
    }

    #[test]
    fn an_export_keeps_only_what_was_said() {
        let json = r#"{"info":{},"messages":[
          {"info":{"role":"user"},"parts":[{"type":"text","text":"Context — App: Firefox, Window: Docs\n\nЧто это?"},{"type":"text","text":"file body","synthetic":true}]},
          {"info":{"role":"assistant"},"parts":[{"type":"step-start"},{"type":"reasoning","text":"hmm"},{"type":"text","text":"Документация."},{"type":"tool","tool":"websearch"},{"type":"text","text":"Ещё."}]},
          {"info":{"role":"user"},"parts":[{"type":"text","text":"Спасибо"}]}
        ]}"#;
        let got = parse_export(json);
        let pairs: Vec<(&str, &str)> = got.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect();
        assert_eq!(pairs, [("user", "Что это?"), ("assistant", "Документация.\n\nЕщё."), ("user", "Спасибо")]);
    }

    #[test]
    fn missing_providers_say_what_to_do() {
        assert!(friendly_error("ProviderModelNotFoundError").contains("opencode auth login"));
        assert_eq!(friendly_error("Overloaded"), "Overloaded");
        assert_eq!(friendly_error(" "), "opencode stopped without an answer.");
    }
}
