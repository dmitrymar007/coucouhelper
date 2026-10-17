# Coucou — third-party agent integration

Any tool that can write to a Unix domain socket can send events to Coucou and have its own pill next to Claude Code.

## The `coucou_agent` field

Add the optional field `coucou_agent` to any hook JSON payload. Coucou will create a pill labelled with the agent name and route all events to it.

**Validation:** the name must match `^[a-z0-9-]{1,24}$` (lowercase letters, digits and hyphens, 1–24 characters). An absent or invalid name routes the event to the Claude Code pill instead.

## Hook command

Configure your tool to call the Coucou relay with `--agent <your-name>`. Coucou copies the relay to `~/.local/share/coucou/bin/coucou-hook` at startup.

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "type": "command", "command": "/path/to/coucou-hook --agent my-tool" }
    ]
  }
}
```

## Payload format

The relay adds `coucou_agent` to the JSON it forwards. You can also add it yourself if you talk to the socket directly:

```json
{
  "hook_event_name": "UserPromptSubmit",
  "session_id": "my-session-1",
  "coucou_agent": "my-tool",
  "prompt": "Running task…"
}
```

Send newline-terminated JSON to the socket `$XDG_RUNTIME_DIR/coucou.sock` (usually `/run/user/<uid>/coucou.sock`). Only your own user account can connect.

## Supported events

All standard Claude Code hook events are supported, **except `PermissionRequest`**:
approval cards are not yet implemented for most third-party agents. A
`PermissionRequest` from an external agent is answered immediately with no
decision, so the relay writes nothing and the agent re-asks in its terminal.

**Exception: Codex.** Settings → Codex → Install hooks previews and backs up
`~/.codex/hooks.json` (or `$CODEX_HOME/hooks.json`), then merges commands with
`--agent codex`. Review and trust the changed hooks in Codex `/hooks`. Codex
has its own persistent `agent_codex` pill and supports Allow/Deny through the
compatible `PermissionRequest` decision format. With hooks alone, permission
prompts run before the native UI and questions remain in Codex. For parity with
Claude Code's two interfaces, enable **Codex · VS Code bridge** in Settings:

- `src-tauri/src/codex_vscode.rs` previews/backups a single VS Code setting,
  `chatgpt.cliExecutable`, preserving JSONC. It points at the launcher
  `~/.local/share/coucou/bin/coucou-codex`. The official extension is not patched.
- The launcher runs `coucou-hook --codex-app-server`; `hook/src/codex_proxy.rs`
  forwards App Server stdio unchanged and mirrors native command/file/permission
  approval requests and `item/tool/requestUserInput` to the Coucou socket.
- Native requests are forwarded to VS Code before connecting to Coucou. The
  first response is sent once; late competing responses are ignored. Closing
  the socket removes the other island card through `approval-gone`; App Server's
  `serverRequest/resolved` clears the native prompt after an island answer.
- `hook/src/codex_async.rs` also supports async `agentMessage.questions` on
  `item/started` / `item/completed`. These are not server requests: choices are
  answered through `turn/start` with the official UI's structured
  `<send_user_message_question_reply>` user input. Question IDs are the JSON
  tuple `["request_user_input_async", itemId, index]`. Bridge-owned RPC responses
  are consumed locally; competing native submissions get the real upstream
  acknowledgement without sending the answer twice. Native userMessage
  notifications cancel cards even when another client answered.
- Async cards additionally carry `coucou_async: true`: they survive Stop and
  are cancelled by the bridge's socket closure or the existing card timeout.
- Questions carry `coucou_transport: "codex-app-server"`; only this transport
  enables Codex's `AskUserQuestion` cards. The bridge translates question text
  back to question IDs and validates selected labels against original options.
  Secret/free-text/unsupported prompts remain in VS Code. Choice prompts with
  free-text alternatives offer **In VS Code** for those answers.
- The child inherits `COUCOU_CODEX_BRIDGE=1`. Only Coucou's own Codex
  `PermissionRequest` hook yields; lifecycle hooks, other policy hooks, the
  sandbox and approval reviewers are unchanged. No inference API or new
  network listener is introduced.

The relay also recognizes old unlabelled imports by Codex's `turn_id` or rollout
transcript filename. `Interrupt` finishes the turn; Codex transcripts are not
parsed as Claude usage.

**Exception: opencode.** The Coucou plugin for opencode
(`opencode-plugin/coucou.js`, installed from Settings → opencode) sends
opencode's `permission.asked` as a `PermissionRequest` with `--agent opencode`,
turns the relay's answer into opencode's own reply (`once` or `reject`), and
kills the relay when the question is answered in opencode first. On the app
side, a relay that closes its connection while a card is up takes the card
down (`approval-gone`), for Claude Code too.

The pill lifecycle:

| Event | Effect |
|---|---|
| `SessionStart` | Creates the pill (if absent), sets state to idle |
| `UserPromptSubmit` | State → thinking; prompt shown in ticker |
| `PreToolUse` | State → working; tool label shown in ticker |
| `PostToolUse` / `PostToolUseFailure` | State → working |
| `Notification` | Rate-limit or question state if applicable |
| `Stop` | State → finished for 5 s; active declared pills (catalog + checked in Settings) reset to idle — all others are removed |
| `StopFailure` | State → error |
| `SessionEnd` | Active declared pills (catalog + checked in Settings) reset to idle — all others are removed |
| `SubagentStart` / `SubagentStop` | Step added to ticker |

## Real-world examples

### opencode

Settings → opencode → Install plugin writes `~/.config/opencode/plugin/coucou.js`, which relays opencode's events with `--agent opencode` (see the exception above).

### Cline

Cline (the VS Code extension) runs, for each event, an executable named after it from its global hooks folder `<Documents>/Cline/Hooks/` (`xdg-user-dir DOCUMENTS`), with its own JSON on stdin (`hookName`, `taskId`, `workspaceRoots`, `preToolUse: { toolName, parameters }`…). Settings → Cline → Install hooks writes one `sh` script per event there — `TaskStart`, `TaskResume`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `TaskComplete`, `TaskCancel` — each running `coucou-hook --agent cline` and answering `{"cancel":false}`. With `--agent cline` the relay translates (`hook/src/cline.rs`):

| Cline | Island |
|---|---|
| `TaskStart`, `TaskResume` | `SessionStart` |
| `UserPromptSubmit` | `UserPromptSubmit`, the prompt without Cline's `<user_input>` markup |
| `PreToolUse` / `PostToolUse` | the same, tools under Claude Code's names (`run_commands` → Bash, `read_files` → Read, `editor`/`apply_patch` → Edit…); a failed tool → `PostToolUseFailure` |
| `PreToolUse` of `ask_question` | `Notification` with `waiting: true`: the pill shows the question |
| `TaskComplete` / `TaskCancel` | `Stop`, with cost, context and turn tokens from `~/.cline/data/sessions/<taskId>/` |

A Cline hook can cancel a task but not approve a tool, so there is no `PermissionRequest` for Cline: approvals stay in its panel.

### Any other tool

Follow the generic pattern: call `~/.local/share/coucou/bin/coucou-hook --agent <your-name> <EventName>` and let the relay forward the event.

## Quick test

With Coucou running:

```sh
echo '{"hook_event_name":"UserPromptSubmit","session_id":"t1","prompt":"hello","coucou_agent":"demo"}' \
  | ~/.local/share/coucou/bin/coucou-hook --agent demo
```

A "demo" pill should appear in the island.
