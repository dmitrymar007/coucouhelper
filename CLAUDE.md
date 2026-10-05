# Coucou for Linux — guide for AI coding agents

Coucou puts Mochi, a small animated character, at the top centre of the screen. It shows AI coding agent sessions (Claude Code, opencode) and a few service integrations, and lets the user approve, answer, chat and drop files from the island. This branch is **Linux only** (Ubuntu GNOME first); the macOS and Windows apps live in the `main` branch and in the original project, [Louis-CFM/coucou](https://github.com/Louis-CFM/coucou).

## Where things are
- `src-tauri/` — the Rust backend (Tauri 2): island window, relay socket, chat backends, pollers, Secret Service. Everything desktop-specific is in `src-tauri/src/platform/linux.rs` and `platform/linux/` (`gnome_shell.rs` = the app's end of the GNOME extension, `mail_app.rs`).
- `src/` — the island and Settings front end (TypeScript, no framework). Mochi is drawn in Canvas 2D (`src/mochi/`).
- `hook/` — `coucou-hook`, the relay Claude Code and opencode run on every event.
- `gnome-extension/` — the GNOME Shell extension (`coucou@coucouhelper`) that puts the island over the top bar. The app embeds these files (`include_str!`) and installs them from Settings → GNOME.
- `opencode-plugin/coucou.js` — the opencode plugin, also embedded and installed from Settings.
- `sounds/` — the 28 WAVs, served/copied by `vite.config.ts`.
- `docs/AGENTS.md` — the hook protocol and the `coucou_agent` field.

Many source comments say "port of Foo.swift": the Swift originals are in the `main` branch (`NotchBuddy/Sources/App/`) and remain the behaviour reference.

## Build and test
```
npm install
npm run tauri dev                                   # dev build (scripts/linux-dev.sh from a VS Code snap terminal)
npm run build                                       # tsc type-check + vite build (+ the coucou-hook relay)
cargo build --release -p coucou-hook && cargo test --workspace
npm run pack                                        # AppImage, .deb, .rpm into release/
```
CI: `.github/workflows/linux.yml` (tests, packages; publishes a release on a `linux-v*` tag when `PUBLISH` is on). The version lives in `src-tauri/tauri.conf.json`, `package.json` and `Cargo.toml`, and all three must match the tag.

## Rules
- Secrets live in the Secret Service (keyring), never on disk or in git.
- No telemetry. Network calls only to services the user configured.
- Never block Claude Code: if the app doesn't answer, the hook exits immediately.
- Never overwrite `~/.claude/settings.json`: dated backup, merge, show the diff, write only after the user confirms.
- Never send an email or approve a Claude Code or opencode permission without an explicit click.
- The GNOME extension only serves a Coucou the user stands behind (root-owned `/usr/bin/coucou`, or a build the user allowed in the Shell dialog). Don't weaken that check.
- Keep the identifier `fr.louisraille.coucou` (keyring entries and settings depend on it). Pill IDs are stable contract values: never rename one.
- Don't restyle existing views unless asked; new views follow the existing style.
- No third-party dependencies unless truly unavoidable. The character is drawn in code, no images.
