// Chat through Claude Code (`claude -p`) and the user's own subscription,
// instead of an API key.
//
// One hidden `claude` process per conversation, started on the first message
// and kept alive: it reads user messages as stream-json on stdin and streams
// the answer back as stream-json on stdout, so only the first message pays for
// the start. It runs lean and harmless: web search only — no file access, no
// edits, no commands, no MCP servers, no plugins, no hooks (Coucou's own hooks
// would otherwise report the chat as a Claude Code session). It is stopped on
// demand, after a while idle, and with the app.
//
// Claude Code saves the conversation in ~/.claude/projects/, like any other
// session; `--resume` picks it up again in a new process.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::claude::{self, ChatContext, ChatReply};
use crate::platform;

pub const DEFAULT_MODEL: &str = "sonnet";

/// The only tool the chat gets, as with the API key. Everything else does not
/// exist in its session at all — Read and WebFetch included: together they
/// would let a prompt injected through a web page or a dropped file read any
/// file of the user's (~/.ssh, Claude's own credentials) and send it off in a
/// URL. A dropped file needs neither: it rides in the first message.
const TOOLS: &str = "WebSearch";

/// Longest silence inside one answer before the process is presumed stuck. A
/// web search can take a while, but never this long.
const SILENCE_LIMIT: Duration = Duration::from_secs(180);

/// Set in the child's environment: coucou-hook exits on it at once, a second
/// guard next to `disableAllHooks`.
pub const CHAT_ENV: &str = "COUCOU_CHAT";

/// Something the island shows while the answer is still coming.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StreamUpdate {
    /// More answer text.
    Text { text: String },
    /// A tool started: web search, fetch or read.
    Tool { name: String },
}

/// One stdout line, reduced to what matters here.
#[derive(Debug, PartialEq)]
pub enum Line {
    Update(StreamUpdate),
    /// The turn is over. `text` is the whole answer, or the error.
    Result { ok: bool, text: String },
    Ignored,
}

pub fn parse_line(line: &str) -> Line {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Line::Ignored };
    match v.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let event = &v["event"];
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                    match event["delta"]["text"].as_str() {
                        Some(t) if !t.is_empty() => Line::Update(StreamUpdate::Text { text: t.to_string() }),
                        _ => Line::Ignored,
                    }
                }
                Some("content_block_start") => {
                    let block = &event["content_block"];
                    match (block["type"].as_str(), block["name"].as_str()) {
                        (Some("tool_use" | "server_tool_use"), Some(name)) => {
                            Line::Update(StreamUpdate::Tool { name: name.to_string() })
                        }
                        _ => Line::Ignored,
                    }
                }
                _ => Line::Ignored,
            }
        }
        Some("result") => {
            let failed = v["is_error"].as_bool().unwrap_or(false)
                || v["subtype"].as_str().is_some_and(|s| s != "success");
            let text = v["result"].as_str().unwrap_or("").trim().to_string();
            Line::Result { ok: !failed, text }
        }
        _ => Line::Ignored,
    }
}

/// What the island shows when the turn failed: Claude Code's own words, unless
/// they mean the user has to sign in first.
pub fn friendly_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let signed_out = ["/login", "not logged in", "please log in", "please run", "oauth", "authenticat", "invalid api key"]
        .iter()
        .any(|k| lower.contains(k));
    if signed_out {
        return "Claude Code is not signed in. Run `claude auth login` in a terminal, then try again.".into();
    }
    let raw = raw.trim();
    if raw.is_empty() {
        "Claude Code stopped without an answer.".into()
    } else {
        raw.chars().take(300).collect()
    }
}

// ── Sessions on disk ──────────────────────────────────────────────────────────

/// Neutral folder the chat runs in, so its sessions never mix with a project's.
pub fn work_dir() -> PathBuf {
    platform::local_dir().join("chat")
}

/// Where Claude Code keeps its sessions.
fn projects_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| platform::home_dir().join(".claude"))
        .join("projects")
}

/// The transcript of session `id`. Claude Code names the project folder after
/// the working directory with its own escaping, so look for the file instead
/// of guessing the folder.
fn transcript_path(id: &str) -> Option<PathBuf> {
    if !is_session_id(id) {
        return None;
    }
    let file = format!("{id}.jsonl");
    std::fs::read_dir(projects_dir())
        .ok()?
        .filter_map(Result::ok)
        .map(|dir| dir.path().join(&file))
        .find(|p| p.is_file())
}

/// Claude Code's folder name for a working directory: every character that is
/// not an ASCII letter or digit becomes a dash.
fn project_folder_name(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    /// The first thing the user asked.
    pub title: String,
    /// Last change, in seconds since 1970.
    pub modified: u64,
    /// How many messages, when the backend says (opencode's list does not).
    pub messages: Option<usize>,
}

/// The chat's own saved conversations, newest first. Only those started from
/// Coucou: they all live in the folder named after the chat's working directory.
pub fn list_sessions(limit: usize) -> Vec<SessionInfo> {
    let dir = projects_dir().join(project_folder_name(&work_dir()));
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_name()?.to_str()?.strip_suffix(".jsonl")?.to_string();
            let modified = e.metadata().ok()?.modified().ok()?;
            is_session_id(&id).then_some((modified, id, path))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .filter_map(|(modified, id, path)| {
            let messages = parse_transcript(&std::fs::read_to_string(&path).ok()?);
            let first = messages.iter().find(|m| m.role == "user")?;
            Some(SessionInfo {
                id,
                title: first.content.lines().next().unwrap_or("").chars().take(80).collect(),
                modified: modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                messages: Some(messages.len()),
            })
        })
        .take(limit)
        .collect()
}

/// A UUID, and nothing that could walk out of the projects folder.
pub fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptMessage {
    pub role: String,
    pub content: String,
}

/// The visible conversation in a session file: user prompts and Claude's text,
/// without tool calls, tool results or the file context.
pub fn parse_transcript(jsonl: &str) -> Vec<TranscriptMessage> {
    let mut out: Vec<TranscriptMessage> = Vec::new();
    let mut last_assistant_id: Option<String> = None;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let role = match v["type"].as_str() {
            Some(r @ ("user" | "assistant")) => r,
            _ => continue,
        };
        if v["isMeta"].as_bool() == Some(true) || v["isSidechain"].as_bool() == Some(true) {
            continue;
        }
        let content = &v["message"]["content"];
        let text = match content {
            Value::String(s) => s.clone(),
            Value::Array(blocks) => {
                if blocks.iter().any(|b| b["type"] == "tool_result") {
                    continue;
                }
                // The first message also carries the file it is about; only the
                // last text block is what the user typed.
                let texts: Vec<&str> = blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect();
                if role == "user" {
                    texts.last().copied().unwrap_or("").to_string()
                } else {
                    texts.join("\n")
                }
            }
            _ => continue,
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        // One answer may be saved as several lines sharing a message id.
        let id = v["message"]["id"].as_str().map(str::to_string);
        if role == "assistant" {
            if let (Some(prev), Some(id)) = (out.last_mut(), id.as_ref()) {
                if prev.role == "assistant" && last_assistant_id.as_ref() == Some(id) {
                    prev.content.push('\n');
                    prev.content.push_str(&text);
                    continue;
                }
            }
            last_assistant_id = id;
        }
        out.push(TranscriptMessage { role: role.to_string(), content: text });
    }
    out
}

// ── The process ───────────────────────────────────────────────────────────────

/// The `claude` executable: on PATH, or where its installer puts it — an app
/// started from the desktop does not always get ~/.local/bin on its PATH.
pub fn find_claude() -> Option<PathBuf> {
    platform::find_on_path("claude").or_else(|| {
        let home = platform::home_dir();
        [home.join(".local/bin/claude"), home.join(".claude/local/claude")]
            .into_iter()
            .find(|p| p.is_file())
    })
}

struct Io {
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    /// Tail of stderr, for the error message when the process dies.
    stderr: Arc<Mutex<String>>,
}

struct Session {
    id: String,
    /// Claude Code has saved it, so a new process must `--resume` it.
    saved: bool,
}

#[derive(Default)]
pub struct CliChat {
    /// The live process's pipes, held for a whole turn: one turn at a time.
    io: tokio::sync::Mutex<Option<Io>>,
    /// The process itself, apart from the pipes so it can be stopped mid-turn.
    child: Mutex<Option<Child>>,
    session: Mutex<Option<Session>>,
    /// Bumped by every turn and stop; an idle timer only fires if it is unchanged.
    generation: AtomicU64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub running: bool,
    pub session_id: Option<String>,
}

impl CliChat {
    pub fn status(&self) -> CliStatus {
        let running = self
            .child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        CliStatus { running, session_id: self.session.lock().unwrap().as_ref().map(|s| s.id.clone()) }
    }

    /// Ends the process. The conversation stays where it is: the next message
    /// starts a new process that resumes it.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
            crate::log::line("chat: claude stopped");
        }
    }

    /// A new conversation: the old process goes, the next message gets a new id.
    pub fn reset(&self) {
        self.stop();
        *self.session.lock().unwrap() = None;
    }

    /// Continue session `id` with the next message; returns what was said so
    /// far, for the island to show.
    pub fn resume(&self, id: &str) -> Result<Vec<TranscriptMessage>, String> {
        let path = transcript_path(id).ok_or("That conversation is gone.")?;
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        self.stop();
        *self.session.lock().unwrap() = Some(Session { id: id.to_string(), saved: true });
        Ok(parse_transcript(&text))
    }

    /// One chat turn. `on_update` gets the answer as it arrives.
    pub async fn send(
        &self,
        model: &str,
        query: String,
        context: Option<ChatContext>,
        on_update: impl Fn(StreamUpdate),
    ) -> Result<(ChatReply, String), String> {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let mut io = self.io.lock().await;

        // The process may have been stopped, timed out or died since last time.
        let alive = self
            .child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        if !alive {
            *io = None;
        }
        let first_turn = {
            let session = self.session.lock().unwrap();
            session.as_ref().is_none_or(|s| !s.saved)
        };
        if io.is_none() {
            *io = Some(self.spawn(model)?);
        }
        let pipes = io.as_mut().expect("just spawned");

        let mut content: Vec<Value> = Vec::new();
        if first_turn {
            content.extend(claude::context_blocks(context.as_ref()));
        }
        content.push(json!({ "type": "text", "text": query }));
        let message = json!({ "type": "user", "message": { "role": "user", "content": content } });

        let result = turn(pipes, &message, &on_update).await;
        let session_id = self.session.lock().unwrap().as_ref().map(|s| s.id.clone()).unwrap_or_default();
        match result {
            Ok(text) => {
                if let Some(s) = self.session.lock().unwrap().as_mut() {
                    s.saved = true;
                }
                Ok((ChatReply { text }, session_id))
            }
            Err(err) => {
                // Whatever state the process is in, it is not worth keeping.
                let tail = pipes.stderr.lock().unwrap().clone();
                *io = None;
                self.stop();
                crate::log::line(format!("chat: claude failed: {}", err.chars().take(120).collect::<String>()));
                Err(friendly_error(if err.is_empty() { &tail } else { &err }))
            }
        }
    }

    /// Stops the process after `idle` without a new turn. Zero: never.
    pub fn stop_when_idle(self: &Arc<Self>, idle: Duration) {
        if idle.is_zero() {
            return;
        }
        let me = self.clone();
        let generation = self.generation.load(Ordering::Relaxed);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(idle).await;
            if me.generation.load(Ordering::Relaxed) == generation {
                crate::log::line("chat: idle");
                me.stop();
            }
        });
    }

    fn spawn(&self, model: &str) -> Result<Io, String> {
        let exe = find_claude().ok_or(
            "Claude Code is not installed: `claude` is nowhere on PATH. Install it, or switch the chat to an API key in Settings.",
        )?;
        let dir = work_dir();
        platform::ensure_private_dir(&dir).map_err(|e| format!("Chat folder: {e}"))?;

        let (id, resume) = {
            let mut session = self.session.lock().unwrap();
            let s = session.get_or_insert_with(|| Session { id: new_session_id(), saved: false });
            (s.id.clone(), s.saved)
        };

        let mut cmd = Command::new(exe);
        cmd.args(args(model, &id, resume))
            .current_dir(&dir)
            .env(CHAT_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        die_with_parent(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| format!("Could not start Claude Code: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr")?;

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

        *self.child.lock().unwrap() = Some(child);
        crate::log::line(if resume { "chat: claude started (resuming)" } else { "chat: claude started" });
        Ok(Io { stdin, stdout: BufReader::new(stdout).lines(), stderr })
    }
}

/// Command line for the chat process. See the top of this file for why each
/// switch is there.
pub fn args(model: &str, session_id: &str, resume: bool) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--settings",
        r#"{"disableAllHooks":true}"#,
        "--tools",
        TOOLS,
        "--allowedTools",
        TOOLS,
        "--system-prompt",
        claude::SYSTEM_PROMPT,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !model.is_empty() {
        a.extend(["--model".into(), model.into()]);
    }
    a.extend([if resume { "--resume" } else { "--session-id" }.into(), session_id.into()]);
    a
}

async fn turn(io: &mut Io, message: &Value, on_update: &impl Fn(StreamUpdate)) -> Result<String, String> {
    let mut line = message.to_string();
    line.push('\n');
    io.stdin.write_all(line.as_bytes()).await.map_err(|_| String::new())?;
    io.stdin.flush().await.map_err(|_| String::new())?;

    let mut streamed = String::new();
    loop {
        let next = tokio::time::timeout(SILENCE_LIMIT, io.stdout.next_line())
            .await
            .map_err(|_| "Claude Code stopped answering.".to_string())?;
        // EOF or a broken pipe: the process is gone. Its stderr says why.
        let Some(line) = next.map_err(|_| String::new())? else { return Err(String::new()) };
        match parse_line(&line) {
            Line::Update(update) => {
                if let StreamUpdate::Text { text } = &update {
                    streamed.push_str(text);
                }
                on_update(update);
            }
            Line::Result { ok: true, text } => {
                let text = if text.is_empty() { streamed.trim().to_string() } else { text };
                return if text.is_empty() { Err("No response text.".into()) } else { Ok(text) };
            }
            Line::Result { ok: false, text } => {
                return Err(if text.is_empty() { "Claude Code could not answer.".into() } else { text });
            }
            Line::Ignored => {}
        }
    }
}

/// Linux: the chat process gets SIGTERM when Coucou goes, however it goes.
pub(crate) fn die_with_parent(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}


/// A random version-4 UUID. The randomness comes from the OS keys std's
/// `RandomState` is seeded with — not worth a dependency.
pub fn new_session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// `claude auth status`: whether the subscription can be used at all.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Install {
    pub path: Option<String>,
    pub logged_in: bool,
    pub auth_method: Option<String>,
}

pub async fn install_status() -> Install {
    let Some(exe) = find_claude() else { return Install::default() };
    let path = Some(exe.to_string_lossy().to_string());
    let mut cmd = Command::new(&exe);
    cmd.args(["auth", "status"]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(15), cmd.output()).await;
    let Ok(Ok(out)) = out else { return Install { path, ..Default::default() } };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    Install {
        path,
        logged_in: v["loggedIn"].as_bool().unwrap_or(false),
        auth_method: v["authMethod"].as_str().map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to the real `claude`, on the subscription: run by hand with
    /// `cargo test --lib -- --ignored real_claude`.
    #[test]
    #[ignore]
    fn real_claude_answers_streams_and_resumes() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let chat = CliChat::default();
            let pieces = Mutex::new(0usize);
            let (reply, id) = chat
                .send("haiku", "Remember the number 42. Answer with one word: ok.".into(), None, |u| {
                    if matches!(u, StreamUpdate::Text { .. }) {
                        *pieces.lock().unwrap() += 1;
                    }
                })
                .await
                .expect("first turn");
            assert!(!reply.text.is_empty());
            assert!(*pieces.lock().unwrap() > 0, "the answer must stream");
            assert!(chat.status().running);

            // Same process, second turn.
            let (reply, _) = chat.send("haiku", "Which number? Digits only.".into(), None, |_| {}).await.expect("second turn");
            assert!(reply.text.contains("42"), "{}", reply.text);

            // A new process resumes the saved conversation.
            chat.stop();
            assert!(!chat.status().running);
            let shown = chat.resume(&id).expect("transcript");
            assert_eq!(shown.len(), 4, "{shown:?}");
            let (reply, again) = chat.send("haiku", "And the number again? Digits only.".into(), None, |_| {}).await.expect("resumed turn");
            assert_eq!(again, id);
            assert!(reply.text.contains("42"), "{}", reply.text);
            chat.stop();
        });
    }

    #[test]
    fn text_deltas_and_tools_stream_through() {
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Пари"}},"session_id":"x"}"#;
        assert_eq!(parse_line(delta), Line::Update(StreamUpdate::Text { text: "Пари".into() }));

        let tool = r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"WebSearch","input":{}}}}"#;
        assert_eq!(parse_line(tool), Line::Update(StreamUpdate::Tool { name: "WebSearch".into() }));

        let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}"#;
        assert_eq!(parse_line(thinking), Line::Ignored);
    }

    #[test]
    fn everything_else_is_ignored() {
        for line in [
            r#"{"type":"rate_limit_event"}"#,
            r#"{"type":"system","subtype":"init","session_id":"x","tools":["Read"]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Paris"}]}}"#,
            r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
            "not json",
            "",
        ] {
            assert_eq!(parse_line(line), Line::Ignored, "{line}");
        }
    }

    #[test]
    fn the_result_ends_the_turn() {
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"Париж","session_id":"x"}"#;
        assert_eq!(parse_line(ok), Line::Result { ok: true, text: "Париж".into() });

        let err = r#"{"type":"result","subtype":"success","is_error":true,"result":"Invalid API key · Please run /login"}"#;
        assert_eq!(parse_line(err), Line::Result { ok: false, text: "Invalid API key · Please run /login".into() });

        let max_turns = r#"{"type":"result","subtype":"error_max_turns","is_error":false}"#;
        assert_eq!(parse_line(max_turns), Line::Result { ok: false, text: String::new() });
    }

    #[test]
    fn signed_out_errors_say_how_to_sign_in() {
        assert!(friendly_error("Invalid API key · Please run /login").contains("claude auth login"));
        assert!(friendly_error("Not logged in").contains("claude auth login"));
        assert_eq!(friendly_error("Overloaded"), "Overloaded");
        assert_eq!(friendly_error("  "), "Claude Code stopped without an answer.");
    }

    #[test]
    fn the_command_line_is_lean_and_resumes_when_asked() {
        let first = args("sonnet", "f2032dcc-af80-4924-ba0b-3231991b3a4d", false);
        let joined = first.join(" ");
        assert!(joined.contains("--strict-mcp-config"));
        // Web search and nothing else: no file reads, no fetching URLs.
        assert!(joined.contains("--tools WebSearch --allowedTools WebSearch "));
        assert!(!joined.contains("Read") && !joined.contains("WebFetch") && !joined.contains("Bash"));
        assert!(joined.contains(r#"{"disableAllHooks":true}"#));
        assert!(joined.ends_with("--session-id f2032dcc-af80-4924-ba0b-3231991b3a4d"));
        assert!(!first.iter().any(|a| a == "--bare"), "--bare would skip the subscription login");
        // "--setting-sources" is followed by an empty value: no user/project settings.
        let i = first.iter().position(|a| a == "--setting-sources").unwrap();
        assert_eq!(first[i + 1], "");

        let again = args("", "f2032dcc-af80-4924-ba0b-3231991b3a4d", true);
        assert!(again.join(" ").ends_with("--resume f2032dcc-af80-4924-ba0b-3231991b3a4d"));
        assert!(!again.iter().any(|a| a == "--model"));
    }

    #[test]
    fn project_folders_are_named_like_claude_code_names_them() {
        assert_eq!(
            project_folder_name(Path::new("/home/dima/.local/share/coucou/chat")),
            "-home-dima--local-share-coucou-chat"
        );
        assert_eq!(project_folder_name(Path::new("/tmp/claude-1000/a_b")), "-tmp-claude-1000-a-b");
    }

    #[test]
    fn session_ids_are_uuids_and_nothing_else() {
        let id = new_session_id();
        assert!(is_session_id(&id), "{id}");
        assert_eq!(&id[14..15], "4");
        assert_ne!(new_session_id(), id);
        assert!(!is_session_id("../../etc/passwd"));
        assert!(!is_session_id("f2032dcc-af80-4924-ba0b-3231991b3a4"));
        assert!(!is_session_id("f2032dcc/af80-4924-ba0b-3231991b3a4d"));
    }

    #[test]
    fn a_transcript_keeps_only_what_was_said() {
        let jsonl = [
            r#"{"type":"queue-operation","operation":"enqueue"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{}},{"type":"text","text":"File: cat.png"},{"type":"text","text":"Что на картинке?"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"thinking","thinking":"…"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"Кот."}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"А какого цвета?"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_2","role":"assistant","content":[{"type":"tool_use","name":"WebSearch","input":{}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"…"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_3","role":"assistant","content":[{"type":"text","text":"Рыжий."}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>"}}"#,
            "garbage",
        ]
        .join("\n");
        let got = parse_transcript(&jsonl);
        let pairs: Vec<(&str, &str)> = got.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect();
        assert_eq!(
            pairs,
            [("user", "Что на картинке?"), ("assistant", "Кот."), ("user", "А какого цвета?"), ("assistant", "Рыжий.")]
        );
    }
}
