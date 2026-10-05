// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, onEvent, type ChatContext, type ChatSession, type ChatStreamUpdate } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import { API_MODELS, CLI_MODELS } from "../core/models";
import type { ViewHost } from "./views";

let nextId = 1;

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }
  return h("div", { class: "chat-row" }, h("div", { class: "reply", text: message.content }));
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** "14:05" today, "3 Oct" before — when a saved conversation last moved. */
function when(seconds: number): string {
  const d = new Date(seconds * 1000);
  const today = new Date().toDateString() === d.toDateString();
  return today
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString([], { day: "numeric", month: "short" });
}

/** "Firefox — Page title", short enough for the chip. */
function windowLabel(app: string, title: string): string {
  const label = title && !title.includes(app) ? `${app} — ${title}` : title || app;
  return label.length > 60 ? `${label.slice(0, 59)}…` : label;
}

/** The model the chat answers with, as short as the bar allows. */
function modelLabel(): string {
  const s = State.settings;
  if (s.chatBackend === "opencode") {
    if (!s.opencodeModel) return "opencode default";
    return s.opencodeModel.slice(s.opencodeModel.indexOf("/") + 1);
  }
  if (s.chatBackend === "api-key") return API_MODELS.find(([id]) => id === s.model)?.[1] ?? s.model;
  return CLI_MODELS.find(([id]) => id === s.cliModel)?.[1] ?? s.cliModel;
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h("button", { class: "send-btn", title: "Send" }, svg(ICONS.arrowUp, 11));
  const newBtn = h("button", { class: "bar-btn", title: "New chat" }, svg(ICONS.compose, 14));
  const historyBtn = h("button", { class: "bar-btn", title: "Earlier chats" }, svg(ICONS.clock, 14));
  const modelBtn = h("button", { class: "model-btn", title: "Model" }) as HTMLButtonElement;
  const bar = h("div", { class: "chat-bar" }, newBtn, historyBtn, input, modelBtn, send);
  /** Earlier conversations, shown in place of the log. */
  const history = h("div", { class: "chat-history" });
  /** The backend's models, also shown in place of the log. */
  const models = h("div", { class: "chat-history" });

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, chipRow, log, history, models, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let historyOpen = false;
  let renderedKey = "";
  /** The answer being streamed in, once its first words have arrived. */
  let streaming: ChatMessage | null = null;

  // Claude Code streams the answer; the API sends it whole, so this never
  // fires for it and the typing dots stay until the reply lands.
  void onEvent<ChatStreamUpdate>("chat-stream", (update) => {
    if (!sending) return;
    if (update.kind === "tool") {
      State.stateOverride = "searching";
    } else {
      if (!streaming) {
        streaming = { id: nextId++, role: "assistant", content: "" };
        State.chatHistory.push(streaming);
      }
      streaming.content += update.text;
    }
    State.notify();
    onHeightChange();
  });

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    Sound.play("send");

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    // The file or window the conversation is about rides along with the first
    // message only, like ClaudeService.chat().
    const file = State.droppedFile;
    const subject = State.promptContext;
    const context: ChatContext | null =
      State.chatHistory.length !== 1
        ? null
        : file
          ? { kind: "file", name: file.name, path: file.path }
          : subject?.kind === "window"
            ? { kind: "window", appName: subject.appName, title: subject.title }
            : null;

    streaming = null;
    try {
      const reply = await Bridge.chatSend(query, context);
      if (streaming) (streaming as ChatMessage).content = reply.text;
      else State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text });
      State.stateOverride = null;
      Sound.play("finish");
    } catch (err) {
      // A half-streamed answer is not an answer.
      const partial = streaming;
      if (partial) State.chatHistory = State.chatHistory.filter((m) => m !== partial);
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      streaming = null;
      sending = false;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  function newChat() {
    if (sending) return;
    Sound.play("send");
    void Bridge.chatReset();
    State.chatHistory = [];
    State.droppedFile = null;
    State.promptContext = null;
    showHistory(false);
    State.notify();
    onHeightChange();
    input.focus();
  }

  function historyRow(session: ChatSession): HTMLElement {
    const count = session.messages == null ? "" : ` · ${session.messages} messages`;
    const meta = `${when(session.modified)}${count}${session.current ? " · open" : ""}`;
    const row = h(
      "button",
      { class: session.current ? "history-row current" : "history-row" },
      h("span", { class: "history-title", text: session.title }),
      h("span", { class: "history-meta", text: meta }),
    );
    row.addEventListener("click", async () => {
      if (session.current) {
        showHistory(false);
        return;
      }
      try {
        // The messages come back as `chat-restored`, which refills the log.
        await Bridge.chatResume(session.id);
        showHistory(false);
      } catch (err) {
        State.noteMessage = String(err).replace(/^Error:\s*/, "");
        State.view = "note";
        State.notify();
      }
    });
    return row;
  }

  let modelsOpen = false;

  /** One model to pick; the chosen one is marked. */
  function modelRow(id: string, label: string, chosen: boolean, pick: () => void): HTMLElement {
    const row = h(
      "button",
      { class: chosen ? "history-row current" : "history-row" },
      h("span", { class: "history-title", text: label }),
      h("span", { class: "history-meta", text: chosen ? "in use" : id }),
    );
    row.addEventListener("click", () => {
      pick();
      void Bridge.saveSettings(State.settings);
      Sound.play("blip");
      void showModels(false);
      State.notify();
    });
    return row;
  }

  async function showModels(open: boolean) {
    modelsOpen = open;
    if (open && historyOpen) await showHistory(false);
    modelBtn.classList.toggle("on", open);
    log.style.display = open ? "none" : "";
    models.style.display = open ? "" : "none";
    if (!open) {
      input.focus();
      return;
    }
    clear(models);
    const s = State.settings;
    if (s.chatBackend === "claude-code") {
      for (const [id, label] of CLI_MODELS) {
        models.append(modelRow(id, label, s.cliModel === id, () => (s.cliModel = id)));
      }
      return;
    }
    if (s.chatBackend === "api-key") {
      for (const [id, label] of API_MODELS) {
        models.append(modelRow(id, label, s.model === id, () => (s.model = id)));
      }
      return;
    }
    models.append(modelRow("", "opencode's default", !s.opencodeModel, () => (s.opencodeModel = "")));
    const list = (await Bridge.chatOpencodeModels()) ?? [];
    if (!modelsOpen) return;
    // Grouped by provider, the user's own first; OpenCode Zen last.
    const providers = [...new Set(list.map((m) => m.slice(0, m.indexOf("/"))))].sort((a, b) =>
      a === "opencode" ? 1 : b === "opencode" ? -1 : a.localeCompare(b),
    );
    for (const provider of providers) {
      models.append(h("div", { class: "history-empty", text: provider === "opencode" ? "OpenCode Zen" : provider }));
      for (const id of list.filter((m) => m.startsWith(`${provider}/`)).sort()) {
        models.append(modelRow(id, id.slice(provider.length + 1), s.opencodeModel === id, () => (s.opencodeModel = id)));
      }
    }
  }
  models.style.display = "none";
  modelBtn.addEventListener("click", () => void showModels(!modelsOpen));

  async function showHistory(open: boolean) {
    if (open && modelsOpen) await showModels(false);
    historyOpen = open;
    historyBtn.classList.toggle("on", open);
    log.style.display = open ? "none" : "";
    history.style.display = open ? "" : "none";
    if (!open) {
      input.focus();
      return;
    }
    clear(history);
    const sessions = (await Bridge.chatSessions()) ?? [];
    if (!historyOpen) return;
    clear(history);
    if (sessions.length === 0) {
      history.append(h("div", { class: "history-empty", text: "No earlier chats yet." }));
    }
    for (const s of sessions) history.append(historyRow(s));
  }
  history.style.display = "none";

  newBtn.addEventListener("click", () => newChat());
  historyBtn.addEventListener("click", () => void showHistory(!historyOpen));
  send.addEventListener("click", () => void submit());
  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      void submit();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const subject = State.promptContext;
      const wantChip = file?.name ?? (subject?.kind === "window" ? windowLabel(subject.appName, subject.title) : "");
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const waiting =
        (State.stateOverride === "thinking" || State.stateOverride === "searching") && !streaming;
      const last = State.chatHistory[State.chatHistory.length - 1];
      const key = `${State.chatHistory.length}:${waiting}:${last?.id ?? 0}:${last?.content.length ?? 0}`;
      if (key !== renderedKey) {
        renderedKey = key;
        clear(log);
        for (const m of State.chatHistory) log.append(bubble(m));
        if (waiting) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      input.placeholder = State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      const label = modelLabel();
      if (modelBtn.textContent !== label) modelBtn.textContent = label;
      modelBtn.toggleAttribute("disabled", sending);
      if (modelsOpen && sending) void showModels(false);
      input.disabled = sending;
      newBtn.toggleAttribute("disabled", sending);
      // Only Claude Code and opencode keep conversations to come back to.
      historyBtn.style.display = State.settings.chatBackend === "api-key" ? "none" : "";
      if (historyOpen && (sending || State.settings.chatBackend === "api-key")) void showHistory(false);
    },
    focus() {
      input.focus();
      input.select();
    },
  };
}
