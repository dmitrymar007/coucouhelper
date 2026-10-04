// Chat through opencode (`opencode run`) and whichever providers the user has
// set up in it: OpenCode Zen, OpenRouter, any OpenAI-compatible endpoint…
//
// One `opencode run` per turn: the message goes in on stdin, the answer comes
// back as JSON events on stdout, and `--session` carries the conversation on.
// opencode saves every session in its own database, so the history list and
// "Continue" work from there.
//
// It runs as harmless as the Claude Code chat: its own agent, defined through
// OPENCODE_CONFIG_CONTENT, with web search and no other tool — no file access,
// no edits, no commands, no MCP tools — no external plugins (`--pure`), no
// project config, nothing from ~/.claude. The user's own opencode config is
// read for providers and models, never written.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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

/// One stdout line, reduced to what matters here.
#[derive(Debug, PartialEq)]
pub enum Event {
    Text(String),
    Tool(String),
    Error(String),
    Ignored,
}

pub fn parse_event(line: &str) -> (Event, Option<String>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return (Event::Ignored, None) };
    let session = v["sessionID"].as_str().filter(|s| is_session_id(s)).map(str::to_string);
    let event = match v["type"].as_str() {
        Some("text") => match v["part"]["text"].as_str().map(str::trim) {
            Some(t) if !t.is_empty() => Event::Text(t.to_string()),
            _ => Event::Ignored,
        },
        Some("tool_use") => Event::Tool(v["part"]["tool"].as_str().unwrap_or("tool").to_string()),
        Some("error") => {
            let e = &v["error"];
            let message = e["data"]["message"].as_str().or(e["message"].as_str()).or(e["name"].as_str());
            Event::Error(message.unwrap_or("opencode could not answer.").to_string())
        }
        _ => Event::Ignored,
    };
    (event, session)
}

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
/// as an attachment (`--file`); its name and the window go in the text.
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

/// Command line of one turn. Values go after `=`, so none can pass for a flag.
pub fn args(model: &str, session: Option<&str>, title: &str, file: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> =
        ["run", "--pure", "--format", "json", "--agent", AGENT].iter().map(|s| s.to_string()).collect();
    if !model.is_empty() {
        a.push(format!("--model={model}"));
    }
    match session {
        Some(id) => a.push(format!("--session={id}")),
        None => a.push(format!("--title={title}")),
    }
    if let Some(file) = file {
        a.push(format!("--file={file}"));
    }
    a
}

#[derive(Default)]
pub struct OpenCodeChat {
    /// Held for a whole turn: one turn at a time.
    turn: tokio::sync::Mutex<()>,
    /// The turn's process, so it can be stopped midway.
    child: Mutex<Option<Child>>,
    session: Mutex<Option<String>>,
}

impl OpenCodeChat {
    pub fn status(&self) -> claude_cli::CliStatus {
        let running = self
            .child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)));
        claude_cli::CliStatus { running, session_id: self.session.lock().unwrap().clone() }
    }

    /// Stops the answer being written, if any. The conversation goes on with
    /// the next message.
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.start_kill();
                crate::log::line("chat: opencode stopped");
            }
        }
    }

    pub fn reset(&self) {
        self.stop();
        *self.session.lock().unwrap() = None;
    }

    /// Continue session `id` with the next message; returns what was said so
    /// far, for the island to show.
    pub async fn resume(&self, id: &str) -> Result<Vec<TranscriptMessage>, String> {
        let messages = transcript(id).await?;
        self.stop();
        *self.session.lock().unwrap() = Some(id.to_string());
        Ok(messages)
    }

    /// One chat turn. `on_update` gets each piece of the answer as it is
    /// written, and the tools it uses.
    pub async fn send(
        &self,
        model: &str,
        query: String,
        context: Option<ChatContext>,
        on_update: impl Fn(StreamUpdate),
    ) -> Result<(ChatReply, String), String> {
        let _turn = self.turn.lock().await;
        let exe = find_opencode().ok_or(
            "opencode is not installed: `opencode` is nowhere on PATH. Install it, or pick another chat backend in Settings.",
        )?;
        platform::ensure_private_dir(&claude_cli::work_dir()).map_err(|e| format!("Chat folder: {e}"))?;

        let session = self.session.lock().unwrap().clone();
        let (message, file) = if session.is_none() {
            first_message(&query, context.as_ref())
        } else {
            (query.clone(), None)
        };
        let title: String = query.lines().next().unwrap_or("").chars().take(80).collect();

        let mut cmd = command(&exe);
        cmd.args(args(model, session.as_deref(), &title, file.as_deref())).stdin(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("Could not start opencode: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("no stderr")?;
        *self.child.lock().unwrap() = Some(child);
        crate::log::line(if session.is_some() { "chat: opencode turn (continuing)" } else { "chat: opencode turn" });

        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = stderr.clone();
        tauri::async_runtime::spawn(async move {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf).await;
            let text = String::from_utf8_lossy(&buf);
            let tail: String = text.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
            *sink.lock().unwrap() = tail;
        });

        stdin.write_all(message.as_bytes()).await.map_err(|e| format!("opencode: {e}"))?;
        drop(stdin);

        let mut lines = BufReader::new(stdout).lines();
        let mut answer = String::new();
        let mut failure: Option<String> = None;
        let mut seen_session = session.clone();
        loop {
            let next = tokio::time::timeout(SILENCE_LIMIT, lines.next_line()).await;
            let line = match next {
                Err(_) => {
                    self.stop();
                    failure = Some("opencode stopped answering.".into());
                    break;
                }
                Ok(Ok(Some(line))) => line,
                Ok(_) => break,
            };
            let (event, id) = parse_event(&line);
            if seen_session.is_none() {
                seen_session = id;
            }
            match event {
                Event::Text(text) => {
                    let piece = if answer.is_empty() { text.clone() } else { format!("\n\n{text}") };
                    answer.push_str(&piece);
                    on_update(StreamUpdate::Text { text: piece });
                }
                Event::Tool(name) => on_update(StreamUpdate::Tool { name }),
                Event::Error(message) => failure = Some(message),
                Event::Ignored => {}
            }
        }

        let child = self.child.lock().unwrap().take();
        let exited_ok = match child {
            Some(mut c) => c.wait().await.map(|s| s.success()).unwrap_or(false),
            // Taken by stop(): the user stopped it.
            None => false,
        };
        if let Some(id) = &seen_session {
            *self.session.lock().unwrap() = Some(id.clone());
        }
        let session_id = seen_session.unwrap_or_default();

        if failure.is_none() && !answer.is_empty() {
            return Ok((ChatReply { text: answer }, session_id));
        }
        // Give stderr a moment to be read to its end.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let raw = failure.unwrap_or_else(|| {
            let tail = stderr.lock().unwrap().clone();
            if exited_ok && tail.trim().is_empty() { "No response text.".into() } else { tail }
        });
        crate::log::line(format!("chat: opencode failed: {}", raw.chars().take(160).collect::<String>()));
        Err(friendly_error(&raw))
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
            let (reply, id) = chat
                .send(&model, "Remember the number 42. Answer with one word: ok.".into(), None, |_| {})
                .await
                .expect("first turn");
            assert!(!reply.text.is_empty());
            assert!(is_session_id(&id), "{id}");

            let (reply, again) = chat.send(&model, "Which number? Digits only.".into(), None, |_| {}).await.expect("second turn");
            assert_eq!(again, id);
            assert!(reply.text.contains("42"), "{}", reply.text);

            let listed = list_sessions(12).await;
            assert!(listed.iter().any(|s| s.id == id), "{id} not in {listed:?}");
            let fresh = OpenCodeChat::default();
            let shown = fresh.resume(&id).await.expect("transcript");
            assert_eq!(shown.len(), 4, "{shown:?}");
            assert_eq!(shown[0].content, "Remember the number 42. Answer with one word: ok.");
        });
    }

    #[test]
    fn session_ids_are_opencodes_and_nothing_else() {
        assert!(is_session_id("ses_ef92c1b5bffeIrOHp90IEeCzYH"));
        assert!(!is_session_id("ses_../../etc/passwdxxxxxxxxxxx"));
        assert!(!is_session_id("--session=ses_ef92c1b5bffeIrOH"));
        assert!(!is_session_id("f2032dcc-af80-4924-ba0b-3231991b3a4d"));
    }

    #[test]
    fn events_become_text_tools_and_errors() {
        let text = r#"{"type":"text","sessionID":"ses_ef92c1b5bffeIrOHp90IEeCzYH","part":{"type":"text","text":"Привет\n"}}"#;
        assert_eq!(parse_event(text), (Event::Text("Привет".into()), Some("ses_ef92c1b5bffeIrOHp90IEeCzYH".into())));

        let tool = r#"{"type":"tool_use","sessionID":"ses_ef92c1b5bffeIrOHp90IEeCzYH","part":{"type":"tool","tool":"websearch","state":{"status":"completed"}}}"#;
        assert_eq!(parse_event(tool).0, Event::Tool("websearch".into()));

        let error = r#"{"type":"error","sessionID":"ses_ef92c9d4fffeqAm58L1ETClkdT","error":{"name":"APIError","data":{"message":"OpenCode's free tier can only be used from within OpenCode","statusCode":403}}}"#;
        assert_eq!(parse_event(error).0, Event::Error("OpenCode's free tier can only be used from within OpenCode".into()));

        for line in [r#"{"type":"step_start","part":{}}"#, r#"{"type":"step_finish"}"#, "not json", ""] {
            assert_eq!(parse_event(line).0, Event::Ignored, "{line}");
        }
    }

    #[test]
    fn the_command_line_keeps_values_out_of_flags() {
        let first = args("anymodel/am/kimi-k3", None, "-rf what", Some("/home/u/.local/share/coucou/inbox/a.txt"));
        assert_eq!(
            first,
            ["run", "--pure", "--format", "json", "--agent", "coucou", "--model=anymodel/am/kimi-k3", "--title=-rf what",
             "--file=/home/u/.local/share/coucou/inbox/a.txt"]
        );
        let again = args("", Some("ses_ef92c1b5bffeIrOHp90IEeCzYH"), "ignored", None);
        assert_eq!(again.last().unwrap(), "--session=ses_ef92c1b5bffeIrOHp90IEeCzYH");
        assert!(!again.iter().any(|a| a.starts_with("--model") || a.starts_with("--title")));
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
