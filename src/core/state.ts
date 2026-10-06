// App state — mirror of AppState.swift (the parts the island needs).

import type { BotEmoteName, BotStateName, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../mochi/engine";

export type AgentSource = "claudeCode" | "n8n" | "agent";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
  /** The session's ancestor processes (Linux), to find its terminal window. */
  sessionPids?: number[] | null;
  /** Every live session of this agent, most recent first. */
  sessions?: AgentSession[];
  /** What the turn that just ended did, for the finished card. */
  lastTurn?: TurnSummary | null;
}

/** One turn of a session, from the prompt to Stop. */
export interface TurnSummary {
  /** Files edited or written, in order, without repeats. */
  files: string[];
  /** Prompt to Stop, when Coucou saw the prompt. */
  ms?: number;
  /** USD. Claude Code: what the turn would cost on the API. opencode: billed. */
  cost?: number;
  /** The cost is the API price of a turn run on a subscription. */
  apiEquivalent?: boolean;
}

/** One session of an agent: one terminal, one project. */
export interface AgentSession {
  id: string;
  project: string;
  cwd: string;
  pids: number[];
  state: BotStateName;
  /** What it did last. */
  step: string;
  /** Last event, ms since 1970. */
  updated: number;
  /** What the session has spent so far, when the agent says (opencode). */
  cost?: number;
  tokens?: number;
}

export interface ApprovalInfo {
  requestId: string;
  /** The pill asking: Claude Code, or an agent whose relay can carry the answer back. */
  agentId: string;
  sessionId: string;
  tool: string;
  command: string;
  /** Tool name and input, to recognise this call's PostToolUse. */
  toolKey: string;
}

/** One question an agent asks (AskUserQuestion, opencode's question tool). */
export interface AgentQuestion {
  question: string;
  header: string;
  options: { label: string; description: string }[];
  multiSelect: boolean;
}

/** Questions waiting on the island, answered one after the other. */
export interface QuestionInfo {
  requestId: string;
  agentId: string;
  sessionId: string;
  questions: AgentQuestion[];
  /** The question on screen. */
  index: number;
  answers: Record<string, string | string[]>;
  /** Labels ticked so far on a question that takes several. */
  picked: string[];
}

/** A permission request or question waiting behind the card on screen. */
export interface QueuedCard {
  requestId: string;
  agentId: string;
  /** The hook payload as it came: the card is built from it when its turn comes. */
  payload: {
    session_id?: string;
    tool_name?: string;
    tool_input?: Record<string, unknown>;
  };
  /** Date.now() when the request came in: its relay's wait started then. */
  arrivedAt: number;
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
}

export type PromptContext =
  | { kind: "window"; appName: string; title: string; url?: string }
  | { kind: "file"; name: string; path?: string };

export interface ResultItem {
  label: string;
  detail: string;
  url?: string;
}

export interface SearchResult {
  title: string;
  items: ResultItem[];
  note?: string;
}

const task = (
  id: string, name: string, color: string, source: AgentSource,
): AgentTask => ({
  id, name, color, state: "idle", stepIndex: 0, steps: [], source, isIntegration: true,
});

/** AgentTask.integrationAgents — same ids, names and colours as macOS. */
export const INTEGRATION_AGENTS: AgentTask[] = [
  task("integration_claude", "Claude Code", "#F5F6F8", "claudeCode"),
  task("integration_resend", "Resend", "#22C55E", "n8n"),
  task("integration_n8n", "n8n", "#F29B38", "n8n"),
  task("integration_vercel", "Vercel", "#7C5CFF", "n8n"),
  task("integration_github", "GitHub", "#F4505E", "n8n"),
  task("integration_notion", "Notion", "#8C8C8C", "n8n"),
  task("integration_calcom", "Cal.com", "#C9956A", "n8n"),
  task("integration_stripe", "Stripe", "#0570DE", "n8n"),
];

export const TOGGLEABLE_INTEGRATION_IDS = [
  "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  "integration_notion", "integration_calcom", "integration_stripe",
];

/** What an integration poller last reported. */
export interface IntegrationInfo {
  data: Record<string, unknown>;
  error: string | null;
  loaded: boolean;
  configured: boolean;
}

export interface Settings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  absenceInterval: number;
  activeIntegrations: string[];
  screen: "primary" | "cursor";
  autostart: boolean;
  hooksInstalled: boolean;
  /** Claude model used by the chat with an API key. */
  model: string;
  /** "claude-code" (subscription, through the claude CLI), "opencode" or "api-key". */
  chatBackend: string;
  /** Model alias for Claude Code: "sonnet", "opus", "haiku". */
  cliModel: string;
  /** "provider/model" for opencode; empty: opencode's own default. */
  opencodeModel: string;
  /** Minutes before an idle Claude Code process stops; 0 = never. */
  chatIdleMinutes: number;
  /** Owned by Rust: the conversation "Continue last chat" picks up. */
  lastChatSession: string | null;
  /** Look for a newer release on GitHub every few hours (packaged installs). */
  autoUpdate: boolean;
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  model: "claude-opus-5",
  chatBackend: "claude-code",
  cliModel: "sonnet",
  opencodeModel: "",
  chatIdleMinutes: 30,
  lastChatSession: null,
  autoUpdate: true,
};

type Listener = () => void;

class AppState {
  mode: IslandMode = "hidden";
  view: IslandViewName = "overview";

  tasks: AgentTask[] = [];
  focusId: string | null = null;

  stateOverride: BotStateName | null = null;

  /** Cursor in logical screen pixels, origin top-left (like AppState.mousePosition). */
  mouse = { x: 0, y: 0 };
  /** Cursor relative to the island's top-left corner. */
  mouseInIsland = { x: 0, y: 0 };

  isPinned = false;
  paused = false;

  uploadProgress = 0;
  uploadDuration = 2.4;
  fileDragOver = false;

  promptContext: PromptContext | null = null;
  droppedFile: { name: string; path: string } | null = null;
  noteMessage: string | null = null;
  searchResult: SearchResult | null = null;
  chatHistory: ChatMessage[] = [];
  pendingApproval: ApprovalInfo | null = null;
  pendingQuestion: QuestionInfo | null = null;
  /** Pixels the question card needs beyond its usual height (wrapped answers). */
  questionExtra = 0;
  /** Requests waiting behind the card on screen, oldest first. */
  cardQueue: QueuedCard[] = [];

  integrations: Record<string, IntegrationInfo> = {};

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Marks the UI dirty; the island re-renders on the next frame. */
  notify() {
    for (const fn of this.listeners) fn();
  }

  get focusTask(): AgentTask | null {
    return this.tasks.find((t) => t.id === this.focusId) ?? this.tasks[0] ?? null;
  }

  get effectiveState(): BotStateName {
    return this.stateOverride ?? this.focusTask?.state ?? "idle";
  }

  get otherTasks(): AgentTask[] {
    return this.tasks.filter((t) => t.id !== this.focusId);
  }

  setFocus(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    this.focusId = id;
    t.pillBadge = null;
    this.notify();
  }

  updateTask(id: string, state: BotStateName) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state;
    this.notify();
  }

  appendStep(id: string, step: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    this.notify();
  }

  /** Records what a session of agent `id` just did; the newest goes first. */
  noteSession(id: string, sessionId: string, patch: Partial<AgentSession>) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t || !sessionId) return;
    const list = t.sessions ?? [];
    const prev = list.find((s) => s.id === sessionId);
    const next: AgentSession = {
      id: sessionId, project: "", cwd: "", pids: [], state: "idle", step: "",
      ...prev,
      ...Object.fromEntries(Object.entries(patch).filter(([, v]) => v !== undefined && v !== "")),
      updated: Date.now(),
    };
    // Sessions quiet for half a day are gone, whatever they last said.
    const stale = Date.now() - 12 * 3600_000;
    t.sessions = [next, ...list.filter((s) => s.id !== sessionId && s.updated > stale)].slice(0, 8);
    this.notify();
  }

  endSession(id: string, sessionId: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t?.sessions) return;
    t.sessions = t.sessions.filter((s) => s.id !== sessionId);
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /** loadIntegrationTasks() — VS Code always on, the rest opt-in (max 4). */
  loadIntegrationTasks() {
    for (const proto of INTEGRATION_AGENTS) {
      const shouldLoad =
        proto.id === "integration_claude" || this.settings.activeIntegrations.includes(proto.id);
      const idx = this.tasks.findIndex((t) => t.id === proto.id);
      if (shouldLoad && idx < 0) this.tasks.push({ ...proto, steps: [] });
      if (!shouldLoad && idx >= 0) this.tasks.splice(idx, 1);
    }
    // Order: integration_claude first, then agent_* pills (visible in slice(0,4)),
    // then other integrations in declaration order.
    const order = INTEGRATION_AGENTS.map((t) => t.id);
    this.tasks.sort((a, b) => {
      const isAgentA = a.id.startsWith("agent_");
      const isAgentB = b.id.startsWith("agent_");
      // integration_claude always first
      if (a.id === "integration_claude") return -1;
      if (b.id === "integration_claude") return 1;
      // agent_* before other integrations; preserve insertion order among themselves
      if (isAgentA && !isAgentB) return -1;
      if (isAgentB && !isAgentA) return 1;
      if (isAgentA && isAgentB) return 0;
      // both known integrations → declaration order
      return order.indexOf(a.id) - order.indexOf(b.id);
    });
    if (!this.focusId) this.focusId = "integration_claude";
    this.notify();
  }

  removeTask(id: string) {
    const idx = this.tasks.findIndex((t) => t.id === id);
    if (idx < 0) return;
    this.tasks.splice(idx, 1);
    if (this.focusId === id) this.focusId = this.tasks[0]?.id ?? "integration_claude";
    this.notify();
  }

  /** Creates a dynamic agent_ pill on first event; no-ops if it already exists.
   *  Inserted right after integration_claude so it appears in the visible slice(0,4). */
  upsertExternalAgent(id: string, name: string, color: string) {
    if (this.tasks.some((t) => t.id === id)) return;
    const at = this.tasks.findIndex((t) => t.id === "integration_claude") + 1;
    this.tasks.splice(at, 0, {
      id, name, color,
      state: "idle", stepIndex: 0, steps: [],
      source: "agent", isIntegration: false,
    });
    if (!this.focusId) this.focusId = id;
    this.notify();
  }

  toggleIntegration(id: string) {
    if (id === "integration_claude") return;
    const active = this.settings.activeIntegrations;
    if (active.includes(id)) {
      this.settings.activeIntegrations = active.filter((x) => x !== id);
      if (this.focusId === id) this.focusId = "integration_claude";
    } else {
      if (active.length >= 4) return;
      this.settings.activeIntegrations = [...active, id];
    }
    this.loadIntegrationTasks();
  }

  defaultView(): IslandViewName {
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
