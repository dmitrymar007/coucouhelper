// Coucou — puts this opencode's sessions on the Coucou island, the way Claude
// Code's hooks do: what it is doing, when it is done or failed, and its
// permission requests, with Allow and Deny.
//
// Coucou writes this file to ~/.config/opencode/plugin/ from Settings →
// opencode, with the path of its relay filled in below. Every event goes
// through that relay (coucou-hook --agent opencode), exactly like a Claude Code
// hook, and nothing else leaves this machine.
//
// Never in the way: when Coucou is not running the relay exits at once, and a
// permission request stays in opencode too — whichever answers first, the
// terminal or the island, decides.

import { spawn } from "node:child_process";

const RELAY = "__COUCOU_RELAY__";
const AGENT = "opencode";

/** opencode's tool names → the Claude Code names the island knows. */
const TOOLS = {
  bash: "Bash",
  read: "Read",
  write: "Write",
  edit: "Edit",
  multiedit: "MultiEdit",
  patch: "Edit",
  apply_patch: "Edit",
  glob: "Glob",
  grep: "Grep",
  list: "LS",
  webfetch: "WebFetch",
  websearch: "WebSearch",
  task: "Task",
  todowrite: "TodoWrite",
};

const toolName = (tool) => TOOLS[tool] ?? tool;

/** The strings of a tool's arguments, under Claude Code's field names. */
function toolInput(args) {
  const input = {};
  for (const [key, value] of Object.entries(args ?? {})) {
    if (typeof value !== "string") continue;
    input[key === "filePath" || key === "filepath" ? "file_path" : key] = value;
  }
  return input;
}

/**
 * Hands one event to Coucou. `answer` resolves with the island's decision for
 * a permission request or a question — `{ behavior: "allow" | "deny",
 * answers? }` — or null; `child` is the relay, to drop the request when it is
 * answered in opencode first.
 */
function relay(payload, waitForAnswer = false) {
  let child;
  try {
    child = spawn(RELAY, ["--agent", AGENT], {
      stdio: ["pipe", waitForAnswer ? "pipe" : "ignore", "ignore"],
    });
  } catch {
    return { child: null, answer: Promise.resolve(null) };
  }
  const answer = new Promise((resolve) => {
    let out = "";
    child.stdout?.on("data", (chunk) => (out += chunk));
    child.on("error", () => resolve(null));
    child.on("close", () => {
      try {
        const decision = JSON.parse(out).hookSpecificOutput?.decision;
        const behavior = decision?.behavior;
        resolve(
          behavior === "allow" || behavior === "deny"
            ? { behavior, answers: decision.updatedInput?.answers ?? null }
            : null,
        );
      } catch {
        resolve(null);
      }
    });
  });
  child.stdin.on("error", () => {});
  child.stdin.end(JSON.stringify(payload));
  return { child, answer };
}

/** What a permission is about, as the island's approval card shows it. */
function permissionTarget(p) {
  const meta = p.metadata ?? {};
  const patterns = Array.isArray(p.patterns) ? p.patterns.join(", ") : "";
  switch (p.permission) {
    case "bash":
      return { tool_name: "Bash", tool_input: { command: meta.command ?? patterns, description: meta.description ?? "" } };
    case "edit":
    case "write":
      return { tool_name: toolName(p.permission), tool_input: { file_path: meta.filepath ?? meta.filePath ?? patterns } };
    case "webfetch":
      return { tool_name: "WebFetch", tool_input: { url: meta.url ?? patterns } };
    default:
      return { tool_name: p.permission ?? "Permission", tool_input: { command: patterns } };
  }
}

export const Coucou = async ({ client, directory }) => {
  // Coucou's own chat runs opencode too: it is not a session to show.
  if (process.env.COUCOU_CHAT) return {};

  /** Sessions started by a task tool → their parent: subagent steps of it. */
  const children = new Map();
  /** Sessions this opencode has shown, to end them when it quits. */
  const open = new Set();
  /** Permission requests on the island: id → relay process. */
  const asking = new Map();
  /**
   * Per session: each answer's cost and tokens (read: input and cache; written:
   * output and reasoning), the context of the latest one, and the totals when
   * the current turn began.
   */
  const spend = new Map();

  function noteSpend(info) {
    if (info?.role !== "assistant" || !info.sessionID || !info.id) return;
    const s = spend.get(info.sessionID) ?? { costs: new Map(), read: new Map(), wrote: new Map(), context: 0, turn: null };
    if (typeof info.cost === "number") s.costs.set(info.id, info.cost);
    const t = info.tokens;
    const context = t ? (t.input ?? 0) + (t.cache?.read ?? 0) + (t.cache?.write ?? 0) : 0;
    if (context > 0) s.context = context;
    if (t) {
      s.read.set(info.id, context);
      s.wrote.set(info.id, (t.output ?? 0) + (t.reasoning ?? 0));
    }
    spend.set(info.sessionID, s);
  }

  const sum = (m) => [...m.values()].reduce((a, b) => a + b, 0);

  /** A prompt: what the session has used so far is where this turn starts. */
  function turnStarts(sessionID) {
    const s = spend.get(sessionID) ?? { costs: new Map(), read: new Map(), wrote: new Map(), context: 0, turn: null };
    s.turn = { read: sum(s.read), wrote: sum(s.wrote) };
    spend.set(sessionID, s);
  }

  function spent(sessionID) {
    const s = spend.get(sessionID);
    if (!s) return {};
    const out = { cost: sum(s.costs), tokens: s.context };
    if (s.turn) {
      out.turn_tokens_in = Math.max(0, sum(s.read) - s.turn.read);
      out.turn_tokens_out = Math.max(0, sum(s.wrote) - s.turn.wrote);
    }
    return out;
  }

  const base = (event, sessionID) => ({ hook_event_name: event, session_id: sessionID, cwd: directory });
  const send = (event, sessionID, extra = {}) => {
    if (!sessionID || children.has(sessionID)) return;
    open.add(sessionID);
    relay({ ...base(event, sessionID), ...extra });
  };

  async function ask(p) {
    if (children.has(p.sessionID)) return;
    const { child, answer } = relay(
      { ...base("PermissionRequest", p.sessionID), ...permissionTarget(p), permission_id: p.id },
      true,
    );
    if (!child) return;
    asking.set(p.id, child);
    const decision = await answer;
    // Gone from the map: opencode had the answer first.
    if (!asking.delete(p.id) || !decision) return;
    try {
      await client.postSessionIdPermissionsPermissionId({
        path: { id: p.sessionID, permissionID: p.id },
        body: { response: decision.behavior === "allow" ? "once" : "reject" },
      });
    } catch {
      // Answered meanwhile, or opencode is going away: nothing to do.
    }
  }

  /**
   * opencode's question tool, asked on the island the way Claude Code's
   * AskUserQuestion is: the same request, the options as buttons. The island
   * answers question text → label(s); opencode wants one list of labels per
   * question, in order.
   */
  async function askQuestion(p) {
    if (children.has(p.sessionID) || !Array.isArray(p.questions)) return;
    const questions = p.questions.map((q) => ({
      question: q.question,
      header: q.header ?? "",
      options: (q.options ?? []).map((o) => ({ label: o.label, description: o.description ?? "" })),
      multiSelect: q.multiple === true,
    }));
    const { child, answer } = relay(
      {
        ...base("PermissionRequest", p.sessionID),
        tool_name: "AskUserQuestion",
        tool_input: { questions },
        question_id: p.id,
      },
      true,
    );
    if (!child) return;
    asking.set(p.id, child);
    const decision = await answer;
    if (!asking.delete(p.id) || !decision?.answers) return;
    const answers = questions.map((q) => {
      const a = decision.answers[q.question];
      if (Array.isArray(a)) return a.map(String);
      return typeof a === "string" ? [a] : [];
    });
    try {
      // The plugin's client predates the question API; its transport does not.
      await client._client.post({
        url: "/question/{requestID}/reply",
        path: { requestID: p.id },
        body: { answers },
        headers: { "Content-Type": "application/json" },
      });
    } catch {
      // Answered meanwhile, or opencode is going away: nothing to do.
    }
  }

  return {
    event: async ({ event }) => {
      const p = event.properties ?? {};
      switch (event.type) {
        case "session.created":
          if (p.info?.parentID) {
            children.set(p.info.id, p.info.parentID);
            send("SubagentStart", p.info.parentID);
          } else {
            send("SessionStart", p.info?.id ?? p.sessionID);
          }
          break;
        case "session.idle":
          if (children.has(p.sessionID)) {
            send("SubagentStop", children.get(p.sessionID));
          } else {
            send("Stop", p.sessionID, spent(p.sessionID));
          }
          break;
        case "session.error":
          // Esc in opencode aborts the answer: not a failure worth an alert.
          if (p.error?.name === "MessageAbortedError") break;
          send("StopFailure", p.sessionID, { message: String(p.error?.data?.message ?? p.error?.name ?? "error") });
          break;
        case "session.deleted":
          send("SessionEnd", p.info?.id ?? p.sessionID);
          open.delete(p.info?.id ?? p.sessionID);
          spend.delete(p.info?.id ?? p.sessionID);
          break;
        case "permission.asked":
          void ask(p);
          break;
        case "message.updated":
          noteSpend(p.info);
          break;
        case "question.asked":
          void askQuestion(p);
          break;
        case "question.replied":
        case "question.rejected":
        case "permission.replied": {
          const id = p.requestID ?? p.permissionID;
          const child = asking.get(id);
          if (child) {
            asking.delete(id);
            child.kill();
          }
          break;
        }
      }
    },
    "chat.message": async (input, output) => {
      const prompt = (output?.parts ?? [])
        .filter((part) => part.type === "text" && !part.synthetic)
        .map((part) => part.text)
        .join("\n")
        .trim();
      if (prompt) {
        turnStarts(input.sessionID);
        send("UserPromptSubmit", input.sessionID, { prompt: prompt.slice(0, 500) });
      }
    },
    "tool.execute.before": async (input, output) => {
      send("PreToolUse", input.sessionID, { tool_name: toolName(input.tool), tool_input: toolInput(output?.args) });
    },
    "tool.execute.after": async (input) => {
      send("PostToolUse", input.sessionID, { tool_name: toolName(input.tool) });
    },
    dispose: async () => {
      for (const child of asking.values()) child.kill();
      asking.clear();
      for (const id of open) relay({ ...base("SessionEnd", id) });
      open.clear();
    },
  };
};
