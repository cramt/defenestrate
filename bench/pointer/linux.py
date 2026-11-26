#!/usr/bin/env python3
"""Physical pixels the pointer moves per mouse count, on Linux (any Wayland
compositor; written for cosmic-comp). Compare with bench/pointer/windows.ps1's
figure for the same mouse.

    nix shell nixpkgs#libinput 'nixpkgs#python3.withPackages(p: [p.pygobject3])' nixpkgs#gtk4 \\
      --command python3 linux.py

Raw counts come from `libinput debug-events` (the unaccelerated deltas,
which are the device's counts), run through sudo since it reads
/dev/input; where the pointer went comes from a fullscreen GTK window's
motion events, in logical pixels, times the window's scale. Move the mouse
sideways in single strokes, stopping between them; Escape ends it with the
median. libinput debug-events reads the devices alongside the compositor
without grabbing them, so the pointer keeps working.
"""

import re
import statistics
import subprocess
import sys
import threading

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

# "-event5   POINTER_MOTION   +1.234s	  1.60/  0.00 ( +1.00/ +0.00)"
MOTION = re.compile(r"POINTER_MOTION\s.*\(\s*([-+]?\d+\.\d+)/\s*([-+]?\d+\.\d+)\)")


class Bench:
    def __init__(self, app):
        self.lock = threading.Lock()
        self.dx = 0.0
        self.packets = 0
        self.moving = False
        self.idle = None
        self.x = None  # logical, last motion event
        self.start = None
        self.ratios = []
        self.last_scale = 1.0
        self.win = Gtk.ApplicationWindow(application=app, title="pointer bench")
        self.win.fullscreen()
        label = Gtk.Label(label="Sideways strokes, stopping between them. Escape ends.")
        self.win.set_child(label)
        motion = Gtk.EventControllerMotion()
        motion.connect("motion", self.on_motion)
        self.win.add_controller(motion)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", lambda _c, keyval, *_: keyval == 0xFF1B and self.win.close())
        self.win.add_controller(keys)
        self.win.present()
        threading.Thread(target=self.read_counts, daemon=True).start()

    def scale(self):
        surface = self.win.get_surface()
        return surface.get_scale() if surface else 1.0

    def on_motion(self, _c, x, _y):
        self.x = x

    def read_counts(self):
        proc = subprocess.Popen(["sudo", "libinput", "debug-events"], stdout=subprocess.PIPE, text=True)
        for line in proc.stdout:
            m = MOTION.search(line)
            if not m:
                continue
            with self.lock:
                if not self.moving:
                    self.moving, self.dx, self.packets = True, 0.0, 0
                self.dx += float(m.group(1))
                self.packets += 1
            GLib.idle_add(self.restart_idle)

    def restart_idle(self):
        if self.idle:
            GLib.source_remove(self.idle)
        self.idle = GLib.timeout_add(300, self.end_stroke)

    # Still for 300 ms: the stroke's movement is between the last two stills.
    def end_stroke(self):
        self.idle = None
        with self.lock:
            dx, packets, self.moving = self.dx, self.packets, False
        if self.x is not None and self.start is not None and abs(dx) >= 200:
            self.last_scale = self.scale()
            px = (self.x - self.start) * self.last_scale
            width = self.win.get_width()
            edge = self.x <= 0 or self.x >= width - 1
            ratio = px / dx
            print(f"{dx:7.0f} counts in {packets:5} packets -> {px:7.1f} px: {ratio:.4f} px/count{'  (hit an edge, dropped)' if edge else ''}")
            if not edge:
                self.ratios.append(ratio)
        self.start = self.x
        return GLib.SOURCE_REMOVE


def main():
    # The password prompt has to come before the window covers the terminal.
    subprocess.run(["sudo", "-v"], check=True)
    app = Gtk.Application(application_id="dev.defenestrate.PointerBench")
    bench = []
    app.connect("activate", lambda a: bench.append(Bench(a)))
    app.run([])
    if bench and bench[0].ratios:
        r = bench[0].ratios
        print(f"{len(r)} strokes, median {statistics.median(r):.4f} px/count (min {min(r):.4f}, max {max(r):.4f}), scale {bench[0].last_scale}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
