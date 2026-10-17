//! Codex uses compatible lifecycle hooks, with a few different tool names.
use serde_json::{Map, Value};

/// Old hook imports had no --agent. turn_id is a documented Codex extension;
/// the rollout filename also identifies session-level events without a turn.
/// An explicit tag/argument always takes precedence over this migration fallback.
pub fn is_codex(payload: &Value) -> bool {
    payload.get("turn_id").is_some_and(Value::is_string)
        || payload.get("transcript_path").and_then(Value::as_str)
            .and_then(|p| std::path::Path::new(p).file_name())
            .is_some_and(|name| name.to_string_lossy().starts_with("rollout-"))
}

pub fn normalize(map: &mut Map<String, Value>) {
    if map.get("hook_event_name").and_then(Value::as_str) == Some("Interrupt") {
        map.insert("hook_event_name".into(), "Stop".into());
        map.insert("message".into(), "Interrupted".into());
    }
    if !map.contains_key("message") {
        if let Some(text) = map.get("last_assistant_message").cloned() {
            map.insert("message".into(), text);
        }
    }
    // Canonical shell hooks already use Bash + command; accept local tool
    // names too, without changing the command shown on an approval card.
    let tool = map.get("tool_name").and_then(Value::as_str).unwrap_or("").to_string();
    if matches!(tool.as_str(), "exec_command" | "shell_command") {
        map.insert("tool_name".into(), "Bash".into());
        if let Some(input) = map.get_mut("tool_input").and_then(Value::as_object_mut) {
            if !input.contains_key("command") {
                if let Some(cmd) = input.get("cmd").cloned() { input.insert("command".into(), cmd); }
            }
        }
    } else if tool == "apply_patch" {
        map.insert("tool_name".into(), "Edit".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognizes_imported_codex_hooks_without_confusing_claude() {
        assert!(is_codex(&json!({"turn_id":"turn-1"})));
        assert!(is_codex(&json!({"transcript_path":"/home/me/.codex/sessions/2026/rollout-123.jsonl"})));
        assert!(!is_codex(&json!({"model":"claude-opus-5", "transcript_path":"/home/me/.claude/projects/project/id.jsonl"})));
        assert!(!is_codex(&json!({"session_id":"id"})));
    }

    #[test]
    fn normalizes_tools_and_interrupt_without_rewriting_commands() {
        let mut v = json!({"tool_name":"exec_command", "tool_input":{"cmd":"echo '$HOME'"}});
        normalize(v.as_object_mut().unwrap());
        assert_eq!(v["tool_name"], "Bash");
        assert_eq!(v["tool_input"]["command"], "echo '$HOME'");
        let mut v = json!({"hook_event_name":"Interrupt"});
        normalize(v.as_object_mut().unwrap());
        assert_eq!(v["hook_event_name"], "Stop");
        assert_eq!(v["message"], "Interrupted");
    }
}
