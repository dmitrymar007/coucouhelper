// Coucou for Windows — app wiring and the commands the island calls.

mod chat;
mod claude;
mod claude_cli;
mod opencode_cli;
mod opencode_plugin;
mod files;
mod hooks;
mod integrations;
mod island;
mod log;
mod mail;
mod pipe;
mod platform;
mod secrets;
mod settings;
mod tray;

use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};

use chat::Chat;
use claude::{ChatContext, ChatReply};
use files::DroppedFile;
use hooks::{HookPreview, HookStatus};
use island::{PollGate, ScreenInfo};
use pipe::Pending;
use settings::Settings;

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub gate: Arc<PollGate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    settings: Settings,
    screen: ScreenInfo,
    version: String,
    hook_path: String,
    /// False where click-through is the input region (Linux): the page then
    /// tracks hover from its own mouse events.
    cursor_poll: bool,
    /// GNOME without the Coucou extension, and the user not told yet.
    shell_hint: bool,
}

#[tauri::command]
fn boot(app: AppHandle, shared: State<Shared>) -> BootInfo {
    let mut settings = shared.settings.lock().unwrap().clone();
    // The real state of ~/.claude/settings.json wins over whatever we stored.
    settings.hooks_installed = hooks::status().installed;
    let screen = island::screen_info(&app, &settings.screen);
    let shell_hint = platform::shell_extension_missing() && !settings.shell_hint_shown;
    BootInfo {
        settings,
        screen,
        version: env!("CARGO_PKG_VERSION").to_string(),
        hook_path: settings::hook_exe_path().to_string_lossy().to_string(),
        cursor_poll: !platform::CLICK_THROUGH_BY_REGION,
        shell_hint,
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<Shared>, chat: State<Chat>, mut settings: Settings) {
    let (screen_changed, autostart_changed, backend_changed) = {
        let mut current = shared.settings.lock().unwrap();
        let screen_changed = current.screen != settings.screen;
        let autostart_changed = current.autostart != settings.autostart;
        let backend_changed = current.chat_backend != settings.chat_backend;
        // Rust keeps these up to date; a window's copy of them may be stale.
        settings.last_chat_session = current.last_chat_session.clone();
        settings.shell_hint_shown = current.shell_hint_shown;
        *current = settings.clone();
        (screen_changed, autostart_changed, backend_changed)
    };
    // The other backend knows nothing of this conversation: start a new one.
    if backend_changed {
        chat.reset();
        let _ = app.emit_to(island::WINDOW_LABEL, "chat-cleared", ());
    }
    if let Err(err) = settings::save(&settings) {
        eprintln!("[coucou] could not save settings: {err}");
    }
    if autostart_changed {
        let manager = app.autolaunch();
        let result = if settings.autostart { manager.enable() } else { manager.disable() };
        if let Err(err) = result {
            eprintln!("[coucou] autostart: {err}");
        }
    }
    if screen_changed {
        let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
        island::apply_geometry(&app, &settings.screen, collapsed);
    }
    // Keep the other window in step (island ⇄ settings window).
    let _ = app.emit("settings-changed", settings);
}

/// Hidden island → shrink the window to the invisible wake strip and park the
/// cursor poll; anything else → full panel and 60 Hz polling.
#[tauri::command]
fn set_collapsed(app: AppHandle, shared: State<Shared>, collapsed: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    shared.gate.collapsed.store(collapsed, Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
    // The wake strip must always take the mouse, and a resize invalidates the flag.
    island::refresh_click_through(&app, &shared.gate);
    shared.gate.set_active(!collapsed);
}

/// The front end pushes the island shape; Rust decides click-through from it.
#[tauri::command]
fn set_island_rect(app: AppHandle, shared: State<Shared>, x: f64, y: f64, width: f64, height: f64) {
    shared.gate.set_rect(island::IslandRect { x, y, w: width, h: height });
    // Without the cursor poll the input region is the click-through: it follows the island.
    if platform::CLICK_THROUGH_BY_REGION {
        island::refresh_click_through(&app, &shared.gate);
    }
}

#[tauri::command]
fn focus_window(app: AppHandle, focused: bool) {
    let Some(win) = island::window(&app) else { return };
    platform::set_activating(&win, focused);
    if focused {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn reposition(app: AppHandle, shared: State<Shared>) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
}

#[tauri::command]
fn open_url(url: String) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    platform::open_url(&url);
}

/// "Open terminal" opens the working folder in VS Code when `code` is on PATH,
/// and falls back to the file manager otherwise.
#[tauri::command]
fn open_in_vscode(path: Option<String>) -> bool {
    // No shell anywhere near this. The path is a project folder chosen by
    // whoever is using Claude Code, and a shell would happily read `&`, `^`, `%`
    // or `$` in a folder name as syntax. Finding the launcher ourselves and
    // handing the path over as a separate argument keeps it a path.
    let path = path.filter(|p| !p.is_empty());
    // It arrives in a hook payload: only an existing folder, given by its full
    // path, goes any further. `code` would read `--something` as an option, and
    // xdg-open would launch a file with whatever handles its type.
    if let Some(p) = path.as_deref() {
        let p = std::path::Path::new(p);
        if !(p.is_absolute() && p.is_dir()) {
            return false;
        }
    }
    if let Some(code) = platform::find_on_path("code") {
        let mut cmd = Command::new(code);
        if let Some(p) = path.as_deref() {
            cmd.arg(p);
        }
        if platform::no_console(&mut cmd).spawn().is_ok() {
            return true;
        }
    }
    if let Some(p) = path.as_deref() {
        platform::reveal_folder(p);
    }
    false
}

/// "Open terminal": the window of the terminal or editor the session runs in
/// (GNOME extension), else the folder in VS Code as before. `pids` are the
/// session's ancestor processes, nearest first, as the relay reported them.
#[tauri::command]
fn focus_terminal(pids: Vec<u32>, cwd: Option<String>) -> bool {
    let hint = cwd
        .as_deref()
        .and_then(|c| std::path::Path::new(c).file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let pids: Vec<u32> = pids.into_iter().filter(|&p| p > 1).take(12).collect();
    if !pids.is_empty() && platform::activate_window_of(&pids, &hint) {
        return true;
    }
    open_in_vscode(cwd)
}

/// The window the user was in before the island, for the chat's context.
#[tauri::command]
fn window_context() -> Option<island::WindowContext> {
    platform::last_focused_window().map(|(app_name, title)| island::WindowContext { app_name, title })
}

/// A press on Mochi that may turn into a drag onto another window.
#[tauri::command]
fn window_drag(on: bool) {
    island::arm_window_drag(on);
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Tray → Pause. Paused means paused: the pollers stop talking to the network,
/// not just the island stopping showing things.
#[tauri::command]
fn set_paused(paused: bool) {
    integrations::set_paused(paused);
}

// ── Claude Code hooks ─────────────────────────────────────────────────────────

#[tauri::command]
fn hooks_status() -> HookStatus {
    hooks::status()
}

/// Returns the diff the user has to look at before anything is written.
#[tauri::command]
fn hooks_preview(install: bool) -> Result<HookPreview, String> {
    hooks::preview(install)
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn hooks_apply(
    app: AppHandle,
    shared: State<Shared>,
    install: bool,
    fingerprint: String,
) -> Result<String, String> {
    // The fingerprint comes from the preview the user actually looked at, so a
    // settings.json that changed in between is refused rather than overwritten.
    let backup = hooks::write(install, &fingerprint)?;
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        current.hooks_installed = install;
        let _ = settings::save(&current);
        current.clone()
    };
    let _ = app.emit("settings-changed", updated);
    Ok(backup)
}

#[tauri::command]
fn approval_decision(app: AppHandle, request_id: String, decision: String) {
    pipe::answer(&app, &request_id, &decision);
}

/// The island has the card on screen, so the long wait for a human may begin.
/// Until this arrives the relay only waits a few hundred milliseconds, which is
/// what stops a paused or unresponsive island from freezing Claude Code.
/// The question card's answer: question text → chosen label(s).
#[tauri::command]
fn approval_answer(app: AppHandle, request_id: String, answers: serde_json::Value) {
    pipe::answer_question(&app, &request_id, &answers);
}

#[tauri::command]
fn approval_ack(app: AppHandle, request_id: String) {
    pipe::acknowledge(&app, &request_id);
}

/// Nobody can act on this request — the island is paused, or another card is
/// already up. Claude Code falls back to asking in the terminal immediately.
#[tauri::command]
fn approval_decline(app: AppHandle, request_id: String) {
    pipe::decline(&app, &request_id);
}

// ── Chat, files and secrets ───────────────────────────────────────────────────

/// One chat turn. The API key and any file bytes stay on the Rust side; the
/// answer streams to the island as `chat-stream` events while it comes.
#[tauri::command]
async fn chat_send(
    app: AppHandle,
    shared: State<'_, Shared>,
    chat: State<'_, Chat>,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let turn = chat::TurnSettings::from(&*shared.settings.lock().unwrap());
    let stream = app.clone();
    let sent = chat
        .send(turn, query, context, move |update| {
            let _ = stream.emit_to(island::WINDOW_LABEL, "chat-stream", update);
        })
        .await?;
    if let Some(session) = sent.session {
        remember_chat_session(&app, &shared, Some(session));
    }
    Ok(sent.reply)
}

/// Stores the conversation to offer again after a restart, and tells the
/// settings window.
fn remember_chat_session(app: &AppHandle, shared: &Shared, session: Option<String>) {
    let updated = {
        let mut current = shared.settings.lock().unwrap();
        if current.last_chat_session == session {
            return;
        }
        current.last_chat_session = session;
        let _ = settings::save(&current);
        current.clone()
    };
    let _ = app.emit("settings-changed", updated);
}

#[tauri::command]
fn chat_reset(chat: State<Chat>) {
    chat.reset();
}

/// Settings → Stop now, and the tray: the next message starts it again.
#[tauri::command]
fn chat_stop(chat: State<Chat>) {
    chat.stop();
}

#[tauri::command]
fn chat_status(shared: State<Shared>, chat: State<Chat>) -> claude_cli::CliStatus {
    chat.status(current_backend(&shared))
}

fn current_backend(shared: &Shared) -> chat::Backend {
    chat::Backend::from_setting(&shared.settings.lock().unwrap().chat_backend)
}

/// Is `claude` installed and signed in? Asked by the settings window.
#[tauri::command]
async fn chat_install() -> claude_cli::Install {
    chat::install_status().await
}

/// The Coucou plugin for opencode: installed, up to date?
#[tauri::command]
fn opencode_plugin_status() -> opencode_plugin::Status {
    opencode_plugin::status()
}

/// Only from an explicit click in Settings.
#[tauri::command]
fn opencode_plugin_install() -> Result<opencode_plugin::Status, String> {
    opencode_plugin::install()
}

/// Only from an explicit click in Settings.
#[tauri::command]
fn opencode_plugin_remove() -> Result<opencode_plugin::Status, String> {
    opencode_plugin::remove()
}

/// Is `opencode` installed, and which version? Asked by the settings window.
#[tauri::command]
async fn chat_opencode_install() -> opencode_cli::Install {
    opencode_cli::install_status().await
}

/// The models opencode can use, for the settings window's list.
#[tauri::command]
async fn chat_opencode_models() -> Vec<String> {
    opencode_cli::models().await
}

/// Settings → Continue last chat: the island gets the conversation back and
/// the next message resumes it.
#[tauri::command]
async fn chat_resume_last(app: AppHandle, shared: State<'_, Shared>, chat: State<'_, Chat>) -> Result<usize, String> {
    let session = shared
        .settings
        .lock()
        .unwrap()
        .last_chat_session
        .clone()
        .ok_or("No saved conversation yet.")?;
    // The next message goes to the current backend, so the conversation must be its own.
    let opencode_session = opencode_cli::is_session_id(&session);
    match current_backend(&shared) {
        chat::Backend::OpenCode if !opencode_session => return Err("The last chat was with Claude Code: switch the chat back to it to continue.".into()),
        chat::Backend::ClaudeCode if opencode_session => return Err("The last chat was with opencode: switch the chat back to it to continue.".into()),
        chat::Backend::ApiKey => return Err("Conversations are kept by Claude Code and opencode, not with an API key.".into()),
        _ => {}
    }
    resume_chat(&app, &chat, &session).await
}

/// The chat's history button: one of the conversations from `chat_sessions`.
#[tauri::command]
async fn chat_resume(app: AppHandle, chat: State<'_, Chat>, session: String) -> Result<usize, String> {
    resume_chat(&app, &chat, &session).await
}

async fn resume_chat(app: &AppHandle, chat: &Chat, session: &str) -> Result<usize, String> {
    let messages = chat.resume(session).await?;
    let count = messages.len();
    let _ = app.emit_to(island::WINDOW_LABEL, "chat-restored", messages);
    Ok(count)
}

/// Recent conversations for the chat's history list, the live one marked.
#[tauri::command]
async fn chat_sessions(shared: State<'_, Shared>, chat: State<'_, Chat>) -> Result<Vec<ChatSessionRow>, String> {
    let backend = current_backend(&shared);
    let current = chat.status(backend).session_id;
    Ok(chat
        .sessions(backend, 12)
        .await
        .into_iter()
        .map(|info| ChatSessionRow { current: current.as_deref() == Some(info.id.as_str()), info })
        .collect())
}

#[derive(Serialize)]
struct ChatSessionRow {
    #[serde(flatten)]
    info: claude_cli::SessionInfo,
    current: bool,
}

/// The Send button of the mail view, and nothing else.
#[tauri::command]
async fn mail_send(to: String, subject: String, body: String, file: Option<String>) -> Result<mail::Sent, String> {
    let result = mail::send(to, subject, body, file).await;
    if let Err(err) = &result {
        log::line(format!("mail: not sent — {err}"));
    }
    result
}

/// Copies a dropped file into the inbox and reports its name back.
#[tauri::command]
fn ingest_file(path: String) -> Result<DroppedFile, String> {
    files::ingest(&path)
}

/// The island may only ask whether a key exists — never read it.
#[tauri::command]
fn secret_present(key: String) -> bool {
    secrets::present(&key)
}

#[tauri::command]
fn secret_set(key: String, value: String) -> Result<(), String> {
    secrets::set(&key, &value)
}

#[tauri::command]
fn secret_clear(key: String) -> Result<(), String> {
    secrets::clear(&key)
}

/// Opens the configured n8n instance — the URL lives in the Credential Manager.
#[tauri::command]
fn open_n8n() {
    if let Some(url) = secrets::get("n8n-url") {
        open_url(url);
    }
}

/// Refresh buttons in the integration cards.
#[tauri::command]
async fn refresh_integration(app: AppHandle, id: String) {
    integrations::poll_once(app, &id).await;
}

// ── GNOME Shell extension ─────────────────────────────────────────────────────

#[tauri::command]
fn shell_extension_status() -> platform::ShellExtensionStatus {
    platform::shell_extension_status()
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn shell_extension_install() -> Result<platform::ShellExtensionStatus, String> {
    platform::shell_extension_install()?;
    Ok(platform::shell_extension_status())
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn shell_extension_remove() -> Result<platform::ShellExtensionStatus, String> {
    platform::shell_extension_remove()?;
    Ok(platform::shell_extension_status())
}

/// The island showed its one-time note about the extension.
#[tauri::command]
fn shell_hint_seen(shared: State<Shared>) {
    let mut current = shared.settings.lock().unwrap();
    current.shell_hint_shown = true;
    let _ = settings::save(&current);
}

/// Lets the island write to the same log as the Rust side.
#[tauri::command]
fn log_line(message: String) {
    log::line(format!("ui  {message}"));
}

// ── Settings window ───────────────────────────────────────────────────────────

/// WebView2 allows exactly one browser environment per app, and its options are
/// fixed by whichever webview is created first. Every window must therefore ask
/// for the *same* arguments as the island (see `additionalBrowserArgs` in
/// tauri.conf.json) — a mismatch makes the second window come up blank, with no
/// error anywhere.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// In a dev build the pages are served by Vite, so the second window needs the
/// absolute dev URL; a bundled build resolves it inside the app bundle.
fn settings_page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/settings.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("settings.html".into())
}

/// The settings window is created hidden at launch and only ever shown and
/// hidden afterwards. A WebView2 window created later — on the main thread or
/// not — silently comes up blank in this app, so the window that works is the
/// one that exists before the island's webview does.
fn create_settings_window(app: &AppHandle) {
    let url = settings_page_url(app);
    match WebviewWindowBuilder::new(app, "settings", url)
        .additional_browser_args(BROWSER_ARGS)
        .title("Settings — Coucou")
        .inner_size(560.0, 680.0)
        .min_inner_size(460.0, 480.0)
        .resizable(true)
        .visible(false)
        .center()
        .build()
    {
        Ok(win) => {
            // Closing it must only hide it, or it could never be reopened.
            let hidden = win.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hidden.hide();
                }
            });
        }
        Err(err) => log::line(format!("settings window failed: {err}")),
    }
}

pub fn show_settings_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("settings") else {
        log::line("settings window missing");
        return;
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn open_settings_window(app: AppHandle) {
    show_settings_window(&app);
}

pub fn run() {
    platform::prepare_environment();
    let loaded = settings::load();
    let gate = Arc::new(PollGate::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let _ = app.emit_to(island::WINDOW_LABEL, "tray", "open".to_string());
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .manage(Shared {
            settings: Mutex::new(loaded.clone()),
            gate: gate.clone(),
        })
        .manage(Pending::default())
        .manage(Chat::default())
        .invoke_handler(tauri::generate_handler![
            boot,
            save_settings,
            set_collapsed,
            set_island_rect,
            focus_window,
            reposition,
            open_url,
            open_in_vscode,
            focus_terminal,
            window_context,
            window_drag,
            quit_app,
            hooks_status,
            hooks_preview,
            hooks_apply,
            approval_decision,
            approval_answer,
            approval_ack,
            approval_decline,
            log_line,
            chat_send,
            chat_reset,
            chat_stop,
            chat_status,
            chat_install,
            chat_opencode_install,
            chat_opencode_models,
            opencode_plugin_status,
            opencode_plugin_install,
            opencode_plugin_remove,
            chat_resume_last,
            chat_resume,
            chat_sessions,
            shell_extension_status,
            shell_extension_install,
            shell_extension_remove,
            shell_hint_seen,
            ingest_file,
            mail_send,
            secret_present,
            secret_set,
            secret_clear,
            refresh_integration,
            open_n8n,
            open_settings_window,
            set_paused,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;
            // Before the island: see create_settings_window.
            create_settings_window(&handle);

            if let Some(win) = island::window(&handle) {
                platform::make_non_activating(&win);
                island::apply_geometry(&handle, &loaded.screen, false);
                let _ = win.show();
            }
            gate.collapsed.store(false, Ordering::Relaxed);
            // Nothing drawn yet, so nothing takes the mouse until the page
            // reports the island's shape.
            if platform::CLICK_THROUGH_BY_REGION {
                island::refresh_click_through(&handle, &gate);
            }
            gate.set_active(true);
            island::spawn_cursor_poll(handle.clone(), gate.clone());

            // GNOME: the Coucou extension places the island over the top bar
            // and reports the pointer; it comes and goes (screen lock).
            let shell_app = handle.clone();
            let shell_gate = gate.clone();
            platform::shell_extension_start(move |event| match event {
                platform::ShellEvent::Active(on) => {
                    log::line(format!("shell extension {}", if on { "active" } else { "inactive" }));
                    let pref = shell_app.state::<Shared>().settings.lock().unwrap().screen.clone();
                    let collapsed = shell_gate.collapsed.load(Ordering::Relaxed);
                    island::apply_geometry(&shell_app, &pref, collapsed);
                    island::refresh_click_through(&shell_app, &shell_gate);
                    platform::pointer_watch(!collapsed);
                }
                platform::ShellEvent::Pointer { x, y, pressed } => {
                    island::shell_pointer(&shell_app, &shell_gate, x, y, pressed);
                }
            });

            log::line(format!("--- Coucou {} started ---", env!("CARGO_PKG_VERSION")));
            hooks::ensure_hook_exe(&handle);
            pipe::start(handle.clone());
            integrations::start(handle.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Coucou");
}
