// Claude Code hook events → island state.
// Port of HookServer.processEvent / processPermissionRequest from the macOS app.
// Difference from macOS: no terminal filter. On Windows the hook fires from any
// terminal (Windows Terminal, VS Code, PowerShell…) and all of them are handled.

import { Bridge, onEvent } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type AgentQuestion, type QueuedCard } from "../core/state";
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

/** What each session's current turn has done so far (session id → turn). */
interface TurnRecord {
  start: number;
  files: string[];
  /** The file the last edit is about, from PreToolUse: opencode's PostToolUse leaves it out. */
  editing?: string;
}
const turns = new Map<string, TurnRecord>();
const EDIT_TOOLS: ReadonlySet<string> = new Set(["Edit", "Write", "MultiEdit", "NotebookEdit"]);

function editedFile(input: Record<string, unknown> | undefined): string {
  const v = input?.file_path ?? input?.notebook_path ?? input?.path;
  return typeof v === "string" ? v : "";
}

/** Keeps the turn's start, the files it changed and, at Stop, its summary. */
function noteTurn(name: string, agentId: string, payload: HookPayload) {
  const sid = payload.session_id ?? "";
  if (!sid) return;
  const tool = payload.tool_name ?? "";
  switch (name) {
    case "UserPromptSubmit":
      turns.set(sid, { start: Date.now(), files: [] });
      break;
    case "PreToolUse":
      if (EDIT_TOOLS.has(tool)) {
        const rec = turns.get(sid) ?? { start: 0, files: [] };
        rec.editing = editedFile(payload.tool_input);
        turns.set(sid, rec);
      }
      break;
    case "PostToolUse": {
      if (!EDIT_TOOLS.has(tool)) break;
      const rec = turns.get(sid) ?? { start: 0, files: [] };
      const file = editedFile(payload.tool_input) || rec.editing || "";
      if (file && !rec.files.includes(file)) rec.files.push(file);
      rec.editing = undefined;
      turns.set(sid, rec);
      break;
    }
    case "Stop": {
      const rec = turns.get(sid);
      turns.delete(sid);
      const count = (v: unknown) => (typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : undefined);
      const task = State.tasks.find((t) => t.id === agentId);
      if (task) {
        task.lastTurn = {
          files: rec?.files ?? [],
          ms: rec && rec.start > 0 ? Date.now() - rec.start : undefined,
          tokensIn: count(payload.turn_tokens_in),
          tokensOut: count(payload.turn_tokens_out),
        };
      }
      break;
    }
    case "SessionEnd":
      turns.delete(sid);
      break;
  }
}

export interface HookPayload {
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
  /** On Stop: what the session has cost (opencode) and the context its last
   *  answer used (opencode's plugin; Claude Code's through the relay). */
  cost?: number;
  /** On Stop: the tokens the turn read and wrote (relay for Claude Code, plugin for opencode). */
  turn_tokens_in?: number;
  turn_tokens_out?: number;
  tokens?: number;
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
  void onEvent<string>("card-key", (accel) => cardKey(island, accel));
  State.subscribe(syncCardKeys);
  // The relay went away while the card was up: the question was answered in
  // the terminal (or the agent quit), so the card has nothing left to decide.
  void onEvent<string>("approval-gone", (requestId) => {
    if (State.pendingApproval?.requestId === requestId) clearApproval(island, State.pendingApproval.agentId);
    if (State.pendingQuestion?.requestId === requestId) clearQuestion(island, State.pendingQuestion.agentId);
    dropQueued(requestId, false);
  });
}

/** A tool call, as a PermissionRequest and its PostToolUse both carry it. */
function toolKey(tool: string, input: unknown): string {
  return `${tool} ${JSON.stringify(input ?? {})}`;
}

/**
 * The session went on without the island: the question was answered where the
 * agent runs. Claude Code in VS Code keeps the hook waiting after an answer in
 * its own panel, so the relay never goes away and "approval-gone" never comes.
 * A later event of the same session says it instead — this very call's
 * PostToolUse (a question's input comes back with its answers, so only the tool
 * is compared), or the end of the turn. The card goes, and the relay is let go.
 */
function settledElsewhere(island: Island, name: string, payload: HookPayload) {
  const sid = payload.session_id ?? "";
  if (!sid) return;
  const after = name === "PostToolUse" || name === "PostToolUseFailure";
  const turnOver = name === "Stop" || name === "StopFailure" || name === "UserPromptSubmit" || name === "SessionEnd";
  const a = State.pendingApproval;
  if (a && a.sessionId === sid && (turnOver || (after && toolKey(payload.tool_name ?? "Tool", payload.tool_input) === a.toolKey))) {
    void Bridge.approvalDecline(a.requestId);
    clearApproval(island, a.agentId);
  }
  const q = State.pendingQuestion;
  if (q && q.sessionId === sid && (turnOver || (after && payload.tool_name === "AskUserQuestion"))) {
    void Bridge.approvalDecline(q.requestId);
    clearQuestion(island, q.agentId);
  }
  for (const c of [...State.cardQueue]) {
    if (c.payload.session_id !== sid) continue;
    const tool = c.payload.tool_name ?? "Tool";
    const same = tool === "AskUserQuestion"
      ? payload.tool_name === "AskUserQuestion"
      : toolKey(payload.tool_name ?? "Tool", payload.tool_input) === toolKey(tool, c.payload.tool_input ?? {});
    if (turnOver || (after && same)) dropQueued(c.requestId, true);
  }
}

/**
 * The pill the user was on before a card took the island over. A card is for
 * acting on now, so it opens on its own agent's pill even when another one
 * held the view; that pill comes back once the card goes, as on the Mac.
 */
let returnFocus: string | null = null;

function takeFocus(agentId: string) {
  if (State.focusId === agentId) return;
  returnFocus = State.focusId;
  State.setFocus(agentId);
}

function giveFocusBack() {
  const id = returnFocus;
  returnFocus = null;
  if (id && State.tasks.some((t) => t.id === id)) State.setFocus(id);
}

/** Takes the question card down and hands the island back. */
function clearQuestion(island: Island, agentId: string) {
  if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
  pendingTimeout = null;
  State.pendingQuestion = null;
  State.isPinned = false;
  island.dropPin();
  State.updateTask(agentId, "working");
  State.setPillBadge(agentId, null);
  giveFocusBack();
  if (State.view === "question") island.setView(State.defaultView());
  State.notify();
  showNextCard(island);
}

/**
 * AskUserQuestion's `questions`, checked: 1–4 questions of 2+ options, each a
 * label (and maybe a description). Anything else goes back to the terminal.
 */
function parseQuestions(raw: unknown): AgentQuestion[] | null {
  if (!Array.isArray(raw) || raw.length === 0 || raw.length > 4) return null;
  const out: AgentQuestion[] = [];
  for (const q of raw) {
    if (!q || typeof q !== "object") return null;
    const { question, header, options, multiSelect } = q as Record<string, unknown>;
    if (typeof question !== "string" || !question.trim() || !Array.isArray(options) || options.length < 1) return null;
    const opts = options.slice(0, 6).map((o) => {
      const r = (o ?? {}) as Record<string, unknown>;
      return {
        label: typeof r.label === "string" ? r.label : "",
        description: typeof r.description === "string" ? r.description : "",
      };
    });
    if (opts.some((o) => !o.label)) return null;
    out.push({
      question,
      header: typeof header === "string" ? header : "",
      options: opts,
      multiSelect: multiSelect === true,
    });
  }
  return out;
}

/** The question card: a label picked (or, on a several-answers question, Done). */
export function answerQuestion(island: Island, label: string | null) {
  const q = State.pendingQuestion;
  if (!q) return;
  const current = q.questions[q.index];
  if (current.multiSelect) {
    if (label != null) {
      q.picked = q.picked.includes(label) ? q.picked.filter((l) => l !== label) : [...q.picked, label];
      State.notify();
      return;
    }
    if (q.picked.length === 0) return;
    q.answers[current.question] = q.picked;
  } else {
    if (label == null) return;
    q.answers[current.question] = label;
  }
  q.picked = [];
  q.index += 1;
  Sound.play("blip");
  if (q.index < q.questions.length) {
    State.notify();
    return;
  }
  void Bridge.approvalAnswer(q.requestId, q.answers);
  Sound.play("approve");
  clearQuestion(island, q.agentId);
}

/** The question card's "In terminal": the agent asks there instead. */
export function questionToTerminal(island: Island) {
  const q = State.pendingQuestion;
  if (!q) return;
  void Bridge.approvalDecline(q.requestId);
  clearQuestion(island, q.agentId);
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
  giveFocusBack();
  if (State.view === "approval") island.setView(State.defaultView());
  State.notify();
  showNextCard(island);
}

// ── Card shortcuts ───────────────────────────────────────────────────────────
// Alt+Shift+Y allows, Alt+Shift+N denies, Alt+Shift+1…9 picks an answer
// and Alt+Shift+Return ends a several-answers question — from any window,
// and only while the card is up: the GNOME extension grabs them for that time.

const KEY_ALLOW = "<Alt><Shift>y";
const KEY_DENY = "<Alt><Shift>n";
const KEY_DONE = "<Alt><Shift>Return";
const keyForOption = (i: number) => `<Alt><Shift>${i + 1}`;

let grabbedKeys = "";

/** Asks for exactly the shortcuts the card on screen can use. */
function syncCardKeys() {
  let keys: string[] = [];
  const q = State.pendingQuestion;
  if (State.pendingApproval) {
    keys = [KEY_ALLOW, KEY_DENY];
  } else if (q) {
    const current = q.questions[q.index];
    keys = current.options.slice(0, 9).map((_, i) => keyForOption(i));
    if (current.multiSelect) keys.push(KEY_DONE);
  }
  const id = keys.join(" ");
  if (id === grabbedKeys) return;
  grabbedKeys = id;
  void Bridge.setCardKeys(keys);
}

function cardKey(island: Island, accel: string) {
  if (State.pendingApproval) {
    if (accel === KEY_ALLOW) decideApproval(island, "allow");
    else if (accel === KEY_DENY) decideApproval(island, "deny");
    return;
  }
  const q = State.pendingQuestion;
  if (!q) return;
  const current = q.questions[q.index];
  if (accel === KEY_DONE && current.multiSelect) {
    answerQuestion(island, null);
    return;
  }
  const option = current.options.findIndex((_, i) => keyForOption(i) === accel);
  if (option >= 0) answerQuestion(island, current.options[option].label);
}

/** How long a relay waits for the island, from the moment it asked. */
const RELAY_WAIT_MS = 108_000;

/**
 * Puts a request's card up: a question (Claude Code's AskUserQuestion,
 * opencode's question tool) with its options as buttons, or a permission with
 * Allow and Deny. False when there is nothing to show, and the request has
 * gone back to the terminal.
 */
function presentCard(island: Island, card: QueuedCard): boolean {
  const { requestId, agentId, payload } = card;
  // A queued request has already spent part of its relay's wait.
  const left = Math.max(5_000, RELAY_WAIT_MS - (Date.now() - card.arrivedAt));
  if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
  pendingTimeout = null;
  if (payload.tool_name === "AskUserQuestion") {
    const questions = parseQuestions(payload.tool_input?.questions);
    if (!questions) {
      void Bridge.approvalDecline(requestId);
      return false;
    }
    State.pendingQuestion = {
      requestId, agentId, sessionId: payload.session_id ?? "", questions, index: 0, answers: {}, picked: [],
    };
    void Bridge.approvalAck(requestId);
    State.updateTask(agentId, "question");
    State.isPinned = true;
    Sound.play("question");
    takeFocus(agentId);
    island.alert("question");
    pendingTimeout = window.setTimeout(() => {
      pendingTimeout = null;
      if (State.pendingQuestion) clearQuestion(island, State.pendingQuestion.agentId);
    }, left);
    return true;
  }
  const tool = payload.tool_name ?? "Tool";
  const input = payload.tool_input ?? {};
  State.pendingApproval = {
    requestId,
    agentId,
    sessionId: payload.session_id ?? "",
    tool,
    command: approvalTarget(tool, input),
    toolKey: toolKey(tool, input),
  };
  // The relay's short ack window closes in 800 ms; everything below this
  // line is synchronous, so the card really is up by the time it lands.
  void Bridge.approvalAck(requestId);
  State.updateTask(agentId, "approval");
  State.isPinned = true;
  Sound.play("approval");
  takeFocus(agentId);
  island.alert("approval");
  // Coucou answers within the relay's wait or not at all; after that the
  // terminal has taken over and the card would be lying.
  pendingTimeout = window.setTimeout(() => {
    pendingTimeout = null;
    if (State.pendingApproval) clearApproval(island, State.pendingApproval.agentId);
  }, left);
  return true;
}

/** The next waiting request, once the card before it has gone. */
function showNextCard(island: Island) {
  while (!State.pendingApproval && !State.pendingQuestion) {
    const next = State.cardQueue.shift();
    if (!next) return;
    if (Date.now() - next.arrivedAt > RELAY_WAIT_MS - 3_000) {
      // Its relay is about to give up: the terminal has it.
      void Bridge.approvalDecline(next.requestId);
      continue;
    }
    if (!State.cardQueue.some((c) => c.agentId === next.agentId)) State.setPillBadge(next.agentId, null);
    presentCard(island, next);
  }
}

/** Takes a waiting request out of the queue (answered elsewhere, relay gone). */
function dropQueued(requestId: string, decline: boolean) {
  const i = State.cardQueue.findIndex((c) => c.requestId === requestId);
  if (i < 0) return;
  const [gone] = State.cardQueue.splice(i, 1);
  if (decline) void Bridge.approvalDecline(requestId);
  if (!State.cardQueue.some((c) => c.agentId === gone.agentId)) State.setPillBadge(gone.agentId, null);
  State.notify();
}

/** Allow or Deny on the permission card, by click or by shortcut. */
export function decideApproval(island: Island, decision: "allow" | "deny") {
  const req = State.pendingApproval;
  void Bridge.log(`decide ${decision} req=${req?.requestId ?? "none"}`);
  if (!req) return;
  Sound.play(decision === "deny" ? "blip" : "approve");
  void Bridge.approvalDecision(req.requestId, decision);
  clearApproval(island, req.agentId);
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

  settledElsewhere(island, name, payload);
  noteTurn(name, agentId, payload);

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
      if (!requestId) break;
      ensurePill();
      // One card on screen at a time. Another request waits its turn behind it
      // — acknowledged, so its relay keeps waiting for a human — and shows as
      // soon as the card before it goes.
      if (State.pendingApproval || State.pendingQuestion) {
        State.cardQueue.push({ requestId, agentId, payload, arrivedAt: Date.now() });
        void Bridge.approvalAck(requestId);
        State.setPillBadge(agentId, "approval");
        Sound.play("blip");
        break;
      }
      presentCard(island, { requestId, agentId, payload, arrivedAt: Date.now() });
      break;
    }

    default:
      break;
  }
  noteSession(name, agentId, payload, projectName, cwd, pids);
  State.notify();
}

/** Keeps the agent's list of sessions in step with what each one does. */
function noteSession(
  name: string, agentId: string, payload: HookPayload, project: string, cwd: string, pids: number[],
) {
  const sid = payload.session_id ?? "";
  if (!sid) return;
  const where = { project, cwd, pids: pids.length ? pids : undefined };
  const spent = {
    cost: typeof payload.cost === "number" && Number.isFinite(payload.cost) ? payload.cost : undefined,
    tokens: typeof payload.tokens === "number" && Number.isFinite(payload.tokens) ? payload.tokens : undefined,
  };
  switch (name) {
    case "SessionStart":
      State.noteSession(agentId, sid, { ...where, state: "idle", step: "Started" });
      break;
    case "UserPromptSubmit":
      State.noteSession(agentId, sid, { ...where, state: "thinking", step: (payload.prompt ?? payload.message ?? "").slice(0, 80) });
      break;
    case "PreToolUse":
      State.noteSession(agentId, sid, { ...where, state: "working", step: stepLabel(payload.tool_name ?? "Tool", payload.tool_input ?? {}) });
      break;
    case "PermissionRequest":
      State.noteSession(agentId, sid, {
        ...where,
        state: payload.tool_name === "AskUserQuestion" ? "question" : "approval",
        step: "Waiting for you",
      });
      break;
    case "Stop":
      State.noteSession(agentId, sid, { ...where, ...spent, state: "finished", step: payload.message?.slice(0, 80) || "Done" });
      break;
    case "StopFailure":
      State.noteSession(agentId, sid, { ...where, state: "error", step: payload.message?.slice(0, 80) || "Failed" });
      break;
    case "SessionEnd":
      State.endSession(agentId, sid);
      break;
  }
}
