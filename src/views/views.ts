// Island views — DOM ports of IslandViewContent.swift. Paddings, font sizes,
// colours and wording are copied from the Swift views so both platforms read
// identically.

import { h, svg, clear, dot } from "./dom";
import { Bridge } from "../core/bridge";
import { ICONS } from "./icons";
import { Ticker } from "./ticker";
import { State, type AgentTask, type TurnSummary } from "../core/state";
import { washRGBA, type IslandViewName, type Wash } from "../core/layout";
import { createMiniBot, pruneMiniBots } from "../mochi/minibots";
import { buildPrompt } from "./chat";
import { buildChoose, buildUpload, buildUploading } from "./upload";
import { buildMail } from "./mail";
import { renderIntegrationCard, type IntegrationCardHooks } from "./integrations";

export interface ViewActions {
  setView(v: IslandViewName): void;
  /** The view's content changed height: size the island to it again. */
  refit(): void;
  collapse(): void;
  setFocus(id: string): void;
  openTerminal(): void;
  /** The chat tab was clicked: take the window the user was in as context. */
  captureWindow(): void;
  /** The ↗ button: opens whatever the focused pill points at. */
  openTarget(): void;
  openUrl(url: string): void;
  decide(d: "allow" | "deny"): void;
  /** The question card: a label picked, or Done (null) on a several-answers question. */
  answer(label: string | null): void;
  /** The question card's "In terminal". */
  questionToTerminal(): void;
  toggleSound(): void;
  setVolume(v: number): void;
  setAutoClose(seconds: number): void;
  openSettingsWindow(): void;
  blip(): void;
}

export interface ViewHost {
  el: HTMLElement;
  sync(): void;
  /** Called when the view becomes active, for views with a text field. */
  focus?(): void;
  /** Called every frame while the view is on screen. */
  tick?(nowMs: number): void;
}

// ── Shared pieces ─────────────────────────────────────────────────────────────

function card(wash: Wash, ...children: (Node | string)[]): HTMLElement {
  const el = h("div", { class: wash ? "card wash" : "card" }, ...children);
  if (wash) el.style.setProperty("--wash", washRGBA(wash));
  return el;
}

function btn(
  label: string,
  kind: "primary" | "secondary",
  onClick: () => void,
  kbd?: string,
): HTMLElement {
  return h(
    "button",
    { class: `btn ${kind}`, onclick: onClick },
    h("span", { text: label }),
    kbd ? h("span", { class: "kbd", text: kbd }) : null,
  );
}

/** AgentWho — coloured dot + task name + grey label. */
function agentWho(task: AgentTask | null, label: string): HTMLElement {
  const row = h("div", { class: "who-row" });
  if (task) {
    row.append(dot(task.color, 8), h("span", { class: "n", text: task.name }));
  }
  row.append(h("span", { text: label }));
  return row;
}

function stack(padLeft: number, padRight: number, ...children: Node[]): HTMLElement {
  const el = h("div", { class: "stack" }, ...children);
  el.style.padding = `4px ${padRight}px 4px ${padLeft}px`;
  return el;
}

// ── Sessions ──────────────────────────────────────────────────────────────────

const SESSION_COLORS: Partial<Record<string, string>> = {
  working: "#60A5FA", thinking: "#A78BFA", approval: "#F5A524", question: "#22D3EE",
  finished: "#22C55E", error: "#F4505E",
};

function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.round(s / 60)}m`;
  return `${Math.round(s / 3600)}h`;
}

/**
 * "$0.012 · 45.2k context": what a session has cost (opencode reports it; on
 * a subscription there is nothing to pay per session) and how much of the
 * model's context its last answer used.
 */
export function spentLabel(cost?: number, tokens?: number): string {
  const parts: string[] = [];
  if (cost != null && cost > 0) parts.push(`$${cost < 0.1 ? cost.toFixed(3) : cost.toFixed(2)}`);
  if (tokens != null && tokens > 0) parts.push(tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k context` : `${tokens} context`);
  return parts.join(" · ");
}

/** An agent's sessions, newest first; a click brings that session's terminal back. */
function sessionList(task: AgentTask, onBack: () => void): HTMLElement {
  const rows = h("div", { class: "sess-rows" });
  for (const sess of task.sessions ?? []) {
    const spent = spentLabel(sess.cost, sess.tokens);
    rows.append(
      h(
        "button",
        {
          class: "sess-row",
          title: sess.cwd,
          onclick: () => void Bridge.focusTerminal(sess.pids, sess.cwd || null),
        },
        dot(SESSION_COLORS[sess.state] ?? "#6B7079", 6),
        h("b", { text: sess.project || "Session" }),
        h("span", { class: "sess-step", text: sess.step }),
        h("span", { class: "sess-meta", text: spent ? `${spent} · ${ago(sess.updated)}` : ago(sess.updated) }),
      ),
    );
  }
  return h(
    "div",
    { class: "int-card detail" },
    h(
      "div",
      { class: "int-detail-head" },
      h("button", { class: "int-back", onclick: onBack }, svg(ICONS.chevronLeft, 10, { stroke: 2.4 })),
      dot(task.color, 6),
      h("b", { text: task.source === "claudeCode" ? "Claude Code" : task.name }),
      h("span", {
        class: "int-badge",
        style: "color:#9398a1;background:rgba(255,255,255,0.08)",
        text: `${task.sessions?.length ?? 0} sessions`,
      }),
    ),
    rows,
  );
}

// ── Header ────────────────────────────────────────────────────────────────────

export function buildHeader(actions: ViewActions): ViewHost {
  const tabHome = h("button", { class: "tab", title: "Overview", onclick: () => go("overview") }, svg(ICONS.house, 13));
  const tabChat = h(
    "button",
    { class: "tab", title: "Ask", onclick: () => { actions.captureWindow(); go("prompt"); } },
    svg(ICONS.bubble, 13),
  );
  const tabDrop = h("button", { class: "tab", title: "Drop", onclick: () => go("upload") }, svg(ICONS.plus, 13));

  const gearBtn = h("button", { title: "Settings", onclick: () => go("settings") }, svg(ICONS.gear, 14));
  const soundBtn = h("button", { title: "Mute", onclick: () => actions.toggleSound() }, svg(ICONS.speakerOn, 14));

  function go(v: IslandViewName) {
    actions.blip();
    actions.setView(v);
  }

  const el = h(
    "div",
    { id: "header" },
    h("div", { class: "tabs" }, tabHome, tabChat, tabDrop),
    h("div", { class: "header-actions" }, gearBtn, soundBtn),
  );

  return {
    el,
    sync() {
      const v = State.view;
      tabHome.classList.toggle("on", v === "overview" || v === "empty");
      tabChat.classList.toggle("on", v === "prompt");
      tabDrop.classList.toggle("on", v === "upload");
      gearBtn.classList.toggle("on", v === "settings");
      clear(gearBtn);
      gearBtn.append(svg(v === "settings" ? ICONS.gearFill : ICONS.gear, 14));
      clear(soundBtn);
      soundBtn.append(svg(State.settings.soundEnabled ? ICONS.speakerOn : ICONS.speakerOff, 14));
      el.style.opacity = v === "confused" ? "0" : "1";
    },
  };
}

// ── Overview ──────────────────────────────────────────────────────────────────

function buildOverview(actions: ViewActions): ViewHost {
  const ticker = new Ticker();
  const who = h("div", { class: "who" });
  const tickerBody = h("div", { class: "card-body" }, who, ticker.el);
  const leftBody = h("div", { class: "left-body" });
  const jump = h(
    "button",
    { class: "icon-btn jump", title: "Open", onclick: () => actions.openTarget() },
    svg(ICONS.arrowUpRight, 8),
  );
  const left = card(null, leftBody, jump);
  const pills = h("div", { class: "pills" });
  const right = card(null, pills);

  const el = h("div", { class: "view overview" },
    h("div", { class: "left" }, left),
    h("div", { class: "right" }, right),
  );

  let pillIds = "";
  let detailOpen = false;
  let lastFocus: string | null = null;
  let mode: "ticker" | "card" | null = null;
  let cardKey = "";

  const hooks: IntegrationCardHooks = {
    get detailOpen() {
      return detailOpen;
    },
    openDetail() {
      detailOpen = true;
      cardKey = "";
      State.notify();
    },
    closeDetail() {
      detailOpen = false;
      cardKey = "";
      State.notify();
    },
    openSettings: () => actions.openSettingsWindow(),
  };

  return {
    el,
    tick(nowMs: number) {
      if (mode === "ticker") ticker.tick(nowMs);
    },
    sync() {
      const task = State.focusTask;
      if (task?.id !== lastFocus) {
        lastFocus = task?.id ?? null;
        detailOpen = false;
        cardKey = "";
        mode = null;
      }

      // An agent with a live session (Claude Code, opencode…) shows the ticker;
      // every other pill shows its own card, exactly like IntegrationCardView.
      const isAgent = task?.id === "integration_claude" || task?.source === "agent";
      const sessionActive = !!task && isAgent && (task.state !== "idle" || task.steps.length > 0);

      if (task && sessionActive && detailOpen && (task.sessions?.length ?? 0) > 0) {
        const key = `sessions~${task.id}~${JSON.stringify(task.sessions)}~${Math.floor(Date.now() / 30000)}`;
        if (key !== cardKey) {
          cardKey = key;
          mode = "card";
          clear(leftBody);
          leftBody.append(sessionList(task, () => hooks.closeDetail()));
        }
      } else if (task && sessionActive) {
        if (mode !== "ticker") {
          clear(leftBody);
          leftBody.append(tickerBody);
          mode = "ticker";
          cardKey = "";
        }
        clear(who);
        who.append(
          dot(task.color, 7),
          h("span", { class: "name", text: task.name }),
          h("span", {
            class: "tool",
            // Claude Code's pill takes the project's name during a session;
            // other agents keep theirs and show the project beside it.
            text: task.source === "claudeCode"
              ? "Claude Code"
              : task.source === "agent"
                ? (task.sessionCwd?.split(/[\\/]/).filter(Boolean).pop() ?? "Agent")
                : "n8n",
          }),
        );
        if (task.steps.length > 1) {
          who.append(h("span", {
            class: "count",
            text: `${Math.min(task.stepIndex + 1, task.steps.length)}/${task.steps.length}`,
          }));
        }
        // Several sessions at once: a chip opens the list of them.
        const sessions = task.sessions ?? [];
        if (sessions.length > 1) {
          who.append(h("button", { class: "sess-chip", text: `${sessions.length} sessions`, onclick: () => hooks.openDetail() }));
        } else if (sessions[0]) {
          const spent = spentLabel(sessions[0].cost, sessions[0].tokens);
          if (spent) who.append(h("span", { class: "count", text: spent }));
        }
        ticker.sync(task);
      } else if (task) {
        const info = State.integrations[task.id];
        const key = [
          task.id, detailOpen, task.state, task.steps.join("|"),
          info?.loaded, info?.error, info?.configured,
          JSON.stringify(info?.data ?? {}),
        ].join("~");
        if (key !== cardKey) {
          cardKey = key;
          mode = "card";
          clear(leftBody);
          leftBody.append(renderIntegrationCard(task, hooks));
        }
      }

      jump.style.display = detailOpen ? "none" : "";

      const others = State.otherTasks.slice(0, 4);
      const pillKey = others.map((t) => `${t.id}:${t.pillBadge ?? ""}`).join("|");
      if (pillKey !== pillIds) {
        pillIds = pillKey;
        clear(pills);
        for (const t of others) pills.append(buildPill(t, actions));
        pruneMiniBots();
      }
    },
  };
}

function buildPill(task: AgentTask, actions: ViewActions): HTMLElement {
  // Claude Code's task is named after the session's project; its pill says who
  // it is, as the Mac's does (a pill called "coucou" was Claude Code in disguise).
  const label = task.source === "claudeCode" ? "Claude Code" : task.name;
  const canvas = createMiniBot(task, 24);
  const pill = h(
    "div",
    { class: "pill", onclick: () => actions.setFocus(task.id) },
    canvas,
    h("span", { class: "lbl", text: label }),
  );
  pill.style.borderColor = `${task.color}24`;
  pill.addEventListener("mouseenter", () => {
    pill.style.background = `${task.color}2e`;
    pill.style.borderColor = `${task.color}8c`;
    pill.style.boxShadow = `0 2px 10px ${task.color}59`;
    (pill.querySelector(".lbl") as HTMLElement).style.color = lighten(task.color, 0.3);
  });
  pill.addEventListener("mouseleave", () => {
    pill.style.background = "";
    pill.style.borderColor = `${task.color}24`;
    pill.style.boxShadow = "";
    (pill.querySelector(".lbl") as HTMLElement).style.color = "";
  });

  if (task.pillBadge) {
    const colors = { approval: "#F5A524", finished: "#22C55E", error: "#F4505E" } as const;
    const icons = { approval: ICONS.bang, finished: ICONS.check, error: ICONS.xmark } as const;
    const inner = h("i", { style: `background:${colors[task.pillBadge]}` }, svg(icons[task.pillBadge], 6, { stroke: task.pillBadge === "finished" ? 3 : 0 }));
    const badge = h("div", { class: "pill-badge" }, inner);
    badge.style.boxShadow = `0 0 4px ${colors[task.pillBadge]}99`;
    pill.append(badge);
  }
  return pill;
}

function lighten(hex: string, amount: number): string {
  const v = parseInt(hex.replace("#", ""), 16);
  const c = [(v >> 16) & 255, (v >> 8) & 255, v & 255].map((x) =>
    Math.min(255, Math.round(x + amount * 255)),
  );
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}

// ── Empty ─────────────────────────────────────────────────────────────────────

function buildEmpty(actions: ViewActions): ViewHost {
  const body = h(
    "div",
    { class: "stack", style: "padding:0 18px 0 118px;flex-direction:row;align-items:center;gap:16px" },
    h(
      "div",
      { style: "display:flex;flex-direction:column;gap:5px" },
      h("div", { class: "title", text: "Nothing running right now." }),
      h("div", { class: "sub", text: "Drop a file or window, or ask me anything." }),
    ),
    h("div", { class: "grow" }),
    btn("Ask Claude", "primary", () => actions.setView("prompt")),
  );
  return { el: h("div", { class: "view" }, card(null, body)), sync() {} };
}

// ── Approval ──────────────────────────────────────────────────────────────────

/** " · 2 more waiting" while other requests queue behind the card. */
function waitingNote(): string {
  const n = State.cardQueue.length;
  return n > 0 ? ` · ${n} more waiting` : "";
}

function buildApproval(actions: ViewActions): ViewHost {
  const who = h("div");
  const code = h("div", { class: "code" });
  const row = h("div", { class: "actions" });
  const el = h("div", { class: "view" }, card("amber", stack(116, 16, who, code, row)));
  let rowKey = "";
  return {
    el,
    sync() {
      clear(who);
      who.append(agentWho(State.focusTask, `needs permission${waitingNote()}`));
      // The whole point of approving here rather than in the terminal: this line
      // is the command, the file path or the URL being authorised, not just the
      // name of the tool asking.
      code.textContent = State.pendingApproval?.command || State.pendingApproval?.tool || "…";
      // Two buttons, built once. Rebuilding them between a mouse-down and a
      // mouse-up would swallow the click, and there is nothing left to vary:
      // "Always" is gone until the remembered-rules list exists to back it.
      if (rowKey === "built") return;
      rowKey = "built";
      clear(row);
      row.append(
        btn("Deny", "secondary", () => actions.decide("deny"), "N"),
        btn("Allow", "primary", () => actions.decide("allow"), "Y"),
      );
    },
  };
}

// ── Question ──────────────────────────────────────────────────────────────────

function buildQuestion(actions: ViewActions): ViewHost {
  const who = h("div");
  const head = h("div", { class: "sub" });
  const title = h("div", { class: "title q-title" });
  const row = h("div", { class: "actions q-options" });
  const el = h("div", { class: "view" }, card("cyan", stack(116, 16, who, head, title, row)));
  let rowKey = "";
  return {
    el,
    sync() {
      const q = State.pendingQuestion;
      clear(who);
      who.append(agentWho(State.focusTask, `is asking${waitingNote()}`));
      if (!q) {
        // A question noticed in a notification only: it is answered there.
        head.textContent = "";
        title.textContent = State.focusTask?.steps.at(-1) ?? "The agent needs an answer.";
        if (rowKey !== "none") {
          rowKey = "none";
          clear(row);
          row.append(h("div", { class: "sub", text: "Answer in your terminal." }));
        }
        return;
      }
      const current = q.questions[q.index];
      const count = q.questions.length > 1 ? ` · ${q.index + 1}/${q.questions.length}` : "";
      head.textContent = `${current.header || "Question"}${count}${current.multiSelect ? " · pick any" : ""}`;
      title.textContent = current.question;
      // Rebuilt only when what it shows changes: rebuilding between a mouse
      // down and up would swallow the click.
      const key = `${q.requestId}:${q.index}:${q.picked.join("|")}`;
      if (key !== rowKey) {
        rowKey = key;
        clear(row);
        current.options.forEach((o, i) => {
          const on = q.picked.includes(o.label);
          // The digit is Alt+Shift+<digit>, as Y and N are on the permission card.
          const key = i < 9 ? String(i + 1) : undefined;
          const b = btn(o.label.slice(0, 32), current.multiSelect && !on ? "secondary" : "primary", () => actions.answer(o.label), key);
          if (o.description) b.title = o.description;
          row.append(b);
        });
        if (current.multiSelect) row.append(btn("Done", "secondary", () => actions.answer(null)));
        row.append(btn("In terminal", "secondary", () => actions.questionToTerminal()));
      }
      // Long or many answers wrap to more rows: the card grows to show them
      // all — Done and In terminal included — instead of clipping them.
      // Measured on every sync: while the card is not on screen it has no size.
      const first = row.firstElementChild as HTMLElement | null;
      if (!first || first.offsetHeight === 0) return;
      const extra = Math.min(120, Math.max(0, row.scrollHeight - first.offsetHeight));
      if (extra !== State.questionExtra) {
        State.questionExtra = extra;
        actions.refit();
      }
    },
  };
}

// ── Error ─────────────────────────────────────────────────────────────────────

function buildError(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title", text: "Workflow stopped." });
  const detail = h("div", { class: "detail" });
  // Only buttons that do something: n8n opens the instance, an agent's error
  // brings its terminal back. ("Retry" used to just close the card.)
  const openN8n = btn("Open n8n", "primary", () => actions.openTarget());
  const openTerm = btn("Open terminal", "primary", () => actions.openTerminal());
  const row = h("div", { class: "actions" }, openN8n, openTerm, btn("OK", "secondary", () => actions.collapse()));
  const el = h("div", { class: "view" }, card("red", stack(116, 16, who, title, detail, row)));
  return {
    el,
    sync() {
      const task = State.focusTask;
      const n8n = task?.source === "n8n";
      clear(who);
      who.append(agentWho(task, n8n ? "" : "stopped on an error"));
      title.textContent = n8n ? "Workflow stopped." : "Session stopped on an error.";
      detail.textContent = task?.steps.at(-1) ?? "No detail available.";
      openN8n.style.display = n8n ? "" : "none";
      openTerm.style.display = n8n ? "none" : "";
    },
  };
}

// ── Finished ──────────────────────────────────────────────────────────────────

/** "Edited a.ts, b.ts · 2 min 14 s · 312k in · 9.8k out": names while they fit, else a count. */
export function turnSummary(turn: TurnSummary | null | undefined): string {
  if (!turn) return "";
  const parts: string[] = [];
  const names = turn.files.map((f) => f.split("/").pop() || f);
  if (names.length > 0) {
    const list = names.join(", ");
    parts.push(names.length <= 3 && list.length <= 40 ? `Edited ${list}` : `Edited ${names.length} files`);
  }
  if (turn.ms !== undefined) {
    const s = Math.max(1, Math.round(turn.ms / 1000));
    if (s < 60) parts.push(`${s} s`);
    else if (s < 3600) parts.push(`${Math.floor(s / 60)} min ${s % 60} s`);
    else parts.push(`${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")} min`);
  }
  if (turn.tokensIn !== undefined && turn.tokensOut !== undefined) {
    parts.push(`${tokenCount(turn.tokensIn)} in · ${tokenCount(turn.tokensOut)} out`);
  }
  return parts.join(" · ");
}

/** 950 → "950", 9 812 → "9.8k", 312 400 → "312k", 1 340 000 → "1.3M". */
function tokenCount(n: number): string {
  if (n < 1000) return String(Math.round(n));
  if (n < 10_000) return `${(n / 1000).toFixed(1)}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

function buildFinished(actions: ViewActions): ViewHost {
  const who = h("div");
  const title = h("div", { class: "title" });
  const detail = h("div", { class: "detail" });
  const row = h("div", { class: "actions" },
    btn("Open terminal", "primary", () => actions.openTerminal()),
    btn("OK", "secondary", () => actions.collapse()),
  );
  const el = h("div", { class: "view" }, card("green", stack(116, 16, who, title, detail, row)));
  return {
    el,
    sync() {
      clear(who);
      who.append(agentWho(State.focusTask, "finished"));
      title.textContent = State.focusTask?.steps.at(-1) ?? "Session finished";
      const turn = State.focusTask?.lastTurn;
      detail.textContent = turnSummary(turn);
      detail.style.display = detail.textContent ? "" : "none";
      detail.title = turn?.tokensIn !== undefined
        ? "Tokens this turn: in = what the model read (prompt and cache), out = what it wrote"
        : "";
    },
  };
}

// ── Confused ──────────────────────────────────────────────────────────────────

function buildConfused(): ViewHost {
  const body = h(
    "div",
    { class: "stack", style: "padding:0 18px 0 128px" },
    h("div", { class: "title", text: "Too many hits at once." }),
    h("div", { class: "sub", text: "Give me a sec — back to work in three seconds." }),
  );
  return { el: h("div", { class: "view" }, card("pink", body)), sync() {} };
}

// ── Note ──────────────────────────────────────────────────────────────────────

function buildNote(): ViewHost {
  const title = h("div", { class: "title" });
  const el = h("div", { class: "view" }, card(null, h("div", { class: "stack", style: "padding:0 18px 0 98px" }, title)));
  return {
    el,
    sync() {
      title.textContent = State.noteMessage ?? "";
    },
  };
}

// ── In-island settings ────────────────────────────────────────────────────────

function buildSettings(actions: ViewActions): ViewHost {
  const soundSwitch = h("button", { class: "switch", onclick: () => actions.toggleSound() });
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.005",
    oninput: (e: Event) => actions.setVolume(Number((e.target as HTMLInputElement).value)),
  }) as HTMLInputElement;
  const autoLabel = h("span", {});
  const segButtons = [10, 15, 30].map((s) =>
    h("button", { onclick: () => actions.setAutoClose(s) }, `${s}s`),
  );
  const claudeBadge = h("span", { class: "status-badge" });
  const apiBadge = h("span", { class: "status-badge" });

  const rows = h(
    "div",
    { class: "settings-rows" },
    h("div", { class: "settings-row" }, soundSwitch, h("span", { text: "Sound" }), volume),
    h(
      "div",
      { class: "settings-row" },
      svg(ICONS.timer, 12),
      autoLabel,
      h("div", { class: "seg" }, ...segButtons),
    ),
    h(
      "div",
      { class: "settings-row", style: "gap:14px" },
      claudeBadge,
      apiBadge,
      h("div", { class: "grow" }),
      h("button", {
        class: "link-btn",
        style: "color:#8e939c;font-size:11.5px",
        text: "Settings…",
        onclick: () => actions.openSettingsWindow(),
      }),
    ),
  );

  const el = h("div", { class: "view" },
    card(null, h("div", { class: "stack", style: "padding:14px 16px 14px 84px" }, rows)));

  return {
    el,
    sync() {
      const s = State.settings;
      soundSwitch.classList.toggle("on", s.soundEnabled);
      volume.value = String(s.soundVolume);
      volume.style.opacity = s.soundEnabled ? "1" : "0.4";
      autoLabel.textContent = `Auto-close · ${Math.round(s.autoCloseInterval)}s`;
      segButtons.forEach((b, i) => b.classList.toggle("on", s.autoCloseInterval === [10, 15, 30][i]));
      clear(claudeBadge);
      claudeBadge.append(
        dot(s.hooksInstalled ? "#22C55E" : "#F4505E", 6),
        h("span", { text: "Claude Code" }),
      );
      clear(apiBadge);
      apiBadge.append(dot("#F4505E", 6), h("span", { text: "API" }));
    },
  };
}

// ── Placeholders filled in later stages ───────────────────────────────────────

function buildPlaceholder(title: string, sub: string): ViewHost {
  const body = h(
    "div",
    { class: "stack", style: "padding:0 18px 0 118px" },
    h("div", { class: "title", text: title }),
    h("div", { class: "sub", text: sub }),
  );
  return { el: h("div", { class: "view" }, card(null, body)), sync() {} };
}

// ── Registry ──────────────────────────────────────────────────────────────────

export function buildViews(
  actions: ViewActions,
  onChatHeightChange: () => void,
): Map<IslandViewName, ViewHost> {
  const map = new Map<IslandViewName, ViewHost>();
  map.set("overview", buildOverview(actions));
  map.set("empty", buildEmpty(actions));
  map.set("approval", buildApproval(actions));
  map.set("question", buildQuestion(actions));
  map.set("error", buildError(actions));
  map.set("finished", buildFinished(actions));
  map.set("confused", buildConfused());
  map.set("note", buildNote());
  map.set("settings", buildSettings(actions));
  map.set("prompt", buildPrompt(onChatHeightChange));
  map.set("upload", buildUpload());
  map.set("uploading", buildUploading());
  map.set("choose", buildChoose(actions));
  // Not in the Windows v1: sending a file by email, window attach + web result.
  map.set("mail", buildMail(actions));
  map.set("searching", buildPlaceholder("Claude is searching…", ""));
  map.set("result", buildPlaceholder("Result", ""));
  return map;
}
