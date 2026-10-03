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

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
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

/** Development only: a stage screenshot, to check the island without eyes. */
const DEBUG = GLib.getenv('COUCOU_EXT_DEBUG') === '1';

const IFACE = `<node>
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
    <property name="Protocol" type="u" access="read"/>
    <signal name="Pointer">
      <arg type="d" name="x"/>
      <arg type="d" name="y"/>
      <arg type="b" name="pressed"/>
    </signal>
    ${DEBUG ? `<method name="Screenshot">
      <arg type="s" name="path" direction="in"/>
      <arg type="s" name="report" direction="out"/>
    </method>
    <method name="Click">
      <arg type="d" name="x" direction="in"/>
      <arg type="d" name="y" direction="in"/>
      <arg type="s" name="report" direction="out"/>
    </method>` : ''}
  </interface>
</node>`;

/** Process id behind a D-Bus connection, as the bus daemon vouches for it. */
async function senderPid(sender) {
    const reply = await Gio.DBus.session.call(
        'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
        'GetConnectionUnixProcessID', new GLib.Variant('(s)', [sender]),
        new GLib.VariantType('(u)'), Gio.DBusCallFlags.NONE, -1, null);
    return reply.deepUnpack()[0];
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
        this._lastFocus = null;

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
        log(`coucou: island adopted (pid ${this._pid})`);
    }

    _forget() {
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
        if (!actor || (DEBUG && GLib.getenv('COUCOU_EXT_NORAISE') === '1'))
            return;
        const parent = actor.get_parent();
        if (parent === global.top_window_group)
            return;
        parent?.remove_child(actor);
        global.top_window_group.add_child(actor);
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
        if (DEBUG) {
            const r = this._window?.get_frame_rect();
            log(`coucou: moved to ${r?.x},${r?.y} ${r?.width}x${r?.height}`);
        }
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

    _giveFocusBack() {
        const previous = this._lastFocus;
        if (previous && previous !== this._window && previous.get_compositor_private())
            previous.activate(global.get_current_time());
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
        this._island = null;
        this._patches = [];
        this._hideFromShell();

        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, BUS_NAME, Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        Gio.bus_unown_name(this._nameId);
        this._dbus.unexport();
        this._dbus = null;
        this._island?.destroy();
        this._island = null;
        for (const [proto, name, original] of this._patches)
            proto[name] = original;
        this._patches = [];
    }

    // ── D-Bus ────────────────────────────────────────────────────────────────

    get Protocol() {
        return PROTOCOL;
    }

    async RegisterAsync([title], invocation) {
        const sender = invocation.get_sender();
        try {
            // The window is matched by the caller's own process id, so no
            // other program can have some window of its choice pinned on top.
            const pid = await senderPid(sender);
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

    async ScreenshotAsync([path], invocation) {
        const shooter = new Shell.Screenshot();
        const stream = Gio.File.new_for_path(path).replace(null, false, Gio.FileCreateFlags.NONE, null);
        await shooter.screenshot(false, stream);
        stream.close(null);
        const win = this._island?.window;
        const actor = win?.get_compositor_private();
        const rect = win?.get_frame_rect();
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify({
            adopted: !!win,
            frame: rect ? [rect.x, rect.y, rect.width, rect.height] : null,
            overTopBar: actor?.get_parent() === global.top_window_group,
            actorVisible: actor?.visible ?? null,
            actorMapped: actor?.mapped ?? null,
            actorBox: actor ? [actor.x, actor.y, actor.width, actor.height] : null,
            topGroupVisible: global.top_window_group.visible,
            topGroupMapped: global.top_window_group.mapped,
            above: win?.is_above() ?? false,
            focus: global.display.focus_window?.get_title() ?? null,
        })]));
    }

    /** Development only: a real click at stage (x, y), then what it opened. */
    async ClickAsync([x, y], invocation) {
        const seat = Clutter.get_default_backend().get_default_seat();
        this._pointer ??= seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        const wait = ms => new Promise(r => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => r() ?? GLib.SOURCE_REMOVE));
        const now = () => GLib.get_monotonic_time();
        this._pointer.notify_absolute_motion(now(), x, y);
        await wait(80);
        this._pointer.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
        await wait(60);
        this._pointer.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
        await wait(300);
        const open = Object.entries(Main.panel.statusArea)
            .filter(([, item]) => item?.menu?.isOpen)
            .map(([name]) => name);
        const rect = this._island?.window?.get_frame_rect();
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify({
            frame: rect ? [rect.x, rect.y, rect.width, rect.height] : null,
            openPanelMenus: open,
            focus: global.display.focus_window?.get_title() ?? null,
        })]));
        for (const name of open)
            Main.panel.statusArea[name].menu.close();
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
