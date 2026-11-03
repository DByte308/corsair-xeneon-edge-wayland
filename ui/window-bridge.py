# SPDX-License-Identifier: GPL-3.0-or-later
"""Private session-bus receiver. Never reads files or modifies windows."""
import json
import sys
import dbus
import dbus.service
import dbus.mainloop.glib
from gi.repository import GLib

dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
name = dbus.service.BusName(sys.argv[1], bus, do_not_queue=True)

class Receiver(dbus.service.Object):
    @dbus.service.method('dev.edgeline.Windows', in_signature='s', out_signature='', sender_keyword='sender')
    def Update(self, payload, sender=None):
        # Only the compositor may supply window metadata.
        if sender != bus.get_name_owner('org.kde.KWin'):
            return
        if len(payload) > 1024 * 1024:
            return
        try:
            data = json.loads(str(payload))
        except (ValueError, TypeError):
            return
        print(json.dumps(data, separators=(',', ':')), flush=True)

receiver = Receiver(bus, '/Windows')
print('{"ready":true}', flush=True)
GLib.MainLoop().run()
