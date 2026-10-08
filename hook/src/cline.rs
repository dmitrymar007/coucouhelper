//! Cline's hooks, in the shape the island knows: Claude Code's.
//!
//! Cline (the VS Code extension) runs an executable named after the event —
//! `TaskStart`, `PreToolUse`… — from its global hooks folder, with one JSON
//! object on stdin: `hookName`, `taskId`, `workspaceRoots` and one field per
//! event (`preToolUse: { toolName, parameters }` …). Every parameter is a
//! string; lists and objects arrive as JSON text. Coucou's scripts there run
//! this relay with `--agent cline`, and this module turns that object into the
//! Claude Code event the app already handles.
//!
//! Cline's hook can stop a task but never approve a tool, so there is no
//! PermissionRequest here: approvals stay in Cline's panel.

use serde_json::{json, Map, Value};

/// The Claude Code event for a Cline payload, or None for one the island
/// does not show (PreCompact, a payload that is not Cline's).
pub fn translate(cline: &Value) -> Option<Map<String, Value>> {
    let hook = cline.get("hookName").and_then(Value::as_str)?;
    let task = cline.get("taskId").and_then(Value::as_str).filter(|t| !t.is_empty())?;
    let mut out = Map::new();
    out.insert("session_id".into(), json!(task));
    if let Some(root) = cline
        .get("workspaceRoots")
        .and_then(Value::as_array)
        .and_then(|roots| roots.iter().find_map(Value::as_str))
        .filter(|r| !r.is_empty())
    {
        out.insert("cwd".into(), json!(root));
    }
    let event = match hook {
        "TaskStart" | "TaskResume" => "SessionStart",
        "UserPromptSubmit" => {
            let raw = cline.pointer("/userPromptSubmit/prompt").and_then(Value::as_str).unwrap_or("");
            let prompt = clean_prompt(raw);
            if prompt.is_empty() {
                return None;
            }
            out.insert("prompt".into(), json!(prompt));
            "UserPromptSubmit"
        }
        "PreToolUse" | "PostToolUse" => {
            let field = if hook == "PreToolUse" { "preToolUse" } else { "postToolUse" };
            let call = cline.get(field)?;
            let name = call.get("toolName").and_then(Value::as_str).unwrap_or("");
            let params = call.get("parameters").and_then(Value::as_object).cloned().unwrap_or_default();
            let tool = tool(name, &params)?;
            if let Tool::Question(question) = &tool {
                // Cline is waiting for an answer in its panel.
                if hook == "PreToolUse" {
                    out.insert("message".into(), json!(question));
                    out.insert("waiting".into(), json!(true));
                    return finish(out, "Notification");
                }
            }
            let (tool_name, input) = tool.claude();
            out.insert("tool_name".into(), json!(tool_name));
            out.insert("tool_input".into(), Value::Object(input));
            // Protobuf JSON: `success` is written when true and left out when false.
            let failed = hook == "PostToolUse" && call.get("success").and_then(Value::as_bool) != Some(true);
            match (hook, failed) {
                ("PreToolUse", _) => "PreToolUse",
                (_, true) => "PostToolUseFailure",
                _ => "PostToolUse",
            }
        }
        "TaskComplete" => "Stop",
        "TaskCancel" => {
            out.insert("message".into(), json!("Cancelled"));
            "Stop"
        }
        _ => return None,
    };
    finish(out, event)
}

fn finish(mut out: Map<String, Value>, event: &str) -> Option<Map<String, Value>> {
    out.insert("hook_event_name".into(), json!(event));
    Some(out)
}

/// What the user typed, without the markup Cline wraps it in:
/// `<user_input mode="act"><mode_notice>…</mode_notice>text</user_input>`,
/// and the files they attached after it.
pub fn clean_prompt(raw: &str) -> String {
    let mut text = raw.to_string();
    for (open, close) in [("<mode_notice>", "</mode_notice>"), ("<file_content", "</file_content>")] {
        while let Some(start) = text.find(open) {
            let end = text[start..].find(close).map(|e| start + e + close.len()).unwrap_or(text.len());
            text.replace_range(start..end, "");
        }
    }
    if let Some(start) = text.find("<user_input") {
        if let Some(gt) = text[start..].find('>') {
            text.replace_range(start..start + gt + 1, "");
        }
    }
    let text = text.replace("</user_input>", "");
    text.trim().to_string()
}

/// A Cline tool call, by what it does.
enum Tool {
    Named(&'static str, Map<String, Value>),
    Question(String),
    Other(String, Map<String, Value>),
}

impl Tool {
    fn claude(self) -> (String, Map<String, Value>) {
        match self {
            Tool::Named(name, input) => (name.to_string(), input),
            Tool::Other(name, input) => (name, input),
            Tool::Question(question) => ("AskUserQuestion".into(), one("question", question)),
        }
    }
}

fn one(key: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.into(), value.into());
    m
}

/// A parameter as text; lists and objects come JSON-encoded.
fn param(params: &Map<String, Value>, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn parsed(params: &Map<String, Value>, key: &str) -> Option<Value> {
    let text = param(params, key)?;
    Some(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/// The strings of a list parameter: `["a","b"]`, `[{"<field>":"a"}]` or plain text.
fn strings(params: &Map<String, Value>, key: &str, field: &str) -> Vec<String> {
    let item = |v: &Value| -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => {
                let head = o.get(field).and_then(Value::as_str)?.to_string();
                // run_commands' `{ command, args }` form.
                let args: Vec<&str> = o.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                Some(if args.is_empty() { head } else { format!("{head} {}", args.join(" ")) })
            }
            _ => None,
        }
    };
    match parsed(params, key) {
        Some(Value::Array(items)) => items.iter().filter_map(item).filter(|s| !s.trim().is_empty()).collect(),
        Some(v) => item(&v).into_iter().collect(),
        None => Vec::new(),
    }
}

/// `a`, or `a (+2)` when the call works on more than one.
fn first_of(items: &[String]) -> Option<String> {
    let first = items.first()?;
    Some(if items.len() > 1 { format!("{first} (+{})", items.len() - 1) } else { first.clone() })
}

/// The file an apply_patch payload changes first.
fn patched_file(patch: &str) -> Option<String> {
    patch.lines().find_map(|line| {
        ["*** Update File:", "*** Add File:", "*** Delete File:"]
            .iter()
            .find_map(|p| line.trim().strip_prefix(p))
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
    })
}

/// Cline's tool → the Claude Code tool the island knows, with the field it
/// shows. None: a tool that is not a step (the task's own ending).
fn tool(name: &str, p: &Map<String, Value>) -> Option<Tool> {
    use Tool::*;
    let path = |keys: &[&str]| keys.iter().find_map(|k| param(p, k)).map(|v| one("file_path", v)).unwrap_or_default();
    Some(match name {
        // Cline 4 (the SDK tools).
        "run_commands" => {
            let commands = strings(p, "commands", "command");
            let command = if commands.is_empty() { param(p, "command").unwrap_or_default() } else { commands.join("\n") };
            Named("Bash", one("command", command))
        }
        "read_files" => {
            let files = strings(p, "files", "path");
            Named("Read", first_of(&files).map(|f| one("file_path", f)).unwrap_or_default())
        }
        "search_codebase" => Named("Grep", first_of(&strings(p, "queries", "query")).map(|q| one("pattern", q)).unwrap_or_default()),
        "editor" => Named("Edit", path(&["path"])),
        "apply_patch" => Named("Edit", param(p, "input").as_deref().and_then(patched_file).map(|f| one("file_path", f)).unwrap_or_default()),
        "fetch_web_content" => Named("WebFetch", first_of(&strings(p, "requests", "url")).map(|u| one("url", u)).unwrap_or_default()),
        "skills" | "use_skill" => Named("Skill", param(p, "skill").map(|s| one("skill", s)).unwrap_or_default()),
        "spawn_agent" | "use_subagents" | "new_task" => Named("Task", Map::new()),
        "ask_question" | "ask_followup_question" => Question(param(p, "question").unwrap_or_else(|| "Cline has a question".into())),
        "attempt_completion" | "submit_and_exit" | "plan_mode_respond" => return None,
        // Cline 3 and its plain names.
        "execute_command" | "bash" => Named("Bash", param(p, "command").map(|c| one("command", c)).unwrap_or_default()),
        "read_file" | "read" => Named("Read", path(&["path", "file_path", "filePath"])),
        "write_to_file" | "write" => Named("Write", path(&["path", "file_path", "filePath"])),
        "replace_in_file" | "edit" => Named("Edit", path(&["path", "file_path", "filePath"])),
        "list_files" | "list_code_definition_names" => Named("LS", param(p, "path").map(|v| one("path", v)).unwrap_or_default()),
        "search_files" | "grep" => Named("Grep", param(p, "regex").or_else(|| param(p, "pattern")).map(|v| one("pattern", v)).unwrap_or_default()),
        "glob" => Named("Glob", param(p, "pattern").map(|v| one("pattern", v)).unwrap_or_default()),
        "web_fetch" => Named("WebFetch", param(p, "url").map(|v| one("url", v)).unwrap_or_default()),
        "web_search" => Named("WebSearch", param(p, "query").map(|v| one("query", v)).unwrap_or_default()),
        "browser_action" => Named("WebFetch", param(p, "url").map(|v| one("url", v)).unwrap_or_default()),
        "use_mcp_tool" => match (param(p, "server_name"), param(p, "tool_name")) {
            (Some(server), Some(tool)) => Other(format!("mcp__{server}__{tool}"), Map::new()),
            _ => Other("MCP".into(), Map::new()),
        },
        "access_mcp_resource" => Other("MCP".into(), param(p, "uri").map(|v| one("url", v)).unwrap_or_default()),
        other => Other(other.to_string(), Map::new()),
    })
}

// ── What the task has used ────────────────────────────────────────────────────

/// Cline's session store: `$CLINE_SESSION_DATA_DIR`, else `<data>/sessions`
/// with `<data>` = `$CLINE_DATA_DIR`, `$CLINE_DIR/data` or `~/.cline/data`.
fn sessions_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    if let Some(dir) = var("CLINE_SESSION_DATA_DIR") {
        return Some(dir);
    }
    let data = var("CLINE_DATA_DIR")
        .or_else(|| var("CLINE_DIR").map(|d| d.join("data")))
        .or_else(|| var("HOME").map(|h| h.join(".cline").join("data")))?;
    Some(data.join("sessions"))
}

/// Longest messages file read; beyond it, no figures.
const MAX_MESSAGES: u64 = 64 * 1024 * 1024;

/// On Stop: the task's cost so far, the context of its last answer, and what
/// the turn that just ended read and wrote — from Cline's own session files,
/// which never leave this process.
pub fn spent(task: &str) -> Map<String, Value> {
    let mut out = Map::new();
    // A task id is Cline's, but it becomes a path: nothing that could climb out.
    if task.is_empty() || !task.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return out;
    }
    let Some(dir) = sessions_dir().map(|d| d.join(task)) else { return out };
    let read = |name: String| -> Option<Value> {
        let path = dir.join(name);
        if std::fs::metadata(&path).ok()?.len() > MAX_MESSAGES {
            return None;
        }
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    };
    if let Some(cost) = read(format!("{task}.json")).and_then(|s| s.pointer("/metadata/totalCost").and_then(Value::as_f64)) {
        out.insert("cost".into(), json!(cost));
    }
    if let Some(messages) = read(format!("{task}.messages.json")) {
        out.extend(usage(&messages));
    }
    out
}

/// `tokens`, `turn_tokens_in` and `turn_tokens_out` from a messages file:
/// each answer carries its own `metrics`, whose input already counts the
/// prompt cache it read.
fn usage(messages: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(list) = messages.get("messages").and_then(Value::as_array) else { return out };
    let n = |m: &Value, k: &str| m.pointer(&format!("/metrics/{k}")).and_then(Value::as_u64).unwrap_or(0);
    if let Some(last) = list.iter().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("assistant") && m.get("metrics").is_some()) {
        let context = n(last, "inputTokens") + n(last, "cacheWriteTokens");
        if context > 0 {
            out.insert("tokens".into(), json!(context));
        }
    }
    let (mut read, mut wrote, mut calls) = (0u64, 0u64, 0);
    for m in list.iter().rev() {
        match m.get("role").and_then(Value::as_str) {
            Some("user") if is_prompt(m) => break,
            Some("assistant") if m.get("metrics").is_some() => {
                read += n(m, "inputTokens") + n(m, "cacheWriteTokens");
                wrote += n(m, "outputTokens");
                calls += 1;
            }
            _ => {}
        }
    }
    if calls > 0 {
        out.insert("turn_tokens_in".into(), json!(read));
        out.insert("turn_tokens_out".into(), json!(wrote));
    }
    out
}

/// A message the user wrote, not a tool's result.
fn is_prompt(m: &Value) -> bool {
    m.get("content").and_then(Value::as_array).is_some_and(|blocks| {
        blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            && !blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(v: Value) -> Map<String, Value> {
        translate(&v).expect("a Cline event")
    }

    #[test]
    fn a_tool_call_reads_like_claude_codes() {
        let out = event(json!({
            "clineVersion": "4.1.22", "hookName": "PreToolUse", "taskId": "1791_ab",
            "workspaceRoots": ["/home/u/proj"],
            "preToolUse": { "toolName": "run_commands", "parameters": { "commands": "[\"ls -la\",\"make\"]" } }
        }));
        assert_eq!(out["hook_event_name"], "PreToolUse");
        assert_eq!(out["session_id"], "1791_ab");
        assert_eq!(out["cwd"], "/home/u/proj");
        assert_eq!(out["tool_name"], "Bash");
        assert_eq!(out["tool_input"]["command"], "ls -la\nmake");

        let read = event(json!({ "hookName": "PreToolUse", "taskId": "t",
            "preToolUse": { "toolName": "read_files", "parameters": { "files": "[{\"path\":\"/a/b.rs\"},{\"path\":\"/a/c.rs\"}]" } } }));
        assert_eq!(read["tool_name"], "Read");
        assert_eq!(read["tool_input"]["file_path"], "/a/b.rs (+1)");

        let patch = event(json!({ "hookName": "PreToolUse", "taskId": "t",
            "preToolUse": { "toolName": "apply_patch", "parameters": { "input": "*** Begin Patch\n*** Update File: src/x.ts\n@@\n-a\n+b\n*** End Patch" } } }));
        assert_eq!(patch["tool_name"], "Edit");
        assert_eq!(patch["tool_input"]["file_path"], "src/x.ts");

        let edit = event(json!({ "hookName": "PreToolUse", "taskId": "t",
            "preToolUse": { "toolName": "editor", "parameters": { "path": "/p/a.c", "new_text": "x" } } }));
        assert_eq!(edit["tool_input"]["file_path"], "/p/a.c");
    }

    #[test]
    fn a_failed_tool_and_the_tasks_end() {
        let failed = event(json!({ "hookName": "PostToolUse", "taskId": "t",
            "postToolUse": { "toolName": "editor", "parameters": { "path": "/p/a.c" }, "result": "no such file" } }));
        assert_eq!(failed["hook_event_name"], "PostToolUseFailure");
        let ok = event(json!({ "hookName": "PostToolUse", "taskId": "t",
            "postToolUse": { "toolName": "editor", "parameters": { "path": "/p/a.c" }, "success": true } }));
        assert_eq!(ok["hook_event_name"], "PostToolUse");
        assert_eq!(event(json!({ "hookName": "TaskComplete", "taskId": "t" }))["hook_event_name"], "Stop");
        assert_eq!(event(json!({ "hookName": "TaskCancel", "taskId": "t" }))["message"], "Cancelled");
        assert_eq!(event(json!({ "hookName": "TaskResume", "taskId": "t" }))["hook_event_name"], "SessionStart");
        // The end of a task is not a step of it, and neither is compaction.
        assert!(translate(&json!({ "hookName": "PreToolUse", "taskId": "t",
            "preToolUse": { "toolName": "attempt_completion", "parameters": {} } })).is_none());
        assert!(translate(&json!({ "hookName": "PreCompact", "taskId": "t" })).is_none());
        // Not Cline's: nothing.
        assert!(translate(&json!({ "hook_event_name": "Stop", "session_id": "x" })).is_none());
    }

    #[test]
    fn a_question_says_cline_is_waiting() {
        let out = event(json!({ "hookName": "PreToolUse", "taskId": "t",
            "preToolUse": { "toolName": "ask_question", "parameters": { "question": "Which one", "options": "[\"a\",\"b\"]" } } }));
        assert_eq!(out["hook_event_name"], "Notification");
        assert_eq!(out["message"], "Which one");
        assert_eq!(out["waiting"], true);
    }

    #[test]
    fn the_prompt_loses_clines_markup() {
        assert_eq!(clean_prompt("<user_input mode=\"act\">объясни алгоритм</user_input>"), "объясни алгоритм");
        assert_eq!(
            clean_prompt("<user_input mode=\"plan\"><mode_notice>The user switched.</mode_notice>\nкратко</user_input>"),
            "кратко"
        );
        assert_eq!(
            clean_prompt("<user_input mode=\"act\">'a.pdf' (see below)  read it?\n\n<file_content path=\"a.pdf\">lots</file_content></user_input>"),
            "'a.pdf' (see below)  read it?"
        );
        assert_eq!(clean_prompt("plain"), "plain");
        assert!(translate(&json!({ "hookName": "UserPromptSubmit", "taskId": "t", "userPromptSubmit": {} })).is_none());
    }

    #[test]
    fn the_turn_counts_its_answers_since_the_prompt() {
        let messages = json!({ "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "first" }] },
            { "role": "assistant", "content": [], "metrics": { "inputTokens": 900, "outputTokens": 9 } },
            { "role": "user", "content": [{ "type": "text", "text": "second" }] },
            { "role": "assistant", "content": [], "metrics": { "inputTokens": 1000, "outputTokens": 20, "cacheReadTokens": 800 } },
            { "role": "user", "content": [{ "type": "tool_result" }] },
            { "role": "assistant", "content": [], "metrics": { "inputTokens": 1200, "outputTokens": 30, "cacheWriteTokens": 5 } }
        ]});
        let out = usage(&messages);
        assert_eq!(out["tokens"], 1205);
        assert_eq!(out["turn_tokens_in"], 2205);
        assert_eq!(out["turn_tokens_out"], 50);
    }

    #[test]
    fn a_task_id_never_becomes_another_path() {
        assert!(spent("../../etc").is_empty());
        assert!(spent("").is_empty());
    }
}
