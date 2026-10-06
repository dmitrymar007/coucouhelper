# Changelog

Coucou for Linux.

## Unreleased

- A question or permission request from an agent that is not on screen opens its card at once, on that agent's pill, and the pill you were on comes back after you answer (as on the Mac). It used to show only a badge.
- Claude Code's pill is called Claude Code, not after the session's project.

## 0.2.1 — October 5, 2026

- A question or permission answered in Claude Code's own window (VS Code) takes the island's card down too; it used to stay until clicked.
- No more frozen, eyeless Mochi behind the live one after making it dizzy (three clicks) while a card was up.

## 0.2.0 — October 5, 2026

- Linux only: the macOS and Windows apps are no longer part of this project (they stay in the `main` branch).
- The .deb brings the GNOME extension with it: after installing, Settings → GNOME → Turn on extension and a new login are all it takes. An older copy in your own extensions folder is replaced by the package's.
- GNOME extension: only a Coucou you stand behind may drive the island. A packaged install is trusted as is; any other build (a cargo build, an AppImage) makes GNOME Shell ask you once per session. Before, any program could copy a binary under the name coucou and see your pointer and windows.
- Questions from an agent are answered on the island: Claude Code's AskUserQuestion and opencode's question tool show their options as buttons (one question after the other, several picks where allowed), or go back to the terminal. Before, Allow on such a card would have let Claude Code run the question with no answer.
- Several sessions of one agent: a chip in its card lists them — project, state, last step — and brings the chosen session's terminal back. Each session shows the context its last answer used, and opencode sessions their cost.
- The opencode pill stays up while its plugin is installed and shows its steps like Claude Code's; it used to show "Key not configured" and vanish after each answer.
- Chat: the model is picked from the input bar; a PDF reaches any opencode model as its text (pdftotext); with the GNOME extension, the camera button asks about the window you were in from a screenshot; answers show code blocks and can be copied.
- The island's error card has no dead buttons any more, the Claude Code pill is called Claude Code instead of VS Code, and a pill badged for a waiting request opens it.
- Send by email works to the end: the file is really attached in Thunderbird (snap included) and other mail apps, and the card after a drop shows Ask a question and Send by email side by side instead of one long button.
- opencode sessions on the island: Settings → opencode → Install plugin. Each session gets its own pill (prompt, tools, done, failed) and its permission requests an approval card with Allow and Deny; answering in opencode first takes the card down.
- An approval card now goes away as soon as the question is answered in the terminal, instead of staying up until it times out.
- The chat can answer through opencode: Settings → Chat → Answers from → opencode, then any model from the providers set up in opencode (OpenCode Zen, OpenRouter, OpenAI-compatible endpoints…). Web search only, as with Claude Code; the clock lists earlier opencode chats to continue.
- GNOME: started at login before GNOME has loaded its extensions, Coucou waits for the Coucou extension instead of falling back below the top bar; minimizing a window or switching workspaces no longer brings the old window back.
- Send a dropped file by email again: through Resend (API key and the new Sender field), or into your mail app with the file attached.
- With the GNOME extension: "Open terminal" brings back the terminal or editor window the session runs in, the chat takes the window you were in as its context, and dragging Mochi onto a window attaches it to a new chat.
- GNOME: a GNOME Shell extension puts the island over the top bar, like the Mac notch, keeps it out of Alt+Tab, the overview and the dock, moves the clock to the left while the island is there, lets Mochi's eyes follow the pointer everywhere and folds the island on a click elsewhere. Install or remove it in Settings → GNOME; GNOME starts it at the next login. Without it, the Xwayland window below the top bar stays as the fallback.
- GNOME on Wayland (temporary, until the GNOME Shell extension): Coucou runs through Xwayland, where the island is a dock window centred right below the top bar, with its header and wake strip in reach, instead of a window GNOME places wherever it likes. `COUCOU_X11=0` keeps native Wayland.
- The chat has a new-chat button and a list of earlier chats at the start of its input bar; pick one and the conversation comes back, ready to continue (Claude Code chats).
- Linux: Mochi's eyes follow the pointer outside the island too, wherever X11 can see it.
- Chat on your Claude subscription: the island's chat now goes through Claude Code (`claude`) by default, no API key needed. It runs in the background from your first message, with web search only — no access to your files, no edits, no commands, no MCP servers, no hooks — and answers stream in as they are written. Choose the model, when it stops, stop it by hand (Settings or the tray) or continue the last conversation in Settings → Chat. The API key stays available as the other backend.
- Linux: the island folds 2 seconds after the pointer leaves it on the overview, finished, error and note views, since a click elsewhere never reaches the app without the GNOME extension.

## Earlier

Versions up to 0.1.2 were released together with the macOS and Windows apps; their history is in the `main` branch and in the original project, [Louis-CFM/coucou](https://github.com/Louis-CFM/coucou/blob/main/CHANGELOG.md).
