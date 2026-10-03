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

use gtk::gio;
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
        3000,
        None::<&gio::Cancellable>,
        move |reply| {
            let protocol = reply.ok().and_then(|v| v.get::<(u32,)>()).map(|(p,)| p);
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
