// The app's end of the Coucou GNOME Shell extension (windows/gnome-extension).
//
// GNOME has no layer-shell; the extension does what layer-shell would: it
// pins the island at the top centre over the top bar, keeps it out of Alt+Tab
// and the overview, keeps the keyboard away from it, and reports the pointer
// anywhere on screen while the island is open. We find it by its D-Bus name,
// register our window with it, and follow it coming and going — GNOME
// disables extensions while the screen is locked.
//
// Everything here runs on the GTK main thread, where gio delivers the D-Bus
// callbacks; the public functions hop there from wherever they are called.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use gtk::gio::{self, prelude::*};
use gtk::glib::{self, ToVariant};

const BUS_NAME: &str = "org.coucouhelper.Shell";
const OBJECT_PATH: &str = "/org/coucouhelper/Shell";
const IFACE: &str = "org.coucouhelper.Shell1";
/// The extension's D-Bus protocol this build speaks.
const PROTOCOL: u32 = 1;
/// The island window's title, which the extension matches together with our
/// process id. Must equal the title in tauri.conf.json.
const WINDOW_TITLE: &str = "Coucou";

use crate::platform::ShellEvent as Event;

/// The same connection and owner as LINK, for questions asked from any
/// thread (gio connections are thread-safe; LINK lives on the main thread).
static REMOTE: Mutex<Option<(gio::DBusConnection, String)>> = Mutex::new(None);

/// True while the extension looks after the island.
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// What we want from the extension, re-sent whenever it (re)appears.
struct Wanted {
    tracking: bool,
    focusable: bool,
    placement: String,
}

struct Link {
    connection: gio::DBusConnection,
    owner: String,
    subscription: Option<gio::SignalSubscriptionId>,
}

thread_local! {
    static WANTED: RefCell<Wanted> = RefCell::new(Wanted {
        tracking: false,
        focusable: false,
        placement: "primary".into(),
    });
    static LINK: RefCell<Option<Link>> = const { RefCell::new(None) };
    /// Where events go: set by `start`, on the main thread.
    static ON_EVENT: RefCell<Option<Rc<dyn Fn(Event)>>> = const { RefCell::new(None) };
}

fn emit(event: Event) {
    // Cloned out first: the handler may well call back into this module.
    let handler = ON_EVENT.with(|h| h.borrow().clone());
    if let Some(handler) = handler {
        handler(event);
    }
}

/// Whether the extension is running right now. Asked once at launch, before
/// GTK starts, to choose between native Wayland and the Xwayland fallback.
pub fn on_bus() -> bool {
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return false;
    };
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(BUS_NAME,).to_variant()),
            Some(glib::VariantTy::new("(b)").unwrap()),
            gio::DBusCallFlags::NONE,
            500,
            None::<&gio::Cancellable>,
        )
        .ok()
        .and_then(|reply| reply.get::<(bool,)>())
        .is_some_and(|(owned,)| owned)
}

/// Whether GNOME is going to start the extension: installed, switched on, and
/// user extensions allowed. At login Coucou can start before GNOME has loaded
/// its extensions, so this is what tells "not yet" from "not at all".
pub fn expected() -> bool {
    let Some(settings) = shell_settings() else { return false };
    install_dir().join("metadata.json").is_file()
        && enabled_list(&settings).iter().any(|u| u == UUID)
        && !settings.boolean("disable-user-extensions")
}

/// Follows the extension for the rest of the run. Main thread only.
pub fn start(on_event: impl Fn(Event) + 'static) {
    ON_EVENT.with(|h| *h.borrow_mut() = Some(Rc::new(on_event)));
    // gio wants thread-safe closures, so these capture nothing; it calls them
    // back on this thread's main context, where ON_EVENT lives.
    let watcher = gio::bus_watch_name(
        gio::BusType::Session,
        BUS_NAME,
        gio::BusNameWatcherFlags::NONE,
        |connection, _name, owner| register(connection, owner.to_string()),
        |_connection, _name| {
            forget();
            emit(Event::Active(false));
        },
    );
    // Watched for as long as the app runs.
    std::mem::forget(watcher);
}

fn register(connection: gio::DBusConnection, owner: String) {
    let conn = connection.clone();
    let destination = owner.clone();
    connection.call(
        Some(&destination),
        OBJECT_PATH,
        IFACE,
        "Register",
        Some(&(WINDOW_TITLE,).to_variant()),
        Some(glib::VariantTy::new("(u)").unwrap()),
        gio::DBusCallFlags::NONE,
        // A build outside a packaged install makes GNOME ask the user first:
        // leave them the time to answer.
        120_000,
        None::<&gio::Cancellable>,
        move |reply| {
            let reply = match reply {
                Ok(v) => v,
                Err(err) => {
                    crate::log::line(format!("GNOME extension: registration refused or unanswered ({err})"));
                    return;
                }
            };
            let protocol = reply.get::<(u32,)>().map(|(p,)| p);
            if protocol != Some(PROTOCOL) {
                crate::log::line(format!("GNOME extension: not usable (protocol {protocol:?})"));
                return;
            }
            let subscription = conn.signal_subscribe(
                Some(owner.as_str()),
                Some(IFACE),
                Some("Pointer"),
                Some(OBJECT_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_conn, _sender, _path, _iface, _signal, params| {
                    if let Some((x, y, pressed)) = params.get::<(f64, f64, bool)>() {
                        emit(Event::Pointer { x, y, pressed });
                    }
                },
            );
            *REMOTE.lock().unwrap() = Some((conn.clone(), owner.clone()));
            LINK.with(|l| *l.borrow_mut() = Some(Link { connection: conn, owner, subscription: Some(subscription) }));
            ACTIVE.store(true, Ordering::Relaxed);
            crate::log::line("GNOME extension: island registered");
            WANTED.with(|w| {
                let w = w.borrow();
                send("SetPlacement", (w.placement.as_str(),).to_variant());
                send("SetTracking", (w.tracking,).to_variant());
                send("SetFocusable", (w.focusable,).to_variant());
            });
            emit(Event::Active(true));
        },
    );
}

fn forget() {
    ACTIVE.store(false, Ordering::Relaxed);
    *REMOTE.lock().unwrap() = None;
    LINK.with(|l| {
        if let Some(mut link) = l.borrow_mut().take() {
            if let Some(id) = link.subscription.take() {
                link.connection.signal_unsubscribe(id);
            }
            crate::log::line("GNOME extension: gone");
        }
    });
}

/// Fire and forget: the extension's answer changes nothing here.
fn send(method: &'static str, args: glib::Variant) {
    LINK.with(|l| {
        if let Some(link) = l.borrow().as_ref() {
            link.connection.call(
                Some(&link.owner),
                OBJECT_PATH,
                IFACE,
                method,
                Some(&args),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                None::<&gio::Cancellable>,
                |_| {},
            );
        }
    });
}

/// Runs `f` on the GTK main thread, now if we are on it.
fn on_main(f: impl FnOnce() + Send + 'static) {
    glib::MainContext::default().invoke(f);
}

/// Pointer reports on (island open) or off (hidden: nothing runs at all).
pub fn set_tracking(on: bool) {
    on_main(move || {
        WANTED.with(|w| w.borrow_mut().tracking = on);
        send("SetTracking", (on,).to_variant());
    });
}

/// Lets the island take the keyboard (the chat) or keeps it away.
pub fn set_focusable(on: bool) {
    on_main(move || {
        WANTED.with(|w| w.borrow_mut().focusable = on);
        send("SetFocusable", (on,).to_variant());
    });
}

/// "primary" or "cursor": the monitor the island lives on.
pub fn set_placement(screen: &str) {
    let screen = screen.to_string();
    on_main(move || {
        let changed = WANTED.with(|w| {
            let mut w = w.borrow_mut();
            let changed = w.placement != screen;
            w.placement = screen.clone();
            changed
        });
        if changed {
            send("SetPlacement", (screen.as_str(),).to_variant());
        }
    });
}

// ── Windows of other apps ─────────────────────────────────────────────────────

/// A question to the extension, answered before returning. Any thread.
fn ask(method: &str, args: Option<glib::Variant>, reply: &str) -> Option<glib::Variant> {
    ask_within(method, args, reply, 1500)
}

fn ask_within(method: &str, args: Option<glib::Variant>, reply: &str, timeout_ms: i32) -> Option<glib::Variant> {
    let (connection, owner) = REMOTE.lock().unwrap().clone()?;
    connection
        .call_sync(
            Some(&owner),
            OBJECT_PATH,
            IFACE,
            method,
            args.as_ref(),
            Some(glib::VariantTy::new(reply).ok()?),
            gio::DBusCallFlags::NONE,
            timeout_ms,
            None::<&gio::Cancellable>,
        )
        .ok()
}

/// (app name, window title) from a `(bss)` answer.
fn window_answer(v: glib::Variant) -> Option<(String, String)> {
    let (found, app, title) = v.get::<(bool, String, String)>()?;
    found.then_some((app, title))
}

/// Brings back the window of the nearest of `pids` that has one; a title
/// containing `hint` wins among several. False when none has a window.
pub fn activate_window_of(pids: &[u32], hint: &str) -> bool {
    ask("ActivateWindowOf", Some((pids.to_vec(), hint).to_variant()), "(b)")
        .and_then(|v| v.get::<(bool,)>())
        .is_some_and(|(found,)| found)
}

/// The window the user was in before the island.
pub fn last_focused_window() -> Option<(String, String)> {
    ask("LastFocusedWindow", None, "(bss)").and_then(window_answer)
}

/// A PNG of the window the user was in, written by the extension to `path`
/// (a "window-<digits>.png" in the inbox; it refuses anything else).
pub fn capture_window(path: &str) -> Option<(String, String)> {
    ask_within("CaptureWindow", Some((path,).to_variant()), "(bss)", 6000).and_then(window_answer)
}

/// The window under a point relative to the island. Main thread: answers
/// arrive on the main loop, so a pointer callback never waits on them.
pub fn window_at(x: f64, y: f64, done: impl FnOnce(Option<(String, String)>) + 'static) {
    let Some((connection, owner)) = REMOTE.lock().unwrap().clone() else { return done(None) };
    connection.call(
        Some(&owner),
        OBJECT_PATH,
        IFACE,
        "WindowAt",
        Some(&(x, y).to_variant()),
        Some(glib::VariantTy::new("(bss)").unwrap()),
        gio::DBusCallFlags::NONE,
        1500,
        None::<&gio::Cancellable>,
        move |reply| done(reply.ok().and_then(window_answer)),
    );
}

// ── Installing the extension ──────────────────────────────────────────────────

/// The extension's id, and its folder name under the user's extensions.
const UUID: &str = "coucou@coucouhelper";

/// The files GNOME Shell needs, built into the app. debug.js stays out: it is
/// for test shells only.
const FILES: [(&str, &str); 2] = [
    ("metadata.json", include_str!("../../../../gnome-extension/metadata.json")),
    ("extension.js", include_str!("../../../../gnome-extension/extension.js")),
];

fn install_dir() -> std::path::PathBuf {
    super::xdg("XDG_DATA_HOME", ".local/share").join("gnome-shell/extensions").join(UUID)
}

/// org.gnome.shell, or None off GNOME (asking gio for a missing schema aborts).
fn shell_settings() -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup("org.gnome.shell", true)?;
    Some(gio::Settings::new("org.gnome.shell"))
}

fn enabled_list(settings: &gio::Settings) -> Vec<String> {
    settings.strv("enabled-extensions").iter().map(|s| s.to_string()).collect()
}

pub fn status() -> crate::platform::ShellExtensionStatus {
    let Some(settings) = shell_settings() else { return Default::default() };
    let dir = install_dir();
    let installed = dir.join("metadata.json").is_file();
    let up_to_date = installed
        && FILES.iter().all(|(name, bundled)| {
            std::fs::read_to_string(dir.join(name)).is_ok_and(|on_disk| on_disk == *bundled)
        });
    crate::platform::ShellExtensionStatus {
        applicable: true,
        installed,
        up_to_date,
        enabled: enabled_list(&settings).iter().any(|u| u == UUID),
        user_extensions_disabled: settings.boolean("disable-user-extensions"),
        active: active(),
    }
}

/// Writes the extension where GNOME looks for it and adds it to the ones it
/// starts. Nothing else of GNOME's is touched. Only ever called from a click
/// in Settings.
pub fn install() -> Result<(), String> {
    let settings = shell_settings().ok_or("This is not a GNOME session.")?;
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, contents) in FILES {
        std::fs::write(dir.join(name), contents).map_err(|e| format!("{name}: {e}"))?;
    }
    let mut list = enabled_list(&settings);
    if !list.iter().any(|u| u == UUID) {
        list.push(UUID.to_string());
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        settings.set_strv("enabled-extensions", refs.as_slice()).map_err(|e| e.to_string())?;
        gio::Settings::sync();
    }
    crate::log::line("GNOME extension installed");
    Ok(())
}

/// Turns the extension off (GNOME stops it at once) and deletes its folder.
pub fn remove() -> Result<(), String> {
    let settings = shell_settings().ok_or("This is not a GNOME session.")?;
    let list: Vec<String> = enabled_list(&settings).into_iter().filter(|u| u != UUID).collect();
    let refs: Vec<&str> = list.iter().map(String::as_str).collect();
    settings.set_strv("enabled-extensions", refs.as_slice()).map_err(|e| e.to_string())?;
    gio::Settings::sync();
    let dir = install_dir();
    if dir.ends_with(UUID) && dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    crate::log::line("GNOME extension removed");
    Ok(())
}
