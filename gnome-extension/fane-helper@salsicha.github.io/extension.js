// Fane Helper: a tiny D-Bus service inside GNOME Shell for the Fane assistant.
//
// On Wayland, regular apps can't learn which window is focused, can't take a
// screenshot without a prompt + flash, and can't position their own windows.
// Shell extensions can, so this exposes just those operations.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'io.github.salsicha.Fane';
const OBJECT_PATH = '/io/github/salsicha/Fane';

const IFACE_XML = `
<node>
  <interface name="io.github.salsicha.Fane">
    <method name="FocusedWindow">
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="ListWindows">
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="Screenshot">
      <arg type="s" direction="in" name="path"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="PlaceWindow">
      <arg type="s" direction="in" name="title"/>
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="b" direction="in" name="above"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="ActivateWindow">
      <arg type="s" direction="in" name="title"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
  </interface>
</node>`;

Gio._promisify(Shell.Screenshot.prototype, 'screenshot', 'screenshot_finish');

function describe(win) {
    if (!win)
        return null;
    return {
        title: win.get_title() ?? '',
        app: win.get_gtk_application_id() ?? win.get_wm_class() ?? '',
        wm_class: win.get_wm_class() ?? '',
        pid: win.get_pid(),
        fullscreen: win.is_fullscreen(),
        workspace: win.get_workspace()?.index() ?? -1,
    };
}

function findWindow(title) {
    return global.get_window_actors()
        .map(a => a.meta_window)
        .find(w => w.get_title() === title);
}

class FaneService {
    FocusedWindow() {
        return JSON.stringify(describe(global.display.focus_window) ?? {});
    }

    ListWindows() {
        const wins = global.get_window_actors()
            .map(a => a.meta_window)
            .filter(w => !w.is_skip_taskbar())
            .map(describe);
        return JSON.stringify(wins);
    }

    async ScreenshotAsync([path], invocation) {
        // Only write inside $XDG_RUNTIME_DIR/fane so callers can't clobber arbitrary files.
        const allowed = GLib.build_filenamev([GLib.get_user_runtime_dir(), 'fane']) + '/';
        if (!GLib.canonicalize_filename(path, null).startsWith(allowed)) {
            invocation.return_value(new GLib.Variant('(b)', [false]));
            return;
        }
        try {
            const file = Gio.File.new_for_path(path);
            const stream = file.replace(null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
            const shooter = new Shell.Screenshot();
            await shooter.screenshot(false, stream);
            stream.close(null);
            invocation.return_value(new GLib.Variant('(b)', [true]));
        } catch (e) {
            logError(e, 'Fane Helper screenshot');
            invocation.return_value(new GLib.Variant('(b)', [false]));
        }
    }

    PlaceWindow(title, x, y, above) {
        const win = findWindow(title);
        if (!win)
            return false;
        win.move_frame(true, x, y);
        if (above)
            win.make_above();
        return true;
    }

    ActivateWindow(title) {
        const win = findWindow(title);
        if (!win)
            return false;
        win.activate(global.get_current_time());
        return true;
    }
}

export default class FaneHelperExtension extends Extension {
    enable() {
        this._service = new FaneService();
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE_XML, this._service);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._ownerId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, BUS_NAME, Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        this._dbus?.unexport();
        this._dbus = null;
        this._service = null;
    }
}
