# Physical pixels the pointer moves per mouse count, on Windows. Compare
# with bench/pointer/linux.py's figure for the same mouse: they should agree
# to about 1 % (cli/src/input.rs is what makes them).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File windows.ps1
#
# Then move the mouse sideways in single strokes, stopping between them,
# without the pointer reaching a screen edge. Each stroke prints the raw
# counts (WM_INPUT), how far GetCursorPos moved in physical pixels, and the
# ratio; Escape or closing the window ends it with the median.

$ErrorActionPreference = 'Stop'
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public class PointerBench : Form {
    [StructLayout(LayoutKind.Sequential)] struct RAWINPUTDEVICE { public ushort UsagePage, Usage; public uint Flags; public IntPtr Target; }
    [StructLayout(LayoutKind.Sequential)] struct RAWINPUTHEADER { public uint Type, Size; public IntPtr Device, WParam; }
    [StructLayout(LayoutKind.Sequential)] struct RAWMOUSE { public ushort Flags; public uint Buttons; public uint RawButtons; public int LastX, LastY; public uint Extra; }
    [StructLayout(LayoutKind.Sequential)] struct POINT { public int X, Y; }
    [DllImport("user32.dll")] static extern bool RegisterRawInputDevices(RAWINPUTDEVICE[] d, uint n, uint size);
    [DllImport("user32.dll")] static extern uint GetRawInputData(IntPtr h, uint cmd, IntPtr data, ref uint size, uint headerSize);
    [DllImport("user32.dll")] static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr c);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
    [DllImport("user32.dll")] static extern uint GetDpiForSystem();

    long dx, dy, packets;
    POINT start;
    bool moving;
    readonly Timer idle = new Timer { Interval = 300 };
    public readonly List<double> Ratios = new List<double>();

    public static void Aware() { SetProcessDpiAwarenessContext(new IntPtr(-4)); } // per-monitor v2: physical pixels

    public PointerBench() {
        Text = "pointer bench: sideways strokes, Escape ends";
        Width = 600; Height = 120; KeyPreview = true;
        KeyDown += (s, e) => { if (e.KeyCode == Keys.Escape) Close(); };
        idle.Tick += (s, e) => EndStroke();
        Console.WriteLine("system DPI {0}", GetDpiForSystem());
    }

    protected override void OnHandleCreated(EventArgs e) {
        base.OnHandleCreated(e);
        // RIDEV_INPUTSINK: every packet, whichever window has focus.
        var dev = new[] { new RAWINPUTDEVICE { UsagePage = 1, Usage = 2, Flags = 0x100, Target = Handle } };
        if (!RegisterRawInputDevices(dev, 1, (uint)Marshal.SizeOf(typeof(RAWINPUTDEVICE)))) throw new Exception("RegisterRawInputDevices");
        GetCursorPos(out start);
    }

    protected override void WndProc(ref Message m) {
        if (m.Msg == 0x00FF) { // WM_INPUT
            uint size = 0, hs = (uint)Marshal.SizeOf(typeof(RAWINPUTHEADER));
            GetRawInputData(m.LParam, 0x10000003, IntPtr.Zero, ref size, hs);
            IntPtr buf = Marshal.AllocHGlobal((int)size);
            try {
                if (GetRawInputData(m.LParam, 0x10000003, buf, ref size, hs) == size) {
                    var h = (RAWINPUTHEADER)Marshal.PtrToStructure(buf, typeof(RAWINPUTHEADER));
                    var r = (RAWMOUSE)Marshal.PtrToStructure(buf + (int)hs, typeof(RAWMOUSE));
                    // RIM_TYPEMOUSE, relative motion only (not a tablet's absolute).
                    if (h.Type == 0 && (r.Flags & 1) == 0 && (r.LastX != 0 || r.LastY != 0)) {
                        if (!moving) { moving = true; dx = dy = packets = 0; }
                        dx += r.LastX; dy += r.LastY; packets++;
                        idle.Stop(); idle.Start();
                    }
                }
            } finally { Marshal.FreeHGlobal(buf); }
        }
        base.WndProc(ref m);
    }

    // The pointer is still once no packet came for 300 ms, so the stroke's
    // movement is between the last two stills.
    void EndStroke() {
        idle.Stop();
        POINT end; GetCursorPos(out end);
        if (moving && Math.Abs(dx) >= 200) {
            int left = GetSystemMetrics(76), top = GetSystemMetrics(77);
            int right = left + GetSystemMetrics(78) - 1, bottom = top + GetSystemMetrics(79) - 1;
            bool edge = end.X <= left || end.X >= right || end.Y <= top || end.Y >= bottom;
            double ratio = (double)(end.X - start.X) / dx;
            Console.WriteLine("{0,7} counts in {1,5} packets -> {2,7} px: {3:F4} px/count{4}", dx, packets, end.X - start.X, ratio, edge ? "  (hit an edge, dropped)" : "");
            if (!edge) Ratios.Add(ratio);
        }
        moving = false;
        start = end;
    }
}
'@

[PointerBench]::Aware()
$form = New-Object PointerBench
[System.Windows.Forms.Application]::Run($form)
$r = $form.Ratios | Sort-Object
if ($r.Count) {
    '{0} strokes, median {1:F4} px/count (min {2:F4}, max {3:F4})' -f $r.Count, $r[[int][math]::Floor($r.Count / 2)], $r[0], $r[-1]
}
$m = Get-ItemProperty 'HKCU:\Control Panel\Mouse'
'MouseSpeed {0} MouseSensitivity {1} LogPixels {2}' -f $m.MouseSpeed, $m.MouseSensitivity, (Get-ItemProperty 'HKCU:\Control Panel\Desktop').LogPixels
