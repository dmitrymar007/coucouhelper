// Entry point: boot the bridge, wire the island, start the greeting.

import "./style.css";
import { Bridge, IS_TAURI, onEvent } from "./core/bridge";
import { Sound } from "./core/sound";
import { State, type Settings } from "./core/state";
import { Island } from "./island/island";
import { registerHookHandlers } from "./island/hooks";
import { registerIntegrationHandlers, refreshConfigured } from "./island/integrations";

async function main() {
  const root = document.getElementById("root");
  if (!root) return;

  void Sound.preload();

  const island = new Island(root);

  const boot = await Bridge.boot();
  if (boot) {
    State.settings = { ...State.settings, ...boot.settings };
  }
  island.applySettings();
  State.loadIntegrationTasks();
  if (boot && !boot.cursorPoll) island.followPageCursor();

  // GNOME without the Coucou extension: say once, after the greeting, that
  // it would put the island over the top bar.
  if (boot?.shellHint) {
    window.setTimeout(() => {
      if (State.mode === "expanded" && State.view !== "greeting") return;
      State.noteMessage =
        "GNOME tip: install the Coucou extension in Settings → GNOME to put me over the top bar.";
      island.alert("note");
      void Bridge.shellHintSeen();
    }, 9000);
  }

  await onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y));
  await onEvent<{ x: number; y: number }>("cursor-far", ({ x, y }) => island.onFarCursor(x, y));
  await onEvent<null>("press-outside", () => island.pressOutside());
  await onEvent<"out" | "cancel">("window-drag", (phase) => island.onWindowDrag(phase));
  await onEvent<{ appName: string; title: string } | null>("window-picked", (w) => island.attachWindow(w));

  // Settings → Continue last chat: the conversation comes back into the island.
  await onEvent<{ role: "user" | "assistant"; content: string }[]>("chat-restored", (messages) => {
    State.chatHistory = messages.map((m, i) => ({ id: 1_000_000 + i, role: m.role, content: m.content }));
    State.droppedFile = null;
    island.alert("prompt");
  });
  // The chat backend changed: the new one starts a new conversation.
  await onEvent<null>("chat-cleared", () => {
    State.chatHistory = [];
    State.notify();
  });

  /** Pause has to reach Rust too, or the pollers keep calling out. */
  const setPaused = (on: boolean) => {
    if (State.paused === on) return;
    State.paused = on;
    void Bridge.setPaused(on);
  };

  await onEvent<string>("tray", (what) => {
    switch (what) {
      case "settings":
        setPaused(false);
        island.alert("settings");
        break;
      case "open":
        setPaused(false);
        island.alert(State.defaultView());
        break;
      case "pause":
        setPaused(!State.paused);
        if (State.paused) island.fsm.forceHidden();
        else island.reveal();
        break;
    }
  });

  await onEvent<null>("screen-changed", () => void Bridge.reposition());

  // The settings window writes preferences; apply them here without a restart.
  await onEvent<Settings>("settings-changed", (s) => {
    State.settings = { ...State.settings, ...s };
    island.applySettings();
    State.loadIntegrationTasks();
    void refreshConfigured();
  });

  registerHookHandlers(island);
  registerIntegrationHandlers(island);

  island.launch();

  // In a plain browser there is no wake strip behind the cursor: make the whole
  // page wake the island so the visuals can be checked with `npm run dev`.
  if (!IS_TAURI) {
    document.addEventListener("click", () => Sound.resume(), { once: true });
  }
}

void main();
