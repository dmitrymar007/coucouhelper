// Thin wrapper over the Tauri commands/events. Every call is a no-op when the
// page is opened in a plain browser, so the island can be iterated on with
// `npm run dev` alone.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { Settings } from "./state";

export const IS_TAURI =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!IS_TAURI) return null;
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    console.error(`[coucou] ${cmd} failed`, err);
    return null;
  }
}

export interface BootInfo {
  settings: Settings;
  /** Logical screen rect of the monitor the island lives on. */
  screen: { x: number; y: number; width: number; height: number; scale: number };
  version: string;
  hookPath: string;
  /** False where the OS has no global cursor (Wayland): see Island.followPageCursor. */
  cursorPoll: boolean;
  /** GNOME without the Coucou extension, and the user not told yet. */
  shellHint: boolean;
}

/** Another app's window: its name and title. */
export interface WindowInfo {
  appName: string;
  title: string;
}

/** Where the GNOME Shell extension stands (Linux). */
/** Where Coucou's own updates stand (packaged installs). */
export interface UpdateStatus {
  applicable: boolean;
  current: string;
  latest: string | null;
  available: boolean;
  checkedAt: number | null;
  checking: boolean;
  installing: boolean;
  error: string | null;
  notes: string;
}

export interface ShellExtensionStatus {
  applicable: boolean;
  installed: boolean;
  packaged: boolean;
  upToDate: boolean;
  enabled: boolean;
  userExtensionsDisabled: boolean;
  active: boolean;
}

export const Bridge = {
  boot: () => call<BootInfo>("boot"),

  saveSettings: (settings: Settings) => call<void>("save_settings", { settings }),

  /** Shrink the window down to the invisible wake strip (hidden) or back to full. */
  setCollapsed: (collapsed: boolean) => call<void>("set_collapsed", { collapsed }),
  /** Card shortcuts to grab desktop-wide (GNOME extension); [] lets them go. */
  setCardKeys: (keys: string[]) => call<void>("set_card_keys", { keys }),

  /**
   * Pushes the island shape in window coordinates. Rust flips click-through from
   * its own cursor poll, so the flag is never a frame behind a click.
   */
  setIslandRect: (x: number, y: number, width: number, height: number) =>
    call<void>("set_island_rect", { x, y, width, height }),

  /** Give the window keyboard focus (chat field) and take it away again. */
  focusWindow: (focused: boolean) => call<void>("focus_window", { focused }),

  reposition: () => call<void>("reposition"),

  openUrl: (url: string) => call<void>("open_url", { url }),

  /** "Open terminal" → opens the folder in VS Code when `code` is on PATH. */
  openInVSCode: (path: string | null) => call<boolean>("open_in_vscode", { path }),
  /** "Open terminal": the session's own window (GNOME extension), else VS Code. */
  focusTerminal: (pids: number[], cwd: string | null) => call<boolean>("focus_terminal", { pids, cwd }),
  /** The window the user was in before the island (GNOME extension). */
  windowContext: () => call<WindowInfo | null>("window_context"),
  /** A press on Mochi that may become a drag onto another window. */
  windowDrag: (on: boolean) => call<void>("window_drag", { on }),

  quit: () => call<void>("quit_app"),

  openSettingsWindow: () => call<void>("open_settings_window"),

  /** Writes to %LOCALAPPDATA%\Coucou\coucou.log, next to the Rust lines. */
  log: (message: string) => call<void>("log_line", { message }),

  // ── Claude Code hooks ─────────────────────────────────────────────────────
  hooksStatus: () => call<HookStatus>("hooks_status"),
  /** Diff to show before anything is written. `install: false` previews removal. */
  hooksPreview: (install: boolean) => callOrThrow<HookPreview>("hooks_preview", { install }),
  /**
   * Writes ~/.claude/settings.json — only ever after an explicit click, and only
   * when the file still matches the preview the user looked at.
   */
  hooksApply: (install: boolean, fingerprint: string) =>
    callOrThrow<string>("hooks_apply", { install, fingerprint }),

  approvalDecision: (requestId: string, decision: "allow" | "deny") =>
    call<void>("approval_decision", { requestId, decision }),
  /** "The card is up" — until this lands the relay only waits a moment. */
  approvalAck: (requestId: string) => call<void>("approval_ack", { requestId }),
  /** "Nobody can act on this" — Claude Code asks in the terminal right away. */
  approvalDecline: (requestId: string) => call<void>("approval_decline", { requestId }),
  /** The question card's answer: question text → chosen label(s). */
  approvalAnswer: (requestId: string, answers: Record<string, string | string[]>) =>
    call<void>("approval_answer", { requestId, answers }),

  // ── Chat, files, secrets ──────────────────────────────────────────────────
  /** One chat turn. The API key and any file bytes never leave Rust. */
  chatSend: (query: string, context: ChatContext | null) =>
    callOrThrow<{ text: string }>("chat_send", { query, context }),
  chatReset: () => call<void>("chat_reset"),
  /** Ends the Claude Code process and any opencode answer being written. */
  chatStop: () => call<void>("chat_stop"),
  chatStatus: () => call<ChatStatus>("chat_status"),
  /** Whether `claude` is installed and signed in. */
  chatInstall: () => call<ClaudeInstall>("chat_install"),
  /** Whether `opencode` is installed, and its version. */
  chatOpencodeInstall: () => call<OpencodeInstall>("chat_opencode_install"),
  /** Every "provider/model" opencode can use. */
  chatOpencodeModels: () => call<string[]>("chat_opencode_models"),
  /** Hands the last Claude Code or opencode conversation back to the island. */
  chatResumeLast: () => callOrThrow<number>("chat_resume_last"),
  // ── opencode plugin ──────────────────────────────────────────────────────
  opencodePluginStatus: () => call<OpencodePluginStatus>("opencode_plugin_status"),
  /** Only from an explicit click in Settings. */
  opencodePluginInstall: () => callOrThrow<OpencodePluginStatus>("opencode_plugin_install"),
  /** Only from an explicit click in Settings. */
  opencodePluginRemove: () => callOrThrow<OpencodePluginStatus>("opencode_plugin_remove"),
  // ── GNOME Shell extension ────────────────────────────────────────────────
  shellExtensionStatus: () => call<ShellExtensionStatus>("shell_extension_status"),
  updateStatus: () => call<UpdateStatus>("update_status"),
  updateCheck: () => callOrThrow<UpdateStatus>("update_check"),
  /** Downloads, checks and installs the newer release; Coucou then restarts. */
  updateInstall: () => callOrThrow<void>("update_install"),
  /** Only from an explicit click in Settings. */
  shellExtensionInstall: () => callOrThrow<ShellExtensionStatus>("shell_extension_install"),
  /** Only from an explicit click in Settings. */
  shellExtensionRemove: () => callOrThrow<ShellExtensionStatus>("shell_extension_remove"),
  shellHintSeen: () => call<void>("shell_hint_seen"),
  /** Recent conversations started from the island on the current backend, newest first. */
  chatSessions: () => call<ChatSession[]>("chat_sessions"),
  /** Picks one of them up again; its messages arrive as `chat-restored`. */
  chatResume: (session: string) => callOrThrow<number>("chat_resume", { session }),
  /** The mail view's Send button: through Resend, or into the user's mail app. */
  mailSend: (to: string, subject: string, body: string, file: string | null) =>
    callOrThrow<"resend" | "mailApp">("mail_send", { to, subject, body, file }),
  /** The chat's camera: the window the user was in, as a PNG in the inbox. */
  captureWindow: () => callOrThrow<DroppedFile>("capture_window"),
  /** Copies a dropped file into the inbox. */
  ingestFile: (path: string) => callOrThrow<DroppedFile>("ingest_file", { path }),
  /** Only ever tells you whether a key exists — never its value. */
  secretPresent: (key: string) => call<boolean>("secret_present", { key }),
  secretSet: (key: string, value: string) => callOrThrow<void>("secret_set", { key, value }),
  secretClear: (key: string) => callOrThrow<void>("secret_clear", { key }),

  // ── Integrations ──────────────────────────────────────────────────────────
  refreshIntegration: (id: string) => call<void>("refresh_integration", { id }),
  /** Opens the configured n8n instance in the browser. */
  openN8n: () => call<void>("open_n8n"),

  /** Tray → Pause. Stops the integration pollers, not just the island. */
  setPaused: (paused: boolean) => call<void>("set_paused", { paused }),
};

export interface IntegrationUpdate {
  id: string;
  data: Record<string, unknown>;
  error: string | null;
  event: { success: boolean; label: string; detail: string | null } | null;
}

/** A piece of the answer while it is still coming (Claude Code and opencode chats). */
export type ChatStreamUpdate = { kind: "text"; text: string } | { kind: "tool"; name: string };

export interface ChatSession {
  id: string;
  title: string;
  /** Seconds since 1970. */
  modified: number;
  /** Unknown for opencode conversations. */
  messages: number | null;
  current: boolean;
}

export interface ChatStatus {
  running: boolean;
  sessionId: string | null;
}

export interface ClaudeInstall {
  path: string | null;
  loggedIn: boolean;
  authMethod: string | null;
}

export interface OpencodePluginStatus {
  /** opencode itself is installed. */
  opencode: boolean;
  installed: boolean;
  packaged: boolean;
  upToDate: boolean;
  path: string;
}

export interface OpencodeInstall {
  path: string | null;
  version: string | null;
}

export type ChatContext =
  | { kind: "file"; name: string; path: string }
  | { kind: "window"; appName: string; title: string; url?: string };

export interface DroppedFile {
  name: string;
  path: string;
  size: number;
}

export interface HookStatus {
  installed: boolean;
  settingsPath: string;
  hookPath: string;
  hookReady: boolean;
}

export interface HookPreview {
  diff: string;
  backup: string;
  settingsPath: string;
  /** Hand back to hooksApply so only the reviewed diff is ever written. */
  fingerprint: string;
}

/** Same as `call`, but surfaces the error so the UI can show what went wrong. */
async function callOrThrow<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!IS_TAURI) throw new Error("not running inside Coucou");
  return invoke<T>(cmd, args);
}

export type BridgeEvent =
  | { name: "cursor"; payload: { x: number; y: number } }
  | { name: "cursor-far"; payload: { x: number; y: number } }
  | { name: "press-outside"; payload: null }
  | { name: "window-drag"; payload: "out" | "cancel" }
  | { name: "window-picked"; payload: WindowInfo | null }
  | { name: "chat-stream"; payload: ChatStreamUpdate }
  | { name: "chat-restored"; payload: { role: "user" | "assistant"; content: string }[] }
  | { name: "chat-cleared"; payload: null }
  | { name: "tray"; payload: string }
  | { name: "hook"; payload: Record<string, unknown> }
  | { name: "screen-changed"; payload: null };

export interface DragDropPayload {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
}

/** Files dragged onto the island. Only reaches us when the window takes the mouse. */
export async function onDragDrop(handler: (e: DragDropPayload) => void) {
  if (!IS_TAURI) return () => {};
  return getCurrentWebview().onDragDropEvent((event) => {
    handler(event.payload as DragDropPayload);
  });
}

export async function onEvent<T>(name: string, handler: (payload: T) => void) {
  if (!IS_TAURI) return () => {};
  return listen<T>(name, (e) => handler(e.payload));
}
