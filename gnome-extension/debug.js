// Development only — never installed by the app. Loaded by extension.js when
// GNOME Shell runs with COUCOU_EXT_DEBUG=1 (a nested test shell) and this
// file sits next to it. A stage screenshot and a real synthetic click let the
// island be checked without anyone looking at the screen.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

export const METHODS = `
    <method name="Screenshot">
      <arg type="s" name="path" direction="in"/>
      <arg type="s" name="report" direction="out"/>
    </method>
    <method name="Click">
      <arg type="d" name="x" direction="in"/>
      <arg type="d" name="y" direction="in"/>
      <arg type="s" name="report" direction="out"/>
    </method>`;

function islandReport(ext) {
    const win = ext.island?.window;
    const actor = win?.get_compositor_private();
    const rect = win?.get_frame_rect();
    return {
        adopted: !!win,
        frame: rect ? [rect.x, rect.y, rect.width, rect.height] : null,
        overTopBar: actor?.get_parent() === global.top_window_group,
        above: win?.is_above() ?? false,
        focus: global.display.focus_window?.get_title() ?? null,
    };
}

export function install(ext) {
    ext.ScreenshotAsync = async ([path], invocation) => {
        const shooter = new Shell.Screenshot();
        const stream = Gio.File.new_for_path(path).replace(null, false, Gio.FileCreateFlags.NONE, null);
        await shooter.screenshot(false, stream);
        stream.close(null);
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify(islandReport(ext))]));
    };

    ext.ClickAsync = async ([x, y], invocation) => {
        const seat = Clutter.get_default_backend().get_default_seat();
        ext._debugPointer ??= seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        const pointer = ext._debugPointer;
        const wait = ms => new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            resolve();
            return GLib.SOURCE_REMOVE;
        }));
        const now = () => GLib.get_monotonic_time();
        pointer.notify_absolute_motion(now(), x, y);
        await wait(80);
        pointer.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
        await wait(60);
        pointer.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
        await wait(300);
        const open = Object.entries(Main.panel.statusArea)
            .filter(([, item]) => item?.menu?.isOpen)
            .map(([name]) => name);
        invocation.return_value(new GLib.Variant('(s)', [JSON.stringify({
            ...islandReport(ext),
            openPanelMenus: open,
        })]));
        for (const name of open)
            Main.panel.statusArea[name].menu.close();
    };
}
