// Linux: XDG directories for files, xdg-open for links and folders, and
// gtk-layer-shell for the island window.
//
// Wayland gives an app no global cursor position and no say over where its
// window goes, so the island works differently from Windows:
//   * it is a layer-shell surface anchored to the top edge, above everything,
//     on compositors that support it (COSMIC, KDE, wlroots — not GNOME);
//   * click-through is the window's input region, set to the island shape, so
//     the compositor itself sends every other click to whatever is underneath;
//   * the cursor comes from the page's own mouse events, which only fire over
//     the island — Mochi's eyes follow the pointer there, not across the screen.
//
// On GNOME (no layer-shell) the app runs through Xwayland for now — a
// temporary measure until the GNOME Shell extension: the island is then an X11
// dock window at the top edge, and X11 also tells Mochi's eyes where the
// pointer is, as long as it is over an X11 window.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use tauri::{AppHandle, WebviewWindow};

use super::{home_dir, LocalTime, ShellEvent};

mod gnome_shell;
mod mail_app;

/// File name of the Claude Code relay.
pub const HOOK_EXE: &str = "coucou-hook";

/// Environment variable holding the home directory.
pub const HOME_VAR: &str = "HOME";

// ── Files ─────────────────────────────────────────────────────────────────────

/// An XDG base directory (`$XDG_CONFIG_HOME` …), or its fallback under the home
/// directory when it is unset or not absolute.
fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(fallback))
}

/// ~/.config/coucou — preferences.
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("coucou")
}

/// ~/.local/share/coucou — where coucou-hook, the inbox and the log live. The
/// relay has to sit at a stable path: an AppImage is mounted somewhere new on
/// every launch.
pub fn local_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("coucou")
}

/// Environment the webview must inherit, set before any thread or process
/// starts.
///
/// Inside an AppImage, WebKit uses the GStreamer bundled with it, and GStreamer
/// keeps its plugin registry in ~/.cache/gstreamer-1.0 by default — the same
/// file the system's GStreamer uses. The AppImage is mounted somewhere new on
/// every launch, so each launch would rewrite the system's registry with
/// plugin paths that vanish once Coucou quits. Give ours its own file.
pub fn prepare_environment() {
    if shell_extension_wanted() {
        let mut found = gnome_shell::on_bus();
        // Started at login, Coucou can come up before GNOME has loaded its
        // extensions. If GNOME is going to start ours, wait for it rather than
        // settle for the Xwayland fallback for the whole run.
        if !found && gnome_shell::expected() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !found && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(250));
                found = gnome_shell::on_bus();
            }
        }
        SHELL_EXTENSION.store(found, Ordering::Relaxed);
        crate::log::line(format!("GNOME extension at launch: {}", if found { "running" } else { "not running" }));
    }
    prefer_x11_on_gnome();
    X11.store(gdk_backend_is_x11(), Ordering::Relaxed);

    if std::env::var_os("APPIMAGE").is_none() || std::env::var_os("GST_REGISTRY").is_some() {
        return;
    }
    let cache = xdg("XDG_CACHE_HOME", ".cache").join("coucou");
    if std::fs::create_dir_all(&cache).is_ok() {
        std::env::set_var("GST_REGISTRY", cache.join("gstreamer-registry.bin"));
    }
}

/// True when GTK runs on X11 (Xwayland included): the island can then be
/// placed by coordinates and the cursor read anywhere X11 can see it.
static X11: AtomicBool = AtomicBool::new(false);

/// TEMPORARY (until the GNOME Shell extension): GNOME's Wayland session has no
/// layer-shell, so a native Wayland island is an ordinary window the
/// compositor places wherever it likes, under the top bar. Through Xwayland it
/// can be a dock window: pinned to the top edge by coordinates, above other
/// windows. `COUCOU_X11=0` keeps native Wayland; an explicit `GDK_BACKEND`
/// always wins.
fn prefer_x11_on_gnome() {
    // The extension does everything Xwayland was for, and more.
    if SHELL_EXTENSION.load(Ordering::Relaxed) {
        return;
    }
    let wanted = std::env::var("COUCOU_X11").map(|v| v != "0").unwrap_or(true);
    let gnome = desktop_is_gnome(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default());
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let xwayland = std::env::var_os("DISPLAY").is_some();
    if wanted && gnome && wayland && xwayland && std::env::var_os("GDK_BACKEND").is_none() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
}

/// True when the Coucou GNOME Shell extension was running at launch: the app
/// then stays on native Wayland and lets the extension place the island.
static SHELL_EXTENSION: AtomicBool = AtomicBool::new(false);

/// GNOME on Wayland, unless `COUCOU_SHELL_EXTENSION=0`.
fn shell_extension_wanted() -> bool {
    std::env::var("COUCOU_SHELL_EXTENSION").map(|v| v != "0").unwrap_or(true)
        && desktop_is_gnome(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn shell_extension_status() -> super::ShellExtensionStatus {
    if !desktop_is_gnome(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()) {
        return Default::default();
    }
    gnome_shell::status()
}

/// Brings the session's terminal (or editor) window to the front.
pub fn activate_window_of(pids: &[u32], hint: &str) -> bool {
    gnome_shell::active() && gnome_shell::activate_window_of(pids, hint)
}

/// (app, window title) the user was in before opening the island.
pub fn last_focused_window() -> Option<(String, String)> {
    if !gnome_shell::active() {
        return None;
    }
    gnome_shell::last_focused_window()
}

/// A screenshot of the window the user was in, saved to `path`: (app, title).
pub fn capture_window(path: &str) -> Option<(String, String)> {
    if !gnome_shell::active() {
        return None;
    }
    gnome_shell::capture_window(path)
}

/// (app, window title) under a point relative to the island. Main thread.
pub fn window_at(x: f64, y: f64, done: impl FnOnce(Option<(String, String)>) + 'static) {
    if !gnome_shell::active() {
        return done(None);
    }
    gnome_shell::window_at(x, y, done)
}

pub fn shell_extension_install() -> Result<(), String> {
    gnome_shell::install()
}

pub fn shell_extension_remove() -> Result<(), String> {
    gnome_shell::remove()
}

/// True on GNOME on Wayland when the extension was not running at launch:
/// the island then lives below the top bar, and the extension would help.
pub fn shell_extension_missing() -> bool {
    shell_extension_wanted() && !SHELL_EXTENSION.load(Ordering::Relaxed)
}

/// Starts following the GNOME Shell extension, if it was there at launch.
/// Main thread only.
pub fn shell_extension_start(on_event: impl Fn(ShellEvent) + 'static) {
    if SHELL_EXTENSION.load(Ordering::Relaxed) && !X11.load(Ordering::Relaxed) {
        gnome_shell::start(on_event);
    }
}

/// While the island is open the extension reports the pointer; hidden, it
/// reports nothing and nothing runs.
pub fn pointer_watch(on: bool) {
    if gnome_shell::active() {
        gnome_shell::set_tracking(on);
    }
}

/// The monitor the island lives on, for the extension to centre it there.
pub fn place_island(screen: &str) {
    if gnome_shell::active() {
        gnome_shell::set_placement(screen);
    }
}

/// With the extension the window never shrinks to the wake strip: resizing a
/// window the compositor places would make it jump for a frame. The strip is
/// then only the input region, in the middle of the full-size window.
pub fn island_keeps_full_size() -> bool {
    gnome_shell::active()
}

/// `XDG_CURRENT_DESKTOP` is a colon-separated list, `ubuntu:GNOME` on Ubuntu.
fn desktop_is_gnome(desktops: &str) -> bool {
    desktops.split(':').any(|d| d.eq_ignore_ascii_case("GNOME"))
}

/// What GTK will pick: the first backend in `GDK_BACKEND`, else Wayland when
/// a Wayland display exists, else X11.
fn gdk_backend_is_x11() -> bool {
    backend_is_x11(
        std::env::var("GDK_BACKEND").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    )
}

fn backend_is_x11(gdk_backend: Option<&str>, wayland: bool, x11: bool) -> bool {
    match gdk_backend.map(str::trim) {
        Some(list) if !list.is_empty() && list != "*" => {
            list.split(',').next().map(str::trim) == Some("x11")
        }
        _ => !wayland && x11,
    }
}

pub fn local_time() -> LocalTime {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        libc::localtime_r(&now, &mut tm);
    }
    LocalTime {
        year: (tm.tm_year + 1900) as u32,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    }
}

/// Creates `dir` and closes it to other users. The log, the inbox of dropped
/// files and the relay binary live under these directories; with the default
/// umask they would come out 0755 and readable by anyone on the machine.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// True when `dir` is a real directory (not a symlink), owned by us, with no
/// access for group or others: what `$XDG_RUNTIME_DIR` promises, checked
/// rather than assumed, since the socket in it decides who can answer a
/// permission request.
fn is_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir)
        .map(|m| {
            m.file_type().is_dir() && m.uid() == unsafe { libc::getuid() } && m.mode() & 0o077 == 0
        })
        .unwrap_or(false)
}

/// Where coucou-hook finds us: `$XDG_RUNTIME_DIR/coucou.sock`, or
/// `/run/user/<uid>/coucou.sock` when the variable is missing. A directory
/// that is not ours and private means no relay at all — never a fallback to a
/// shared place like /tmp. Must match `socket_path()` in hook/src/unix.rs
/// exactly.
pub fn relay_socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    is_private_dir(&dir).then(|| dir.join("coucou.sock"))
}

// ── Processes ─────────────────────────────────────────────────────────────────

/// Nothing to hide: a spawned process only gets a terminal if it asks for one.
pub fn no_console(cmd: &mut Command) -> &mut Command {
    cmd
}

pub fn open_url(url: &str) {
    let _ = Command::new("xdg-open").arg(url).spawn();
}

/// The user's mail app with the email written and the file attached, via
/// xdg-email. They send it themselves. No shell: every part is one argument.
pub fn compose_email(to: &str, subject: &str, body: &str, attachment: Option<&Path>) -> Result<(), String> {
    let app = mail_app::find();
    // The mail app must be able to read the file: a sandboxed one gets a copy
    // in its own folder (see mail_app.rs).
    let sandbox = app.as_ref().map_or(mail_app::Sandbox::None, |a| a.sandbox.clone());
    let attachment = attachment.map(|f| mail_app::stage(f, &sandbox)).transpose()?;

    if let Some(app) = app.filter(|a| a.thunderbird) {
        let (program, args) = app.exec.split_first().ok_or("No mail app.")?;
        Command::new(program)
            .args(args)
            .arg("-compose")
            .arg(mail_app::thunderbird_compose(to, subject, body, attachment.as_deref()))
            .spawn()
            .map_err(|e| format!("Could not open Thunderbird: {e}"))?;
        return Ok(());
    }

    let exe = find_on_path("xdg-email").ok_or("No mail app helper (xdg-email) found. Set up Resend in Settings instead.")?;
    let mut cmd = Command::new(exe);
    cmd.arg("--subject").arg(subject);
    if !body.is_empty() {
        cmd.arg("--body").arg(body);
    }
    if let Some(file) = &attachment {
        cmd.arg("--attach").arg(file);
    }
    cmd.arg(to);
    cmd.spawn().map(|_| ()).map_err(|e| format!("Could not open the mail app: {e}"))
}

pub fn reveal_folder(path: &str) {
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Our own `which`: the first executable file named `stem` on $PATH.
pub fn find_on_path(stem: &str) -> Option<PathBuf> {
    let dirs = std::env::var_os("PATH")?;
    std::env::split_paths(&dirs)
        .map(|dir| dir.join(stem))
        .find(|p| {
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

// ── Cursor ────────────────────────────────────────────────────────────────────

/// Click-through is the input region, set to the island shape, never a
/// per-tick decision from the cursor: under Xwayland the X server stops seeing
/// the pointer once it is over a native Wayland window, so an island that
/// ignored the mouse could never learn that the pointer came back. The page
/// reports hover from its own mouse events (see the top of this file).
pub const CLICK_THROUGH_BY_REGION: bool = true;

/// Whether `cursor_physical` reads anything. On X11 it does, for Mochi's eyes
/// only — and under Xwayland only while the pointer is over an X11 window.
pub fn cursor_poll() -> bool {
    X11.load(Ordering::Relaxed)
}

/// Cursor position in physical screen pixels, from X11.
pub fn cursor_physical() -> Option<(f64, f64)> {
    if !cursor_poll() {
        return None;
    }
    xlib::pointer()
}

pub fn left_button_down() -> bool {
    false
}

/// The one Xlib call we need, on a connection of our own: GDK's belongs to the
/// main thread and the cursor poll runs on another. libX11 is already loaded
/// by GTK's X11 backend.
mod xlib {
    use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};
    use std::sync::Mutex;

    type Display = c_void;
    type Window = c_ulong;

    #[link(name = "X11")]
    extern "C" {
        fn XOpenDisplay(name: *const c_char) -> *mut Display;
        fn XDefaultRootWindow(display: *mut Display) -> Window;
        fn XQueryPointer(
            display: *mut Display,
            w: Window,
            root_return: *mut Window,
            child_return: *mut Window,
            root_x: *mut c_int,
            root_y: *mut c_int,
            win_x: *mut c_int,
            win_y: *mut c_int,
            mask: *mut c_uint,
        ) -> c_int;
    }

    /// The connection, opened on first use. Xlib is not thread-safe without
    /// XInitThreads, so every call goes through this lock. `Some(0)` means
    /// opening failed and is not retried.
    static DISPLAY: Mutex<Option<usize>> = Mutex::new(None);

    pub fn pointer() -> Option<(f64, f64)> {
        let mut guard = DISPLAY.lock().ok()?;
        let display =
            *guard.get_or_insert_with(|| unsafe { XOpenDisplay(std::ptr::null()) } as usize) as *mut Display;
        if display.is_null() {
            return None;
        }
        let (mut root, mut child): (Window, Window) = (0, 0);
        let (mut x, mut y, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0);
        let on_screen = unsafe {
            XQueryPointer(
                display,
                XDefaultRootWindow(display),
                &mut root,
                &mut child,
                &mut x,
                &mut y,
                &mut wx,
                &mut wy,
                &mut mask,
            )
        };
        (on_screen != 0).then_some((x as f64, y as f64))
    }
}

// ── Island window ─────────────────────────────────────────────────────────────

/// The few gtk-layer-shell calls we need, straight from the C library.
mod layer {
    use gtk::ffi::GtkWindow;
    use std::os::raw::{c_char, c_int};

    pub const LAYER_OVERLAY: c_int = 3;
    pub const EDGE_TOP: c_int = 2;
    pub const KEYBOARD_NONE: c_int = 0;
    pub const KEYBOARD_ON_DEMAND: c_int = 2;

    #[link(name = "gtk-layer-shell")]
    extern "C" {
        pub fn gtk_layer_is_supported() -> c_int;
        pub fn gtk_layer_init_for_window(window: *mut GtkWindow);
        pub fn gtk_layer_set_namespace(window: *mut GtkWindow, name_space: *const c_char);
        pub fn gtk_layer_set_layer(window: *mut GtkWindow, layer: c_int);
        pub fn gtk_layer_set_anchor(window: *mut GtkWindow, edge: c_int, anchor: c_int);
        pub fn gtk_layer_set_exclusive_zone(window: *mut GtkWindow, zone: c_int);
        pub fn gtk_layer_set_keyboard_mode(window: *mut GtkWindow, mode: c_int);
    }
}

/// True once the island window is a layer-shell surface.
static LAYER_SURFACE: AtomicBool = AtomicBool::new(false);

/// The input region last asked for, re-applied whenever the window is mapped:
/// GTK resets it to the whole window on map. Until the page reports the island
/// shape it is empty, so nothing takes the mouse.
type Region = Option<(f64, f64, f64, f64)>;
static INPUT_REGION: Mutex<Region> = Mutex::new(Some((0.0, 0.0, 0.0, 0.0)));

fn gtk_window_ptr(win: &gtk::ApplicationWindow) -> *mut gtk::ffi::GtkWindow {
    let w: &gtk::Window = win.upcast_ref();
    w.to_glib_none().0
}

/// WebKitGTK has no competing drop target to remove.
pub fn unblock_webview_drops(_app: &AppHandle) {}

/// Last inset worked out on the main thread, for calls from anywhere else.
static TOP_INSET: Mutex<i32> = Mutex::new(0);

/// Physical pixels between the top of the monitor at `pos`/`size` and its work
/// area. On X11 under GNOME that is the top bar: GNOME Shell draws it over
/// every window, docks included, so an island at the very edge would sit
/// beneath it, header and wake strip out of reach. TEMPORARY, like the
/// Xwayland mode itself: the GNOME Shell extension will put the island over
/// the bar. Zero everywhere else — a layer surface already sits over the panel.
pub fn top_inset(pos: (i32, i32), size: (u32, u32)) -> i32 {
    if !X11.load(Ordering::Relaxed) {
        return 0;
    }
    // GDK belongs to the main thread.
    if !gtk::is_initialized_main_thread() {
        return *TOP_INSET.lock().unwrap();
    }
    let Some(display) = gtk::gdk::Display::default() else { return 0 };
    let inset = (0..display.n_monitors())
        .filter_map(|i| display.monitor(i))
        .find_map(|m| {
            let (g, sf) = (m.geometry(), m.scale_factor());
            let same = (g.x() * sf, g.y() * sf) == pos
                && (g.width() * sf, g.height() * sf) == (size.0 as i32, size.1 as i32);
            same.then(|| work_area_top(g.y(), m.workarea().y(), g.height()) * sf)
        })
        .unwrap_or(0);
    *TOP_INSET.lock().unwrap() = inset;
    inset
}

/// How far the work area starts below the monitor's top, in GDK units; a
/// nonsensical work area counts as none.
fn work_area_top(monitor_y: i32, workarea_y: i32, monitor_h: i32) -> i32 {
    let inset = workarea_y - monitor_y;
    if (0..monitor_h / 4).contains(&inset) { inset } else { 0 }
}

/// Turns the island into an overlay surface on the top edge that never takes
/// the keyboard. Must run before the window is first shown: a layer surface
/// cannot be made out of a window the compositor already knows.
///
/// Without layer-shell (GNOME, X11, or COUCOU_LAYER_SHELL=0) the window stays
/// an always-on-top window that refuses focus. On X11 it is a dock window,
/// which window managers (Mutter included) place exactly where asked, top
/// edge included; on Wayland where it lands is up to the compositor.
pub fn make_non_activating(win: &WebviewWindow) {
    let Ok(gw) = win.gtk_window() else { return };
    // COUCOU_LAYER_SHELL=0 is the way out on a compositor where it misbehaves.
    let wanted = std::env::var("COUCOU_LAYER_SHELL").map(|v| v != "0").unwrap_or(true);
    let supported = unsafe { layer::gtk_layer_is_supported() } != 0;
    if !wanted || !supported || gw.is_realized() {
        let why = if !wanted {
            "COUCOU_LAYER_SHELL=0"
        } else if supported {
            "window already shown"
        } else {
            "compositor has no layer-shell"
        };
        gw.set_accept_focus(false);
        // A normal window is pushed below GNOME's top bar; a dock is not. The
        // hint has to be set before the window is first mapped.
        if X11.load(Ordering::Relaxed) && !gw.is_realized() {
            gw.set_type_hint(gtk::gdk::WindowTypeHint::Dock);
            crate::log::line(format!("island is an X11 dock window ({why})"));
        } else {
            crate::log::line(format!("island is a regular window ({why})"));
        }
        return;
    }
    // tao gives undecorated Wayland windows an empty titlebar to force
    // client-side decorations. A layer surface has none, and a client-decorated
    // GtkWindow recomputes its own input region (shadow margins included) on
    // every map, over ours.
    gw.set_titlebar(None::<&gtk::Widget>);
    let ptr = gtk_window_ptr(&gw);
    unsafe {
        layer::gtk_layer_init_for_window(ptr);
        layer::gtk_layer_set_namespace(ptr, c"coucou".as_ptr());
        layer::gtk_layer_set_layer(ptr, layer::LAYER_OVERLAY);
        // Top edge only: the compositor centres the surface horizontally.
        layer::gtk_layer_set_anchor(ptr, layer::EDGE_TOP, 1);
        // -1: sit right against the screen edge, over any top panel, the way
        // the Mac island sits in the notch.
        layer::gtk_layer_set_exclusive_zone(ptr, -1);
        layer::gtk_layer_set_keyboard_mode(ptr, layer::KEYBOARD_NONE);
    }
    // WebKitGTK in a freshly mapped layer surface never paints its first frame
    // (seen on COSMIC, and reproduced with a bare GTK window + WebKitGTK, no
    // Tauri involved): the surface stays empty. Unmapping and mapping it once,
    // right after the first map, gets it drawing for good.
    let remapped = std::cell::Cell::new(false);
    gw.connect_map_event(move |w, _| {
        apply_input_region(w, *INPUT_REGION.lock().unwrap());
        if !remapped.replace(true) {
            let w = w.clone();
            gtk::glib::idle_add_local_once(move || {
                w.hide();
                w.show_all();
                apply_input_region(&w, *INPUT_REGION.lock().unwrap());
            });
        }
        gtk::glib::Propagation::Proceed
    });
    LAYER_SURFACE.store(true, Ordering::Relaxed);
    crate::log::line("island is a layer-shell overlay");
}

/// Temporarily allow keyboard focus so a text field inside the island can be
/// typed in.
pub fn set_activating(win: &WebviewWindow, activating: bool) {
    let Ok(gw) = win.gtk_window() else { return };
    // The island is created `focusable: false` (tauri.linux.conf.json), so GTK
    // refuses focus until we say otherwise — on a layer surface too.
    gw.set_accept_focus(activating);
    // Under the extension, Wayland has no say: the extension hands the
    // keyboard to the island, or keeps it away.
    if gnome_shell::active() {
        gnome_shell::set_focusable(activating);
    }
    if LAYER_SURFACE.load(Ordering::Relaxed) {
        let mode = if activating { layer::KEYBOARD_ON_DEMAND } else { layer::KEYBOARD_NONE };
        unsafe { layer::gtk_layer_set_keyboard_mode(gtk_window_ptr(&gw), mode) };
    }
}

/// Only this rectangle (window-logical pixels) takes the mouse; `None` means
/// the whole window does. Everything outside goes to the window underneath.
pub fn set_input_region(win: &WebviewWindow, rect: Region) {
    *INPUT_REGION.lock().unwrap() = rect;
    let Ok(gw) = win.gtk_window() else { return };
    apply_input_region(&gw, rect);
}

fn apply_input_region(gw: &impl IsA<gtk::Widget>, rect: Region) {
    match rect {
        None => gw.input_shape_combine_region(None),
        Some((x, y, w, h)) => {
            let Some(gdk_window) = gw.window() else { return };
            let region = gtk::cairo::Region::create_rectangle(&gtk::cairo::RectangleInt::new(
                x.floor() as i32,
                y.floor() as i32,
                w.ceil().max(0.0) as i32,
                h.ceil().max(0.0) as i32,
            ));
            gdk_window.input_shape_combine_region(&region, 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_is_found_in_the_desktop_list() {
        assert!(desktop_is_gnome("ubuntu:GNOME"));
        assert!(desktop_is_gnome("GNOME"));
        assert!(desktop_is_gnome("gnome-classic:GNOME"));
        assert!(!desktop_is_gnome("KDE"));
        assert!(!desktop_is_gnome("COSMIC"));
        assert!(!desktop_is_gnome("Hyprland"));
        assert!(!desktop_is_gnome(""));
    }

    #[test]
    fn the_inset_is_the_top_bar_and_nothing_absurd() {
        assert_eq!(work_area_top(0, 32, 1200), 32);
        assert_eq!(work_area_top(1200, 1200, 1080), 0);
        assert_eq!(work_area_top(1200, 1232, 1080), 32);
        assert_eq!(work_area_top(0, -5, 1200), 0);
        assert_eq!(work_area_top(0, 600, 1200), 0);
    }

    #[test]
    fn the_first_gdk_backend_decides() {
        assert!(backend_is_x11(Some("x11"), true, true));
        assert!(backend_is_x11(Some("x11,wayland"), true, true));
        assert!(!backend_is_x11(Some("wayland,x11"), true, true));
        assert!(!backend_is_x11(Some("wayland"), false, true));
        // Unset or "*": GTK prefers Wayland when there is one.
        assert!(!backend_is_x11(None, true, true));
        assert!(!backend_is_x11(Some("*"), true, true));
        assert!(backend_is_x11(None, false, true));
        assert!(!backend_is_x11(None, false, false));
    }

    #[test]
    fn only_a_private_directory_of_ours_can_hold_the_relay_socket() {
        let base = std::env::temp_dir().join(format!("coucou-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("runtime");
        std::fs::create_dir_all(&dir).unwrap();
        let set = |mode| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();

        set(0o700);
        assert!(is_private_dir(&dir));

        // Readable or reachable by group or others: no.
        for open in [0o750, 0o705, 0o755, 0o777, 0o1777] {
            set(open);
            assert!(!is_private_dir(&dir), "{open:o} must be refused");
        }

        // A symlink to a private directory: no, the link itself is what we got.
        set(0o700);
        let link = base.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!is_private_dir(&link));

        // Missing: no.
        assert!(!is_private_dir(&base.join("missing")));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn private_dirs_are_closed_to_everyone_else() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("coucou-priv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
