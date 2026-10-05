// Claude Code hook events → island state.
// Port of HookServer.processEvent / processPermissionRequest from the macOS app.
// Difference from macOS: no terminal filter. On Windows the hook fires from any
// terminal (Windows Terminal, VS Code, PowerShell…) and all of them are handled.

import { Bridge, onEvent } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { Island } from "./island";

const CLAUDE_ID = "integration_claude";

/**
 * External agents whose relay turns the island's answer into their own: the
 * Coucou plugin for opencode replies to opencode's permission request. Any
 * other agent's request goes back to its terminal, since a Claude Code-shaped
 * answer would mean nothing to it.
 */
const APPROVAL_AGENTS: ReadonlySet<string> = new Set(["agent_opencode"]);

/**
 * Agents Coucou integrates itself (opencode, through its plugin): their pill
 * has a fixed name and colour, shows from launch while the integration is
 * installed, and goes back to idle after a session like Claude Code's —
 * instead of appearing and vanishing with every answer like an unknown agent.
 */
const DECLARED_AGENTS: ReadonlyMap<string, { name: string; color: string }> = new Map([
  ["agent_opencode", { name: "opencode", color: "#FAB283" }],
]);

/** Puts the opencode pill up when its plugin is installed. */
export async function showDeclaredAgents() {
  const plugin = await Bridge.opencodePluginStatus();
  const opencode = DECLARED_AGENTS.get("agent_opencode")!;
  if (plugin?.installed) State.upsertExternalAgent("agent_opencode", opencode.name, opencode.color);
}

/** Clears the approval card if no decision was made before the hook gave up. */
let pendingTimeout: number | null = null;

interface HookPayload {
  hook_event_name?: string;
  request_id?: string;
  session_id?: string;
  cwd?: string;
  message?: string;
  /** UserPromptSubmit carries `prompt`; `message` belongs to Notification/Stop. */
  prompt?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
  /** Optional agent tag: lowercase, digits and hyphens, ≤ 24 chars. */
  coucou_agent?: string;
  /** Linux relay: the session's ancestor processes, nearest first. */
  ancestor_pids?: unknown;
}

/** Plain process ids only, and not too many: "Open terminal" walks them. */
function ancestorPids(value: unknown): number[] {
  if (!Array.isArray(value)) return [];
  return value.filter((n): n is number => Number.isInteger(n) && n > 1).slice(0, 12);
}

/** Same rule as HookServer.validateAgent on macOS. "claude" is reserved. */
function validateAgent(raw: string | undefined): string | null {
  if (!raw || raw.length > 24 || raw === "claude") return null;
  if (!/^[a-z0-9-]+$/.test(raw)) return null;
  return raw;
}

const FALLBACK_COLORS = ["#22C55E", "#EAB308", "#60A5FA", "#E879F9"];

function agentColor(name: string): string {
  let h = 0;
  for (let i = 0; i < name.length; i++) {
    h = (Math.imul(31, h) + name.charCodeAt(i)) | 0;
  }
  return FALLBACK_COLORS[Math.abs(h) % FALLBACK_COLORS.length];
}

const PROJECT_ALIASES: Record<string, string> = {
  "notch-buddy": "Notch Buddy",
  notchbuddy: "Notch Buddy",
  notch_buddy: "Notch Buddy",
};

function aliasProjectName(name: string): string {
  return PROJECT_ALIASES[name.toLowerCase()] ?? name;
}

function lastPathComponent(p: string): string {
  const cleaned = p.replace(/[\\/]+$/, "");
  const idx = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return idx >= 0 ? cleaned.slice(idx + 1) : cleaned;
}

/**
 * What each tool is doing, in plain words. The Mac app says it in French
 * (frenchStep()); this build speaks English everywhere else, so the ticker
 * does too. opencode's tools arrive under these names through its plugin.
 */
const TOOL_LABELS: Record<string, string> = {
  Bash: "Run",
  PowerShell: "Run",
  Read: "Read",
  Write: "Write",
  Edit: "Edit",
  MultiEdit: "Edit",
  NotebookEdit: "Edit notebook",
  Glob: "Find files",
  Grep: "Search code",
  LS: "List",
  WebSearch: "Search the web",
  WebFetch: "Open page",
  TodoWrite: "Update to-do list",
  Task: "Subagent",
  Agent: "Subagent",
  Skill: "Skill",
};

/** Fields that say what a tool works on, most telling first. */
const STEP_FIELDS = ["command", "file_path", "path", "pattern", "query", "url", "description", "skill"] as const;

function stepLabel(tool: string, input: Record<string, unknown>): string {
  // MCP tools: "mcp__server__tool" reads better as "server · tool".
  const mcp = /^mcp__(.+?)__(.+)$/.exec(tool);
  const label = mcp ? `${mcp[1]} · ${mcp[2].replace(/_/g, " ")}` : (TOOL_LABELS[tool] ?? tool);
  for (const field of STEP_FIELDS) {
    const value = input[field];
    if (typeof value !== "string" || !value.trim()) continue;
    const text = value.trim().split("\n")[0];
    const shown = field === "file_path" || field === "path" ? lastPathComponent(text) : text;
    return `${label} · ${shown.slice(0, 48)}`;
  }
  return label;
}

/**
 * What the Allow button actually authorises. Approving "Write" tells you nothing
 * — approving `Write · C:\…\.env` tells you everything, and the difference is
 * the whole point of approving from the island rather than blind.
 *
 * Ordered by how specific the field is, so an unfamiliar tool still shows
 * whatever identifying string it carries instead of falling back to its name.
 */
const APPROVAL_FIELDS = [
  "command", // Bash, PowerShell
  "file_path", // Write, Edit, MultiEdit, NotebookEdit
  "path", // Read, LS
  "url", // WebFetch
  "query", // WebSearch
  "pattern", // Glob, Grep
  "prompt", // Task
] as const;

function approvalTarget(tool: string, input: Record<string, unknown>): string {
  for (const field of APPROVAL_FIELDS) {
    const value = input[field];
    if (typeof value === "string" && value.trim()) {
      return `${tool} · ${value.trim()}`;
    }
  }
  return tool;
}

function upsert(projectName: string, cwd: string, pids: number[]) {
  const t = State.tasks.find((x) => x.id === CLAUDE_ID);
  if (!t) return;
  t.name = projectName;
  if (cwd) t.sessionCwd = cwd;
  if (pids.length) t.sessionPids = pids;
}

function clearSession() {
  const t = State.tasks.find((x) => x.id === CLAUDE_ID);
  if (!t) return;
  t.steps = [];
  t.stepIndex = 0;
  t.name = "Claude Code";
  t.pillBadge = null;
}

export function registerHookHandlers(island: Island) {
  void onEvent<HookPayload>("hook", (payload) => handleHook(island, payload));
  // The relay went away while the card was up: the question was answered in
  // the terminal (or the agent quit), so the card has nothing left to decide.
  void onEvent<string>("approval-gone", (requestId) => {
    if (State.pendingApproval?.requestId !== requestId) return;
    clearApproval(island, State.pendingApproval.agentId);
  });
}

/** Takes the approval card down and hands the island back. */
function clearApproval(island: Island, agentId: string) {
  if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
  pendingTimeout = null;
  State.pendingApproval = null;
  State.isPinned = false;
  island.dropPin();
  State.updateTask(agentId, "working");
  State.setPillBadge(agentId, null);
  if (State.view === "approval") island.setView(State.defaultView());
  State.notify();
}

function handleHook(island: Island, payload: HookPayload) {
  if (State.paused) {
    // Silence here used to cost Claude Code nearly two minutes: the relay waited
    // for a decision from an island that had already decided not to look. Say so,
    // and the terminal takes the question immediately.
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    return;
  }

  const name = payload.hook_event_name ?? "";
  const cwd = payload.cwd ?? "";
  const pids = ancestorPids(payload.ancestor_pids);
  const raw = lastPathComponent(cwd);
  const projectName = aliasProjectName(raw || "Session");

  // Route to the right pill. Valid coucou_agent → dynamic "agent_<name>" pill.
  // "claude" is reserved; absent or invalid → Claude Code pill unchanged.
  const validAgent = validateAgent(payload.coucou_agent);
  const agentId = validAgent ? `agent_${validAgent}` : CLAUDE_ID;
  const isExternalAgent = validAgent !== null;
  const declared = DECLARED_AGENTS.get(agentId);
  /** Unknown agents' pills come and go with their sessions; declared ones stay. */
  const transient = isExternalAgent && !declared;

  const focused = State.focusId === agentId;

  /** Alerts force the island open; work events only reveal the compact island. */
  const surface = (view: Parameters<Island["alert"]>[0], isAlert: boolean) => {
    if (State.mode === "expanded") {
      if (isAlert) island.setView(view);
    } else if (isAlert) {
      island.alert(view);
    } else if (State.mode === "hidden") {
      island.reveal();
    }
  };

  /** Ensure the agent pill exists (no-op for Claude Code). */
  const ensurePill = () => {
    if (isExternalAgent) {
      State.upsertExternalAgent(agentId, declared?.name ?? validAgent!, declared?.color ?? agentColor(validAgent!));
      // "Open terminal" brings back the agent's window, as for Claude Code.
      const t = State.tasks.find((x) => x.id === agentId);
      if (t && cwd) t.sessionCwd = cwd;
      if (t && pids.length) t.sessionPids = pids;
    } else {
      upsert(projectName, cwd, pids);
    }
  };

  switch (name) {
    case "SessionStart":
      ensurePill();
      surface("overview", false);
      Sound.play("work");
      break;

    case "UserPromptSubmit": {
      ensurePill();
      State.updateTask(agentId, "thinking");
      // The field is `prompt`; reading `message` meant this step was always blank.
      const asked = payload.prompt ?? payload.message;
      if (asked) State.appendStep(agentId, asked.slice(0, 60));
      surface("overview", false);
      break;
    }

    case "PreToolUse": {
      ensurePill();
      State.updateTask(agentId, "working");
      const tool = payload.tool_name ?? "Tool";
      State.appendStep(agentId, stepLabel(tool, payload.tool_input ?? {}));
      surface("overview", false);
      break;
    }

    case "PostToolUse":
      State.updateTask(agentId, "working");
      break;

    case "PostToolUseFailure":
      State.updateTask(agentId, "working");
      State.appendStep(agentId, "⚠ Tool failed");
      break;

    case "Notification": {
      const message = payload.message ?? "";
      const lower = message.toLowerCase();
      if (lower.includes("rate limit") || lower.includes("limite d")) {
        State.updateTask(agentId, "ratelimit");
        Sound.play("rate");
      } else if (message.endsWith("?")) {
        State.updateTask(agentId, "question");
        State.appendStep(agentId, message);
      }
      break;
    }

    case "Stop":
      State.updateTask(agentId, "finished");
      if (payload.message) State.appendStep(agentId, payload.message.slice(0, 60));
      Sound.play("finish");
      if (focused) surface("finished", true);
      else State.setPillBadge(agentId, "finished");
      window.setTimeout(() => {
        if (transient) {
          State.removeTask(agentId);
        } else {
          State.updateTask(agentId, "idle");
          State.setPillBadge(agentId, null);
        }
      }, 5200);
      break;

    case "StopFailure":
      State.updateTask(agentId, "error");
      Sound.play("error");
      if (focused) surface("error", true);
      else State.setPillBadge(agentId, "error");
      break;

    case "SessionEnd":
      if (transient) {
        State.removeTask(agentId);
      } else if (isExternalAgent) {
        State.updateTask(agentId, "idle");
        State.setPillBadge(agentId, null);
      } else {
        State.updateTask(agentId, "idle");
        clearSession();
      }
      break;

    case "SubagentStart":
      State.appendStep(agentId, "Subagent started");
      break;

    case "SubagentStop":
      State.appendStep(agentId, "Subagent done");
      break;

    case "PermissionRequest": {
      // An external agent gets a card only if its relay can carry the answer
      // back (opencode's plugin can); any other re-asks in its terminal.
      if (isExternalAgent && !APPROVAL_AGENTS.has(agentId)) {
        if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
        break;
      }

      const requestId = payload.request_id ?? "";
      // One card, one request. A second one must never quietly replace the first
      // — that would leave a human staring at request B while request A waits for
      // a decision nobody can give. Hand it straight back to the terminal.
      if (State.pendingApproval && State.pendingApproval.requestId !== requestId) {
        if (requestId) void Bridge.approvalDecline(requestId);
        break;
      }
      ensurePill();
      if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
      const tool = payload.tool_name ?? "Tool";
      const input = payload.tool_input ?? {};
      State.pendingApproval = {
        requestId,
        agentId,
        sessionId: payload.session_id ?? "",
        tool,
        command: approvalTarget(tool, input),
      };
      // The relay's short ack window closes in 800 ms; everything below this
      // line is synchronous, so the card really is up by the time it lands.
      if (requestId) void Bridge.approvalAck(requestId);
      State.updateTask(agentId, "approval");
      State.isPinned = true;
      Sound.play("approval");
      if (focused) {
        island.alert("approval");
      } else {
        // Another agent holds the view, so the card would yank it away. The badge
        // is the signal instead — but it has to be on screen for that to mean
        // anything, hence the reveal. We just told the relay a human can act.
        State.setPillBadge(agentId, "approval");
        island.reveal();
      }
      // Coucou answers within 108 s or not at all; after that the terminal has
      // taken over and the card would be lying.
      pendingTimeout = window.setTimeout(() => {
        pendingTimeout = null;
        if (State.pendingApproval) clearApproval(island, State.pendingApproval.agentId);
      }, 110_000);
      break;
    }

    default:
      break;
  }
  State.notify();
}
