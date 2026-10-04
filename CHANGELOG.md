# Changelog

## Unreleased (coucouhelper, Linux)

- GNOME: a GNOME Shell extension puts the island over the top bar, like the Mac notch, keeps it out of Alt+Tab, the overview and the dock, moves the clock to the left while the island is there, lets Mochi's eyes follow the pointer everywhere and folds the island on a click elsewhere. Install or remove it in Settings → GNOME; GNOME starts it at the next login. Without it, the Xwayland window below the top bar stays as the fallback.
- GNOME on Wayland (temporary, until the GNOME Shell extension): Coucou runs through Xwayland, where the island is a dock window centred right below the top bar, with its header and wake strip in reach, instead of a window GNOME places wherever it likes. `COUCOU_X11=0` keeps native Wayland.
- The chat has a new-chat button and a list of earlier chats at the start of its input bar; pick one and the conversation comes back, ready to continue (Claude Code chats).
- Linux: Mochi's eyes follow the pointer outside the island too, wherever X11 can see it.
- Chat on your Claude subscription: the island's chat now goes through Claude Code (`claude`) by default, no API key needed. It runs in the background from your first message, with web search only — no access to your files, no edits, no commands, no MCP servers, no hooks — and answers stream in as they are written. Choose the model, when it stops, stop it by hand (Settings or the tray) or continue the last conversation in Settings → Chat. The API key stays available as the other backend.
- Linux: the island folds 2 seconds after the pointer leaves it on the overview, finished, error and note views, since a click elsewhere never reaches the app without the GNOME extension.

## 0.1.2 — October 2, 2026

- Codex support (GitHub build): sessions show up live on the Codex pill, and permission requests get Allow and Deny in the notch. Install from Settings → Codex Hooks, then trust the hooks once with /hooks in Codex (#130) — thanks @lacatu5
- Cursor: Claude Code started in Cursor's terminal shows up on the Cursor pill, and you can answer its permission requests from the notch (#120).
- Pick your main coding tool in Settings → Active pills: VS Code, Cursor, Codex or Antigravity (Codex and Antigravity: GitHub build). It stays on and no longer takes one of the 4 slots (#120).
- The permission card stays in the notch until you answer it: the mouse no longer folds it, and reopening the island shows the request again (#117).
- The permission card also shows when the island is already open, and the pill you were on comes back once you answer (#120).

## 0.1.1 — October 2, 2026

- Declare the tools you use in Settings: Gemini CLI, Antigravity, Anthropic, Google AI and OpenAI pills join the existing ones (Cursor and Codex pills are coming soon), and you pick the main pill.
- Chat now supports Google AI (Gemini) and OpenAI in addition to Anthropic; switch provider and model by clicking the model name in the chat view, on macOS.
- Linux version: the Tauri app now builds for Linux too (AppImage, .deb, .rpm), with the island as a layer-shell overlay on Wayland and Claude Code hooks over a private Unix socket (#21) — thanks @Davy133
- Compact island on screens without a notch (#22) — thanks @Kamasoutra
- Only web links (http/https) open from the notch; other kinds of links from Claude or integrations are ignored (#16) — thanks @Cris1670
- Hook socket limited to your own user account, with size and time limits; logs no longer keep commands, n8n data or full URLs, and stay under 1 MB (#16) — thanks @Cris1670 and @Vignesh-Thangamariappan
- The island always reopens after folding, and Settings opens below it, resizable — thanks @rouderz
- Choose the Claude model for the chat in Settings; the list comes from your Anthropic account, and Claude Sonnet 4.6 stays the default — thanks @rouderz
- Windows build artifacts are now downloadable from a manual CI run — thanks @MysJofR
- Any agent can talk to Mochi: tag a hook payload with `coucou_agent` (e.g. `nb-hook --agent my-agent`) and it gets its own pill in the island (#7, #9) — thanks @lacatu5
- Gemini CLI and Antigravity (agy) hook support on macOS: install from Settings and their sessions show up in the island — thanks @corefusiion
