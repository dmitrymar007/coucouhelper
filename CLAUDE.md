# Coucou — guide for AI coding agents

Coucou is a native macOS app (`NotchBuddy/`); `windows/` is the Tauri version for Windows and Linux. Mochi, a small animated character living in the MacBook notch, shows AI coding agent sessions (Claude Code, Gemini CLI, Antigravity and more) and a few integrations, and lets the user approve, answer, chat and drop files from the notch.

## Where things are
- `NotchBuddy/Sources/App/` — all Swift code. `NotchBuddy/Resources/sounds/` — the 28 WAV sounds. `NotchBuddy/project.yml` — XcodeGen project (never edit the `.xcodeproj` by hand).
- `NotchBuddy/Sources/App/PillCatalog.swift` — single source of truth for all declared pills (workspace tools, agents, AI providers, services). Every pill ID, color, category and subtitle lives here.
- `docs/SPEC.md`, `docs/INTEGRATIONS.md` — behaviour, views, states, integrations (in French).
- `design/prototype/notch-buddy.html` — original prototype, the visual source of truth. `design/captures/` — target screenshots.
- `windows/` — the Tauri app for Windows and Linux: Rust in `src-tauri/`, TypeScript in `src/`, the `coucou-hook` relay in `hook/`. `windows/README.md` lists what differs from the Mac.
- `docs/*.html` — the GitHub Pages site (privacy, terms, support, legal notice).

## Build and test
```
cd NotchBuddy && xcodegen && xcodebuild -scheme NotchBuddy -configuration Debug build
bash scripts/test-screen-geometry.sh   # swiftc-compiles IslandScreenGeometry.swift + tests/IslandScreenGeometryTests.swift
bash scripts/test-safe-links.sh        # same for SafeWebURL.swift + tests/SafeWebURLTests.swift
```
There is no XCTest target: tests are standalone executables built with `swiftc` from one source file plus its test file, so a file under test must stay free of dependencies on the rest of the app. CI (`.github/workflows/build.yml`) runs both scripts, then a Release build with signing disabled.

Windows and Linux: `cd windows && npm install && npm run tauri dev` (`npm run build` type-checks with `tsc` and builds the `coucou-hook` relay first; `npm run pack` makes the installer).

## Architecture notes
- `project.yml` defines two targets sharing `Sources/`: `NotchBuddy` (GitHub build, Developer ID, bundle ID `fr.louisraille.NotchBuddy`, no sandbox) and `CoucouAppStore` (bundle ID `fr.louisraille.Coucou`, built with the `APPSTORE` compilation flag). Code that differs is gated with `#if APPSTORE`, so check both targets when touching `HookServer`, `PillCatalog`, `SettingsView`, `IslandRootView`, `IslandWindowController`, `IslandViewContent` or `WindowContextCapture`.
- Agents talk to the app through a hook relay over a Unix socket (macOS, Linux) or named pipe (Windows), handled by `HookServer.swift` on the Mac and `src-tauri/src/{hooks,pipe}.rs` + `hook/` on Windows/Linux. The optional `coucou_agent` field routes an event to a pill (`docs/AGENTS.md`).
- The Mac and Windows/Linux apps are parallel implementations, and the Windows side is written as a port: `windows/src/island/fsm.ts` ports `IslandStateMachine.swift`, and `windows/src/island/integrations.ts` ports the pollers' `handle…` methods. A behaviour change usually needs both, unless `windows/README.md` lists it as a known difference.
- Each service integration is a `*Poller.swift` (Stripe, GitHub, Vercel, Notion, n8n, Resend, Cal.com) plus a `.service` entry in `PillCatalog.swift`.
- Releases: `scripts/release.sh <version>` (macOS: needs a `## <version>` section in `CHANGELOG.md` and `CFBundleShortVersionString` in `project.yml` to match). Windows releases come from `windows-v*` tags and require the same version in `tauri.conf.json`, `package.json` and `Cargo.toml`.

## Rules
- Swift 6, SwiftUI + AppKit. No third-party dependencies unless truly unavoidable. The character is drawn in code (`Canvas` + `TimelineView`), no Rive/Lottie/images.
- Secrets live in the Keychain, never on disk or in git.
- No telemetry. Network calls only to services the user configured.
- Never block Claude Code: if the app doesn't answer, the hook exits immediately.
- Never overwrite `~/.claude/settings.json`: dated backup, merge, show the diff, write only after the user confirms.
- Never send an email or approve a Claude Code or Codex permission without an explicit click.
- Performance: 0 % CPU when the island is hidden.
- Keep the bundle identifier `fr.louisraille.NotchBuddy` (Keychain items, preferences and permissions depend on it).
- Never restyle what already ships (pills, cards, Settings, chat…): existing views stay exactly as they are in `main`, which is the App Store build. Change the look of an existing view only when explicitly asked.
- Pill IDs are stable contract values (Keychain, UserDefaults, hook routing): never rename an existing pill ID.
- New views follow the existing app style. `design/prototype/notch-buddy.html` and `design/captures/` are references for new work, not a reason to change existing views.
