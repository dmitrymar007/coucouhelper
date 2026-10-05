// Coucou island — the GNOME Shell half of the Coucou app.
//
// GNOME has no layer-shell, so a Wayland app cannot place its own window,
// keep it over the top bar, or know where the pointer is. This extension does
// exactly that for one window: the island of the Coucou process that
// registered over D-Bus, and nothing else.
//   * placement: top centre of the chosen monitor, over the top bar, on every
//     workspace, out of Alt+Tab, the overview and the dock;
//   * focus: the island never takes the keyboard unless the app asks (chat);
//   * pointer: while the app asks — only while its island is open — where the
//     pointer is and whether a button is held, relative to the island, sent to
//     that app alone.
//
// Over the top bar means global.top_window_group, the layer Mutter keeps menus
// in. It is also above the lock screen, which is why this extension declares
// no "unlock-dialog" session mode: locking the screen disables it, the island
// drops back among ordinary windows under the shield, and the app registers
// again once the session is unlocked.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import * as Workspace from 'resource:///org/gnome/shell/ui/workspace.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'org.coucouhelper.Shell';
const OBJECT_PATH = '/org/coucouhelper/Shell';
const IFACE_NAME = 'org.coucouhelper.Shell1';
/** Bumped when the D-Bus interface changes incompatibly. */
const PROTOCOL = 1;
/** Pointer sampling while the island is open, like the Windows cursor poll. */
const POINTER_MS = 16;
const BUTTONS =
    Clutter.ModifierType.BUTTON1_MASK |
    Clutter.ModifierType.BUTTON2_MASK |
    Clutter.ModifierType.BUTTON3_MASK;

/** Coucou's executable is called this in every build — necessary, never enough. */
const APP_EXECUTABLE = 'coucou';

/**
 * Development only: debug.js (screenshots, synthetic clicks) is loaded when
 * GNOME Shell runs with COUCOU_EXT_DEBUG=1 and the file is there — the app
 * never installs it.
 */
const DEBUG = GLib.getenv('COUCOU_EXT_DEBUG') === '1';

const iface = (extraMethods = '') => `<node>
  <interface name="${IFACE_NAME}">
    <method name="Register">
      <arg type="s" name="title" direction="in"/>
      <arg type="u" name="protocol" direction="out"/>
    </method>
    <method name="SetPlacement">
      <arg type="s" name="screen" direction="in"/>
    </method>
    <method name="SetTracking">
      <arg type="b" name="on" direction="in"/>
    </method>
    <method name="SetFocusable">
      <arg type="b" name="on" direction="in"/>
    </method>
    <method name="ActivateWindowOf">
      <arg type="au" name="pids" direction="in"/>
      <arg type="s" name="hint" direction="in"/>
      <arg type="b" name="found" direction="out"/>
    </method>
    <method name="LastFocusedWindow">
      <arg type="b" name="found" direction="out"/>
      <arg type="s" name="app" direction="out"/>
      <arg type="s" name="title" direction="out"/>
    </method>
    <method name="CaptureWindow">
      <arg type="s" name="path" direction="in"/>
      <arg type="b" name="found" direction="out"/>
      <arg type="s" name="app" direction="out"/>
      <arg type="s" name="title" direction="out"/>
    </method>
    <method name="WindowAt">
      <arg type="d" name="x" direction="in"/>
      <arg type="d" name="y" direction="in"/>
      <arg type="b" name="found" direction="out"/>
      <arg type="s" name="app" direction="out"/>
      <arg type="s" name="title" direction="out"/>
    </method>
    <property name="Protocol" type="u" access="read"/>
    <signal name="Pointer">
      <arg type="d" name="x"/>
      <arg type="d" name="y"/>
      <arg type="b" name="pressed"/>
    </signal>
    ${extraMethods}
  </interface>
</node>`;

/** A window the user works in: not ours, not a menu, a tooltip or the desktop. */
function isUserWindow(win) {
    return !!win && !win._coucouIsland && !win.skip_taskbar &&
        win.get_window_type() === Meta.WindowType.NORMAL;
}

/** The app's name ("Firefox") and the window title, for the chat's context. */
function describe(win) {
    const app = Shell.WindowTracker.get_default().get_window_app(win);
    return [true, app?.get_name() ?? win.get_wm_class() ?? '', win.get_title() ?? ''];
}

const NOTHING = [false, '', ''];

/** Process id behind a D-Bus connection, as the bus daemon vouches for it. */
async function senderPid(sender) {
    const reply = await Gio.DBus.session.call(
        'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
        'GetConnectionUnixProcessID', new GLib.Variant('(s)', [sender]),
        new GLib.VariantType('(u)'), Gio.DBusCallFlags.NONE, -1, null);
    return reply.deepUnpack()[0];
}

/** Path of a process's executable, or null when it cannot be read or is gone. */
function executablePath(pid) {
    try {
        const path = GLib.file_read_link(`/proc/${pid}/exe`);
        return path.endsWith(' (deleted)') ? null : path;
    } catch {
        return null;
    }
}

/**
 * Whether `path` and every folder above it belong to root and only root may
 * write to them: a packaged install (/usr/bin/coucou) that no program running
 * as the user can replace or sit beside.
 */
function rootOwned(path) {
    let at = path;
    for (;;) {
        let info;
        try {
            info = Gio.File.new_for_path(at).query_info(
                'unix::uid,unix::mode', Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS, null);
        } catch {
            return false;
        }
        if (info.get_attribute_uint32('unix::uid') !== 0 ||
            (info.get_attribute_uint32('unix::mode') & 0o022) !== 0)
            return false;
        if (at === '/')
            return true;
        at = GLib.path_get_dirname(at);
    }
}

/** SHA-256 of the image a process is running — not of whatever sits at its path now. */
function runningImageHash(pid) {
    return new Promise(resolve => {
        Gio.File.new_for_path(`/proc/${pid}/exe`).load_contents_async(null, (file, res) => {
            try {
                const [, bytes] = file.load_contents_finish(res);
                resolve(GLib.compute_checksum_for_data(GLib.ChecksumType.SHA256, bytes));
            } catch {
                resolve(null);
            }
        });
    });
}

/**
 * Coucou builds the user allowed this session: path → hash of the image. Kept
 * at module level, so locking the screen (which disables user extensions)
 * does not ask again; a new login, or a different file at that path, does.
 */
const approved = new Map();
/** Paths the user said no to this session: never asked about again. */
const denied = new Set();
/** One question at a time, and not one right after another. */
let asking = false;
let lastAsked = 0;
const ASK_GAP_US = 60 * GLib.USEC_PER_SEC;

/**
 * Environment that makes a genuine Coucou binary run someone else's code:
 * preloaded or audited libraries, a library path, a module path for GTK or
 * GIO.
 */
const CODE_INJECTING_ENV = [
    'LD_PRELOAD', 'LD_AUDIT', 'LD_LIBRARY_PATH', 'GTK_PATH', 'GIO_MODULE_DIR',
];

/**
 * GTK modules are fine as bare names, which GTK looks up in the system's own
 * folders — Ubuntu sets GTK_MODULES=gail:atk-bridge for every session. A path
 * would load any file at all.
 */
const MODULE_LISTS = ['GTK_MODULES', 'GTK3_MODULES'];

function injecting(variable) {
    const at = variable.indexOf('=');
    const name = variable.slice(0, at);
    const value = variable.slice(at + 1);
    if (at <= 0 || !value)
        return false;
    if (CODE_INJECTING_ENV.includes(name))
        return true;
    if (MODULE_LISTS.includes(name))
        return value.split(/[:;,]/).some(m => m !== '' && !/^[A-Za-z0-9_-]+$/.test(m));
    return false;
}

/**
 * Whether the process runs other code than its own file: started with one of
 * those variables, or under a debugger. Either way the right file proves
 * nothing, so it is refused.
 */
function tampered(pid) {
    try {
        const [, environ] = GLib.file_get_contents(`/proc/${pid}/environ`);
        const vars = new TextDecoder().decode(environ).split('\0');
        if (vars.some(injecting))
            return true;
        const [, status] = GLib.file_get_contents(`/proc/${pid}/status`);
        const tracer = /^TracerPid:\s*(\d+)/m.exec(new TextDecoder().decode(status));
        return !tracer || tracer[1] !== '0';
    } catch {
        return true;
    }
}

/**
 * Asks the user, in GNOME Shell's own dialog, whether the program at `path`
 * may drive the island. No app can click a Shell dialog on Wayland, so the
 * answer is the user's. Unanswered, it is a no.
 */
function askToAllow(path) {
    // The path is shown on a line of its own, in quotes; a path that could
    // fake a line of the dialog never gets this far (see trustedCoucou).
    const home = GLib.get_home_dir();
    const inHome = path.startsWith(`${home}/`);
    return new Promise(resolve => {
        const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
        const content = new Dialog.MessageDialogContent({
            title: 'Allow Coucou to use the island?',
            description:
                `“${path}”\n\n` +
                'This program asks to put the Coucou island over the top bar, to know where ' +
                'the pointer is and which window you are in, and to bring windows to the front. ' +
                (inHome
                    ? 'It lives in your home folder, where any program of yours could have put it. '
                    : '') +
                'Allow it only if you started Coucou from there yourself.',
        });
        dialog.contentLayout.add_child(content);
        let answered = false;
        const answer = yes => {
            if (answered)
                return;
            answered = true;
            GLib.source_remove(timer);
            dialog.close();
            resolve(yes);
        };
        const timer = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 110, () => {
            answer(false);
            return GLib.SOURCE_REMOVE;
        });
        dialog.setButtons([
            {label: 'Deny', action: () => answer(false), key: Clutter.KEY_Escape},
            {label: 'Allow', action: () => answer(true)},
        ]);
        if (!dialog.open())
            answer(false);
    });
}

/**
 * Whether the process `pid` is a Coucou the user stands behind: a packaged
 * install, or a build at a path the user allowed this session, still the
 * same file. The executable's name alone proves nothing — any program can
 * copy a binary under the name coucou.
 */
async function trustedCoucou(pid) {
    const path = executablePath(pid);
    if (!path || GLib.path_get_basename(path) !== APP_EXECUTABLE)
        return false;
    // Nothing that could pass for another line of the dialog, nor a novel.
    if (path.length > 512 || /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u202a-\u202e\u2066-\u2069]/.test(path))
        return false;
    // The right file running someone else's code is not Coucou.
    if (tampered(pid))
        return false;
    if (rootOwned(path))
        return true;
    const hash = await runningImageHash(pid);
    if (!hash)
        return false;
    if (approved.get(path) === hash)
        return true;
    // No question again about a path already refused, none while one is up,
    // and none right after the last: a program calling Register in a loop
    // must not get to wear the user down.
    const now = GLib.get_monotonic_time();
    if (denied.has(path) || asking || (lastAsked && now - lastAsked < ASK_GAP_US))
        return false;
    asking = true;
    let yes = false;
    try {
        yes = await askToAllow(path);
    } finally {
        asking = false;
        lastAsked = GLib.get_monotonic_time();
    }
    if (!yes) {
        denied.add(path);
        return false;
    }
    approved.set(path, hash);
    return true;
}

/** The one island being looked after, and the app that owns it. */
class Island {
    constructor(sender, pid, title, onGone) {
        this._sender = sender;
        this._pid = pid;
        this._title = title;
        this._placement = 'primary';
        this._focusable = false;
        this._window = null;
        this._windowSignals = [];
        this._pointerSource = 0;
        this._lastPointer = null;
        /** When the user last pressed a button over the island (µs, monotonic). */
        this._islandPressAt = 0;
        // Where the user is right now, for the chat's context until focus moves.
        const focused = global.display.focus_window;
        this._lastFocus = isUserWindow(focused) ? focused : null;

        // The app quitting or crashing ends the registration.
        this._watch = Gio.bus_watch_name_on_connection(
            Gio.DBus.session, sender, Gio.BusNameWatcherFlags.NONE, null, () => onGone(this));

        this._shellSignals = [
            [global.window_manager, global.window_manager.connect('map',
                (_wm, actor) => this._consider(actor.get_meta_window()))],
            [global.display, global.display.connect('notify::focus-window', () => this._focusChanged())],
            [global.display, global.display.connect('restacked', () => this._raise())],
            [Main.layoutManager, Main.layoutManager.connect('monitors-changed', () => this._place())],
        ];

        for (const win of global.display.list_all_windows())
            this._consider(win);
    }

    get window() {
        return this._window;
    }

    get sender() {
        return this._sender;
    }

    _consider(win) {
        if (this._window || !win || win.get_pid() !== this._pid || win.get_title() !== this._title)
            return;
        this._window = win;
        win._coucouIsland = true;
        win.make_above();
        win.stick();
        this._windowSignals = [
            win.connect('size-changed', () => this._place()),
            win.connect('position-changed', () => this._placeLater()),
            win.connect('unmanaged', () => this._forget()),
        ];
        this._raise();
        this._place();
        this._moveClockAside();
        log(`coucou: island adopted (pid ${this._pid})`);
    }

    _forget() {
        this._restoreClock();
        if (!this._window)
            return;
        for (const id of this._windowSignals)
            this._window.disconnect(id);
        this._windowSignals = [];
        delete this._window._coucouIsland;
        this._window = null;
        this.setTracking(false);
    }

    /** Over the top bar: the window's actor goes to the layer above chrome. */
    _raise() {
        const actor = this._window?.get_compositor_private();
        if (!actor)
            return;
        const parent = actor.get_parent();
        if (parent === global.top_window_group)
            return;
        parent?.remove_child(actor);
        global.top_window_group.add_child(actor);
    }

    /**
     * The island sits where GNOME puts the clock, so while it is there the
     * clock moves to the left end of the top bar — the way the Mac's menu bar
     * keeps clear of the notch — and goes back when the island does.
     */
    _moveClockAside() {
        const clock = Main.panel.statusArea.dateMenu?.container;
        const left = Main.panel._leftBox;
        const parent = clock?.get_parent();
        if (this._clock || !clock || !left || !parent || parent === left)
            return;
        this._clock = {parent, index: parent.get_children().indexOf(clock)};
        parent.remove_child(clock);
        left.add_child(clock);
    }

    _restoreClock() {
        const saved = this._clock;
        this._clock = null;
        const clock = Main.panel.statusArea.dateMenu?.container;
        if (!saved || !clock || clock.get_parent() !== Main.panel._leftBox)
            return;
        Main.panel._leftBox.remove_child(clock);
        saved.parent.insert_child_at_index(clock, Math.max(0, saved.index));
    }

    /** Back among ordinary windows, where Mutter expects it. */
    _lower() {
        const actor = this._window?.get_compositor_private();
        if (!actor || actor.get_parent() !== global.top_window_group)
            return;
        global.top_window_group.remove_child(actor);
        global.window_group.add_child(actor);
    }

    _monitor() {
        return this._placement === 'cursor'
            ? global.display.get_current_monitor()
            : global.display.get_primary_monitor();
    }

    /** Top centre of the monitor, right against the edge. */
    _place() {
        const win = this._window;
        if (!win)
            return;
        const area = global.display.get_monitor_geometry(this._monitor());
        const rect = win.get_frame_rect();
        const x = area.x + Math.round((area.width - rect.width) / 2);
        const y = area.y;
        // user_op = true: an ordinary window is otherwise pushed back below
        // the top bar by Mutter's "keep the titlebar visible" constraint.
        if (rect.x !== x || rect.y !== y)
            win.move_frame(true, x, y);
    }

    /** A move we did not make (Mutter's constraints, a stray drag): undo it. */
    _placeLater() {
        if (this._placeSource)
            return;
        this._placeSource = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
            this._placeSource = 0;
            this._place();
            return GLib.SOURCE_REMOVE;
        });
    }

    setPlacement(screen) {
        this._placement = screen === 'cursor' ? 'cursor' : 'primary';
        this._place();
    }

    /** The keyboard goes to the island only while the app asks (the chat). */
    setFocusable(on) {
        this._focusable = on;
        if (on)
            this._window?.activate(global.get_current_time());
        else if (this._window?.has_focus())
            this._giveFocusBack();
    }

    _focusChanged() {
        const focused = global.display.focus_window;
        if (focused && focused === this._window) {
            // A click on the island must not take the keyboard from the
            // window the user is typing in — the Mac island never does.
            if (!this._focusable)
                this._giveFocusBack();
        } else if (focused) {
            this._lastFocus = focused;
        }
    }

    /** The window the user was in before the island, if it is still there. */
    lastFocused() {
        const win = this._lastFocus;
        return isUserWindow(win) && win.get_compositor_private() ? describe(win) : NOTHING;
    }

    /**
     * A PNG of the window the user was in before the island, for the chat:
     * the area of its frame as it shows on screen. Written only to `path`,
     * which must be a "window-<digits>.png" in Coucou's own inbox.
     */
    async captureLast(path) {
        const inbox = GLib.build_filenamev([GLib.get_user_data_dir(), 'coucou', 'inbox']);
        const name = GLib.path_get_basename(path);
        if (GLib.path_get_dirname(path) !== inbox || !/^window-\d{1,20}\.png$/.test(name))
            return NOTHING;
        // Wayland lets no app read the screen without the user's say-so. Here
        // the say-so is a real click on the island in the last few seconds —
        // the chat's camera button — and it buys exactly one screenshot: no
        // process can make the extension capture a window behind the user's back.
        const sincePress = GLib.get_monotonic_time() - this._islandPressAt;
        this._islandPressAt = 0;
        if (sincePress > 3 * GLib.USEC_PER_SEC)
            return NOTHING;
        const win = this._lastFocus;
        if (!isUserWindow(win) || !win.get_compositor_private() || win.minimized)
            return NOTHING;
        const rect = win.get_frame_rect();
        let stream;
        try {
            stream = Gio.File.new_for_path(path).replace(null, false, Gio.FileCreateFlags.NONE, null);
        } catch {
            return NOTHING;
        }
        const shooter = new Shell.Screenshot();
        const ok = await new Promise(resolve => {
            try {
                shooter.screenshot_area(rect.x, rect.y, rect.width, rect.height, stream, (obj, res) => {
                    try {
                        obj.screenshot_area_finish(res);
                        resolve(true);
                    } catch {
                        resolve(false);
                    }
                });
            } catch {
                resolve(false);
            }
        });
        try {
            stream.close(null);
        } catch {}
        return ok ? describe(win) : NOTHING;
    }

    /** The topmost window under a point given relative to the island. */
    windowAt(x, y) {
        if (!this._window)
            return NOTHING;
        const origin = this._window.get_frame_rect();
        const px = origin.x + x;
        const py = origin.y + y;
        const workspace = global.workspace_manager.get_active_workspace();
        const candidates = global.display.list_all_windows().filter(w =>
            isUserWindow(w) && !w.minimized && w.showing_on_its_workspace?.() !== false &&
            (w.is_on_all_workspaces() || w.get_workspace() === workspace));
        const hit = global.display.sort_windows_by_stacking(candidates).reverse().find(w => {
            const r = w.get_frame_rect();
            return px >= r.x && px < r.x + r.width && py >= r.y && py < r.y + r.height;
        });
        return hit ? describe(hit) : NOTHING;
    }

    /**
     * Brings back the window of the nearest process in `pids` (Claude Code's
     * ancestors, nearest first) that has one. One process can own several —
     * gnome-terminal, VS Code — so a title containing `hint` (the project
     * folder) wins, then the most recently used.
     */
    activateWindowOf(pids, hint) {
        const windows = global.display.list_all_windows().filter(isUserWindow);
        for (const pid of pids) {
            const mine = windows.filter(w => w.get_pid() === pid);
            if (!mine.length)
                continue;
            const titled = hint ? mine.filter(w => (w.get_title() ?? '').includes(hint)) : [];
            const pool = titled.length ? titled : mine;
            const pick = pool.sort((a, b) => b.get_user_time() - a.get_user_time())[0];
            Main.activateWindow(pick);
            return true;
        }
        return false;
    }

    /**
     * Hands the keyboard to a window the user can see. The island is sticky
     * and above, so Mutter picks it whenever it looks for a window to focus:
     * after a minimize, a close or a workspace switch. Going back to the last
     * window then would undo what the user just did — unminimize it, or pull
     * them back to the workspace they left — so the last window only gets the
     * focus back while it is on this workspace and showing; otherwise the
     * window Mutter would have picked, the island aside; otherwise none.
     * focus(), not activate(): nothing is raised, unminimized or switched to.
     */
    _giveFocusBack() {
        const time = global.get_current_time();
        const workspace = global.workspace_manager.get_active_workspace();
        const visible = w => isUserWindow(w) && w.get_compositor_private() && !w.minimized &&
            (w.is_on_all_workspaces() || w.get_workspace() === workspace);
        const previous = this._lastFocus;
        const target = visible(previous)
            ? previous
            : global.display.sort_windows_by_stacking(workspace.list_windows().filter(visible)).pop();
        if (target)
            target.focus(time);
        else
            global.display.unset_input_focus(time);
    }

    setTracking(on) {
        if (on && !this._pointerSource) {
            this._lastPointer = null;
            this._pointerSource = GLib.timeout_add(GLib.PRIORITY_DEFAULT, POINTER_MS, () => {
                this._samplePointer();
                return GLib.SOURCE_CONTINUE;
            });
        } else if (!on && this._pointerSource) {
            GLib.source_remove(this._pointerSource);
            this._pointerSource = 0;
        }
    }

    /** Pointer relative to the island, to its owner only, and only on change. */
    _samplePointer() {
        if (!this._window)
            return;
        const [px, py, mods] = global.get_pointer();
        const rect = this._window.get_frame_rect();
        const x = px - rect.x;
        const y = py - rect.y;
        const pressed = (mods & BUTTONS) !== 0;
        const last = this._lastPointer;
        // A real press over the island: what entitles Coucou to one screenshot.
        if (pressed && !last?.[2] && x >= 0 && y >= 0 && x < rect.width && y < rect.height)
            this._islandPressAt = GLib.get_monotonic_time();
        if (last && last[0] === x && last[1] === y && last[2] === pressed)
            return;
        this._lastPointer = [x, y, pressed];
        Gio.DBus.session.emit_signal(this._sender, OBJECT_PATH, IFACE_NAME, 'Pointer',
            new GLib.Variant('(ddb)', [x, y, pressed]));
    }

    destroy() {
        this.setTracking(false);
        if (this._placeSource)
            GLib.source_remove(this._placeSource);
        Gio.bus_unwatch_name(this._watch);
        for (const [object, id] of this._shellSignals)
            object.disconnect(id);
        this._shellSignals = [];
        if (this._window) {
            this._lower();
            this._window.unmake_above();
            this._window.unstick();
        }
        this._forget();
    }
}

export default class CoucouIslandExtension extends Extension {
    enable() {
        this._enabled = true;
        this._island = null;
        this._patches = [];
        this._hideFromShell();
        if (DEBUG)
            this._exportWithDebug();
        else
            this._export();
    }

    disable() {
        this._enabled = false;
        if (this._nameId)
            Gio.bus_unown_name(this._nameId);
        this._nameId = 0;
        this._dbus?.unexport();
        this._dbus = null;
        this._island?.destroy();
        this._island = null;
        for (const [proto, name, original] of this._patches)
            proto[name] = original;
        this._patches = [];
    }

    // ── D-Bus ────────────────────────────────────────────────────────────────

    _export(extraMethods = '') {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(iface(extraMethods), this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, BUS_NAME, Gio.BusNameOwnerFlags.NONE, null, null);
    }

    async _exportWithDebug() {
        let methods = '';
        try {
            const debug = await import('./debug.js');
            debug.install(this);
            methods = debug.METHODS;
        } catch (e) {
            log(`coucou: no debug helpers (${e.message})`);
        }
        if (this._enabled)
            this._export(methods);
    }

    /** The island currently looked after, for the debug helpers. */
    get island() {
        return this._island;
    }

    get Protocol() {
        return PROTOCOL;
    }

    async RegisterAsync([title], invocation) {
        const sender = invocation.get_sender();
        try {
            // The window is matched by the caller's own process id, so no
            // other program can have some window of its choice pinned on top.
            const pid = await senderPid(sender);
            // Pinning a window over the top bar, out of every switcher, and
            // reporting the pointer to it is more than Wayland gives any app:
            // only Coucou gets it, and an island already looked after is not
            // handed to anyone else while its owner is around.
            if (!await trustedCoucou(pid))
                throw new Error('only a Coucou the user stands behind can register');
            if (this._island && this._island.sender !== sender)
                throw new Error('another Coucou island is registered');
            this._island?.destroy();
            this._island = new Island(sender, pid, title, gone => {
                if (this._island === gone) {
                    gone.destroy();
                    this._island = null;
                }
            });
            invocation.return_value(new GLib.Variant('(u)', [PROTOCOL]));
        } catch (e) {
            invocation.return_dbus_error('org.coucouhelper.Shell.Error', String(e));
        }
    }

    SetPlacementAsync([screen], invocation) {
        this._ownedBy(invocation)?.setPlacement(screen);
        invocation.return_value(null);
    }

    SetTrackingAsync([on], invocation) {
        this._ownedBy(invocation)?.setTracking(on);
        invocation.return_value(null);
    }

    SetFocusableAsync([on], invocation) {
        this._ownedBy(invocation)?.setFocusable(on);
        invocation.return_value(null);
    }

    ActivateWindowOfAsync([pids, hint], invocation) {
        const found = this._ownedBy(invocation)?.activateWindowOf(pids, hint) ?? false;
        invocation.return_value(new GLib.Variant('(b)', [found]));
    }

    LastFocusedWindowAsync(_params, invocation) {
        const answer = this._ownedBy(invocation)?.lastFocused() ?? NOTHING;
        invocation.return_value(new GLib.Variant('(bss)', answer));
    }

    async CaptureWindowAsync([path], invocation) {
        const island = this._ownedBy(invocation);
        const answer = island ? await island.captureLast(path) : NOTHING;
        invocation.return_value(new GLib.Variant('(bss)', answer));
    }

    WindowAtAsync([x, y], invocation) {
        const answer = this._ownedBy(invocation)?.windowAt(x, y) ?? NOTHING;
        invocation.return_value(new GLib.Variant('(bss)', answer));
    }

    /** Only the app that registered may steer its island. */
    _ownedBy(invocation) {
        return this._island?.sender === invocation.get_sender() ? this._island : null;
    }

    // ── Out of Alt+Tab, the overview and the dock ────────────────────────────

    _patch(proto, name, wrap) {
        const original = proto[name];
        this._patches.push([proto, name, original]);
        proto[name] = wrap(original);
    }

    _hideFromShell() {
        const isIsland = w => !!(w?.get_meta_window ? w.get_meta_window() : w)?._coucouIsland;
        const without = list => list.filter(w => !isIsland(w));
        this._patch(Shell.Global.prototype, 'get_window_actors',
            original => function (...args) {
                return without(original.apply(this, args));
            });
        this._patch(Meta.Display.prototype, 'get_tab_list',
            original => function (...args) {
                return without(original.apply(this, args));
            });
        this._patch(Shell.App.prototype, 'get_windows',
            original => function (...args) {
                return without(original.apply(this, args));
            });
        this._patch(Workspace.Workspace.prototype, '_isOverviewWindow',
            original => function (window) {
                return !isIsland(window) && original.call(this, window);
            });
    }
}
