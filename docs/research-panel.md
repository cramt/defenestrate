# Panel research: COSMIC's panel config, and hosting a tray/task list without explorer

Research notes for an optional defenestrate panel that mirrors cosmic-panel. Gathered 2026-11-25 from primary
sources only. Links are pinned to the commits that were read:

| Source | Commit |
|---|---|
| pop-os/cosmic-panel | `7e3dc12e50d80181b8f935a33e5896e3a76ec096` |
| pop-os/cosmic-applets | `c0fca14c85d3f0f2c32065dbc5b3c6382afe89d8` |
| pop-os/cosmic-comp | `0681eaf68324d2afd8cac175d1e10a22b7d34cdf` |
| pop-os/libcosmic | `009fb9d4138f25615122c4ccc42d025472da9cdd` |
| Smithay/smithay | `54cf9c10fe214066f2e69f43784aafde0d921682` |
| cairoshell/ManagedShell (Cairo Desktop, RetroBar) | `855222035e927910ca53fd3215caef056337a048` |
| reactos/reactos | `3e6033a613fbf7c5bd58aaeaa05af42514af59d5` |
| ramensoftware/windhawk-mods | `1a8b68fc7e486beb33373928b5fb030b549eb5df` |
| m417z/7-Taskbar-Tweaker | `8f9c8a2407182576a1c5df89e8588020a5651570` |
| MicrosoftDocs/sdk-api, MicrosoftDocs/win32 | `docs` branch (= learn.microsoft.com) |

Short forms below: `CP` = `https://github.com/pop-os/cosmic-panel/blob/7e3dc12`, `CA` = `…/cosmic-applets/blob/c0fca14`,
`CC` = `…/cosmic-comp/blob/0681eaf`, `MS` = `https://github.com/cairoshell/ManagedShell/blob/8552220/src`,
`RO` = `https://github.com/reactos/reactos/blob/3e6033a`.

---

## Part A: COSMIC panel

### A1. Storage layout (cosmic-config)

- Every config is a directory `<id>/v<version>/` with **one RON file per key**, file name = key name.
  User values: `$XDG_CONFIG_HOME/cosmic/<id>/v<N>/<key>`; system defaults: `$XDG_DATA_DIRS/cosmic/<id>/v<N>/<key>`
  (usually `/usr/share/cosmic/…`). A read tries the user file and falls back to the system default **per key**
  (libcosmic `cosmic-config/src/lib.rs` L231-L250 `Config::new`, L423-L460 `get`/`get_local`/`get_system_default`).
  Writes are `ron::ser::to_string_pretty` (L503).
- The panel list is `com.system76.CosmicPanel/v1/entries` = `["Panel","Dock"]`. Each name in it is its own config
  id `com.system76.CosmicPanel.<name>/v1/<key>` (`CP/cosmic-panel-config/src/container_config.rs` L42-L43, L62-L93).
  Missing keys in an entry fall back to the struct's `Default` (`CosmicConfigEntry` derive).
- Shipped defaults are the files under `CP/data/default_schema/`, installed to `/usr/share/cosmic`
  (`CP/justfile` L10, L47). **They disagree with the Rust `Default` for the Dock** (see A3); the data files are what
  users actually get.

### A2. `CosmicPanelConfig` fields (`CP/cosmic-panel-config/src/panel_config.rs` L413-L471, defaults L503-L532)

| Key | Type | Rust default | Notes |
|---|---|---|---|
| `name` | `String` | `""` | unique; also a magic string: `"Dock"` gets theme-derived padding, names containing `"panel"` get +10 priority (L598, panel_space.rs L754) |
| `anchor` | `PanelAnchor` = `Left \| Right \| Top \| Bottom` | `Top` | anchored to 3 edges: the chosen one plus both perpendicular (L100-L110) |
| `anchor_gap` | `bool` | `false` | when true, `margin` is also applied as a gap from the anchored edge (`get_effective_anchor_gap`, L628-L630) |
| `layer` | `Background \| Bottom \| Top \| Overlay` | `Top` | wlr-layer-shell layer (`xdg-shell-wrapper-config/src/lib.rs` L10-L19) |
| `keyboard_interactivity` | `None \| Exclusive \| OnDemand` | `None` | shipped entries use `OnDemand` |
| `size` | `PanelSize` = `XS \| S \| M \| L \| XL \| Custom(u32)` | `M` | see size table below |
| `output` | `CosmicPanelOuput` = `All \| Active \| Name(String)` | `All` | (sic, "Ouput"); `Name` is a connector name |
| `background` | `ThemeDefault \| Dark \| Light \| Color([f32;3])` | `ThemeDefault` | `Color` is RGB; alpha comes from `opacity` (L698-L703) |
| `plugins_wings` | `Option<(Vec<String>, Vec<String>)>` | `None` | (start = left/top, end = right/bottom) |
| `plugins_center` | `Option<Vec<String>>` | `None` | |
| `size_wings` | `Option<(Option<PanelSize>, Option<PanelSize>)>` | `None` | per-side applet size override (L549-L573) |
| `size_center` | `Option<PanelSize>` | `None` | |
| `expand_to_edges` | `bool` | `true` | false = floating, content-sized bar ("dock") |
| `padding_override` | `Option<u32>` | `None` | None → 0, except `name == "Dock"`: 4 (round/slightly-round theme) or 0 (square) (`cosmic-panel-bin/src/space/panel_space.rs` L750-L764) |
| `spacing` | `u32` | `0` | between applets |
| `border_radius_override` | `Option<u32>` | `None` | None → theme `radius_xl` if `anchor_gap`, `min(radius_xl,12)` if not expanded, else 0 (panel_space.rs L727-L739) |
| `border_width` | `f32` | `0.0` | outline |
| `exclusive_zone` | `bool` | `true` | **effective value is `exclusive_zone \|\| !autohide_enabled()`** (L657-L659): a non-autohiding panel always reserves space, whatever this says |
| `autohide` | `Never \| OnOverlap \| Always` | `Never` | legacy `Option<AutoHideBehavior>` still parsed (L328-L357) |
| `autohide_behavior` | `{wait_time, transition_time, handle_size, unhide_delay}` ms/px | `{1000, 200, 4, 200}` | L294-L310 |
| `margin` | `u16` | `4` | |
| `opacity` | `f32` | `0.8` | |
| `autohover_delay_ms` | `Option<u32>` | `Some(500)` | hover-to-open popups |
| `padding_overlap` | `f32` | `0.5` | clamped 0..1 |
| `keep_style_on_maximize` | `bool` | `false` | when false and a window is maximized: opacity 1.0, and (unless autohide) expand to edges, margin 0, radius 0, border 0, no gap (`maximize()`, L740-L753) |

`PanelSize` pixel values (L143-L232; `Custom(s)` first rounds `max(s,16)` down to a multiple of 4):

| | XS | S | M | L | XL | Custom(s) |
|---|---|---|---|---|---|---|
| symbolic icon | 16 | 20 | 28 | 32 | 48 | s/2 |
| symbolic padding | 8 | 10 | 14 | 16 | 16 | s/4 |
| full-colour icon | 24 | 32 | 40 | 48 | 56 | s·7/10 |
| full-colour padding | 4 | 4 | 8 | 8 | 12 | s·3/20 |
| bar thickness range (`get_dimensions`, L706-L733) | 8..61 | 8..81 | 8..101 | 8..121 | 8..141 | 8..2s+1 (all + gap) |

The bar's actual thickness is content-driven within that range (applet icon + 2×padding + panel padding).
Several panels on one output are ordered by `get_priority` (L587-L602): expand_to_edges +10000, not autohide +1000,
margin 0 +200, no anchor_gap +100, name contains "panel" +10.

### A3. What COSMIC ships (`CP/data/default_schema/…`)

**Panel**: `anchor: Top, anchor_gap: false, size: XS, expand_to_edges: true, exclusive_zone: true, margin: 0,
opacity: 1.0, layer: Top, keyboard_interactivity: OnDemand, output: All, autohide: Never`.
- wings start: `CosmicPanelWorkspacesButton`, `CosmicPanelAppButton`
- center: `CosmicAppletTime`
- wings end: `InputSources, A11y, StatusArea, Tiling, Audio, Bluetooth, Network, Battery, Notifications, Power`
  (all `com.system76.CosmicApplet…`)

**Dock** (data files): `anchor: Bottom, anchor_gap: true, size: M, expand_to_edges: false, exclusive_zone: true,
margin: 4, opacity: 1.0`. Center only: `CosmicPanelLauncherButton, CosmicPanelWorkspacesButton, CosmicPanelAppButton,
CosmicAppList, CosmicAppletMinimize`. The Rust fallback (`container_config.rs` L180-L213) instead has
`size: L, anchor_gap: false, margin: 0, exclusive_zone: false`; only reached if the data files are absent.

So in stock COSMIC the clock and tray live in the top Panel; the task list lives in the Dock. Both entries exist
by default; "off by default" for defenestrate means `entries = []` (a valid state in COSMIC: no panels).

### A4. Applets

**App list, `com.system76.CosmicAppList`** (`CA/cosmic-app-list/cosmic-app-list-config/src/lib.rs` L9-L32)
- Keys: `favorites: Vec<String>` (desktop-entry ids; shipped: firefox, CosmicFiles, CosmicEdit, CosmicTerm,
  CosmicStore, CosmicSettings), `filter_top_levels: Option<ToplevelFilter>` with `ActiveWorkspace | ConfiguredOutput`
  (shipped `None`), `enable_drag_source: bool` (default true).
- Filter (`CA/cosmic-app-list/src/app.rs` L638-L666): `None` = every window everywhere; `ActiveWorkspace` = windows on
  an active workspace; `ConfiguredOutput` = on the panel's own output **and** an active workspace.
- Layout: pinned group (favorites, in config order; running pinned apps stay there) | divider | "active" group
  (running, unpinned apps, appended as they appear) (L1950-L1995). Windows are grouped per app (desktop entry).
  When it doesn't fit, both groups are truncated (min 4 slots) and overflow buttons open popups (`OpenFavorites`,
  `OpenActive`, L1931-L1948, L1569, L1637).
- Indicator next to the icon, on the edge-facing side: nothing for 0 windows, a dot for 1, a wider bar for ≥2;
  accent colour if focused (L224-L290).
- Clicks on an app icon (L297-L313):
  - left, 0 windows → launch; 1 window → `Toggle`: **minimize if it's focused, else activate** (L1082-L1094);
    ≥2 windows → popup listing the windows (thumbnails; left = Toggle, middle = close, × button = close, L498-L523)
  - middle → launch a new instance
  - right → context menu (new window, desktop actions, window list, pin/unpin, quit/quit all; L2097-L2210)
  - no scroll handling.
  - drag pinned icons to reorder; drop a .desktop to pin.

**Time, `com.system76.CosmicAppletTime`** (`CA/cosmic-applets-config/src/time.rs` L8-L29; view `CA/cosmic-applet-time/src/window.rs`)
- Keys/defaults: `military_time: false`, `show_seconds: false`, `first_day_of_week: 6` (0 = Monday … 5 = Saturday,
  anything else Sunday, L133-L141), `show_date_in_top_panel: true`, `show_weekday: false`, `format_strftime: ""`
  (non-empty overrides everything; split on whitespace into lines on vertical panels).
- Horizontal: one line, ICU medium month-day + time (+weekday if `show_weekday`) (L259-L310). Vertical: date pieces,
  a rule, then time split on spaces **and colons** into stacked lines (L192-L257).
- Left press → popup: long date, weekday, month calendar with prev/next, "Date, time and calendar settings" (L697, L728+).

**Status area, `com.system76.CosmicAppletStatusArea`** (`CA/cosmic-applet-status-area/src/components/app.rs`)
- A StatusNotifierItem host; it can also run the watcher (`--status-notifier-watcher`, `lib.rs` L12-L22).
- Icons in registration order. Left click → SNI `Activate(0,0)`, or opens the menu if the item is `ItemIsMenu` or
  Activate fails (L612-L638). Right click → dbusmenu popup (L541-L542). Wheel → SNI `Scroll` (L397-L408, L640).
  Hovering another icon while a menu is open switches the menu.
- Overflow (L81-L105, L533-L575): button count = available length / (icon + 2·padding); if items don't fit, show
  `n-1` and an arrow button (`go-down` on Top, `go-up` on Bottom) that opens a popup with the rest.
  **Quirk: vertical panels truncate but never draw the overflow button**, so overflowed items are unreachable there.

### A5. Exclusive zone and cosmic-comp gaps

- The panel sets its layer-surface exclusive zone to its thickness when not autohiding (panel_space.rs L1319-L1328);
  with `anchor_gap` it also sets a margin. Smithay subtracts **exclusive zone + margin** on the anchored edge when
  computing `non_exclusive_zone` (smithay `src/desktop/wayland/layer.rs` L367-L385). Autohide panels set the zone
  only while hidden (handle-sized) (L1329-L1340).
- Tiling: available rect = `non_exclusive_zone` inset by the **outer** gap on all four sides, windows separated by
  the **inner** gap (each side gets `inner/2`) (`CC/src/shell/layout/tiling/mod.rs` L2989-L3072). Gaps come from the
  theme, `cosmic_theme gaps: (outer, inner)`, default `(0, 8)` (libcosmic `cosmic-theme/src/model/theme.rs` L114, L979).
  So the panel and the outer gap stack: no gap between panel and outer gap.
- Maximized windows take the full `non_exclusive_zone`, no gaps (`CC/src/shell/layout/floating/mod.rs` L385-L395).
- Fullscreen: Top-layer surfaces (the panel) are skipped while the workspace has a focused fullscreen window
  (`CC/src/shell/focus/order.rs` L297, L408). The panel never covers a fullscreen game.

---

## Part B: Hosting the Windows notification area without explorer

### B1. The Shell_NotifyIcon wire protocol

**Discovery**: `Shell_NotifyIconW` does `FindWindowW(L"Shell_TrayWnd", NULL)` and
`SendMessage(hTray, WM_COPYDATA, (WPARAM)nid.hWnd, &cds)`; no tray window → returns FALSE
(`RO/dll/win32/shell32/systray.cpp` L137-L139, L207-L210). `FindWindow` returns the topmost match, which is why
ManagedShell keeps its `Shell_TrayWnd` `HWND_TOPMOST` and re-raises it on a 100 ms timer when another one appears
(`MS/ManagedShell.WindowsTray/TrayService.cs` L347-L385). The window can be invisible: ManagedShell creates a
`WS_POPUP`, `WS_EX_TOPMOST|WS_EX_TOOLWINDOW` window and strips `WS_VISIBLE` if anyone shows it (L237-L248, L299-L322),
plus a `TrayNotifyWnd` child (L324-L345).

**`COPYDATASTRUCT.dwData` values** handled by a tray (`TrayService.cs` L168-L234; `RO/sdk/include/reactos/undocshell.h` L1024-L1026):
- `0` = SHAppBarMessage (`APPBARMSGDATAV3`, see B4)
- `1` = Shell_NotifyIcon (`TRAYNOTIFYDATAW`)
- `3` = Shell_NotifyIconGetRect (`NOTIFYICONIDENTIFIER` internal)

**Payload for dwData = 1 on 64-bit Windows 10/11.** Handles are carried as 32-bit values so 32- and 64-bit
programs share one format (comment in windhawk-mods `mods/tray-ghost-cleaner.wh.cpp` L104-L131; same layout in
`MS/ManagedShell.Interop/NativeMethods.Shell32.cs` L366-L395). Packed, little-endian, UTF-16:

```
off  size  field
  0     4  dwSignature      = 0x34753423   (NI_NOTIFY_SIG, undocshell.h L60)
  4     4  dwMessage        NIM_ADD 0, NIM_MODIFY 1, NIM_DELETE 2, NIM_SETFOCUS 3, NIM_SETVERSION 4
  8     4  cbSize           the size the *app* declared (its own struct version/bitness)
 12     4  hWnd             32-bit HWND (zero-extend / sign-extend to HWND)
 16     4  uID
 20     4  uFlags           NIF_MESSAGE 1, ICON 2, TIP 4, STATE 8, INFO 0x10, GUID 0x20, REALTIME 0x40, SHOWTIP 0x80
 24     4  uCallbackMessage
 28     4  hIcon            32-bit HICON
 32   256  szTip[128]
288     4  dwState          NIS_HIDDEN 1, NIS_SHAREDICON 2
292     4  dwStateMask
296   512  szInfo[256]
808     4  uVersion / uTimeout (union)
812   128  szInfoTitle[64]
940     4  dwInfoFlags
944    16  guidItem
960     4  hBalloonIcon
964        end  (NOTIFYICONDATAW = 956 bytes; whole payload 964)
```

Treat `cbSize` only as an upper bound alongside `cbData`: older apps send V1/V2/V3 structs, and only fields within
their size are meaningful (an app without `guidItem` never sets NIF_GUID). ReactOS's shell32 normalizes cbSize,
Windows's doesn't (tray-ghost-cleaner L134-L137 vs systray.cpp L141-L174). Return non-zero from WM_COPYDATA for
success; ManagedShell returns FALSE for an `NIM_MODIFY` of an icon it doesn't know (and adds it anyway)
(`NotificationArea.cs` L380-L387).

**Identity**: an icon is `(hWnd, uID)`, or `guidItem` when NIF_GUID is set (`NotifyIcon.cs` L569-L580).

**dwData = 3 (Shell_NotifyIconGetRect)**: 40 bytes: `dwMagic 0x34753423, dwMessage, cbSize, pad, hWnd(32-bit), uID,
guidItem` (`NativeMethods.Shell32.cs` L397-L407). Answer `MAKELPARAM(left, top)` for message 1. For message 2,
ManagedShell answers `(right, bottom)` (`NotificationArea.cs` L255-L270) but 7-Taskbar-Tweaker's comment says
`(w, h)` (`dll/wnd_proc.c` L29-L37). **They disagree; check on a real Windows before relying on it.**

**Versions and callbacks.** Apps opt into `NOTIFYICON_VERSION_4` (4) with `NIM_SETVERSION`; versions above 4 are
rejected (`NotificationArea.cs` L430-L445). Callback is `SendNotifyMessage(hWnd, uCallbackMessage, wParam, lParam)`
(`NotifyIcon.cs` L506-L529):

| | wParam | lParam |
|---|---|---|
| v0 / v3 | `uID` | event (WM_MOUSEMOVE, WM_LBUTTONDOWN, …) |
| v4 | `MAKELPARAM(x, y)` anchor in screen coords (icon's top-left for keyboard events) | `LOWORD` = event, `HIWORD` = uID (16-bit IDs in v4) |

(learn.microsoft.com NOTIFYICONDATAW, "uCallbackMessage" remarks.) Events Explorer-compatible hosts send
(ManagedShell `NotifyIcon.cs` L384-L503, which notes Explorer sends these for v3 too):
- hover: `WM_MOUSEHOVER`… then `NIN_POPUPOPEN` (0x406) on enter / `NIN_POPUPCLOSE` (0x407) on leave (v4 only);
  `WM_MOUSEMOVE` while moving.
- left: `WM_LBUTTONDOWN` (or `WM_LBUTTONDBLCLK` within double-click time), then `WM_LBUTTONUP` + `NIN_SELECT` (0x400).
- right: `WM_RBUTTONDOWN`, then `WM_RBUTTONUP` + `WM_CONTEXTMENU` (v ≥ 3).
- middle: `WM_MBUTTONDOWN`/`UP`. Keyboard activation: `NIN_KEYSELECT` (0x401).
- balloons (v ≥ 2): `NIN_BALLOONSHOW` 0x402, `NIN_BALLOONHIDE` 0x403, `NIN_BALLOONTIMEOUT` 0x404,
  `NIN_BALLOONUSERCLICK` 0x405 (Shell_NotifyIcon docs, remarks).

**Foreground**: on mouse-down, call `GetWindowThreadProcessId(icon.hWnd)` + `AllowSetForegroundWindow(pid)` before
sending, so the app's `SetForegroundWindow` + `TrackPopupMenu` works and the menu closes on outside click
(`NotifyIcon.cs` L409-L413). The host can only grant what it has: it must itself be foreground or meet
SetForegroundWindow's rules (learn.microsoft.com SetForegroundWindow remarks). Shell32 does the reverse for
`NIM_SETFOCUS` (grants the tray's pid; systray.cpp L200-L205).

**Dead icons**: before sending a callback, `IsWindow(hWnd)`; if gone, drop the icon. Explorer and ReactOS only
detect this on mouse-over (`RO/base/shell/explorer/syspager.cpp` L1108-L1121; `NotifyIcon.cs` L531-L541).

**Icons**: the HICON belongs to the app, which may `DestroyIcon` right after the call. Copy or rasterize it
synchronously inside the WM_COPYDATA handler: ReactOS `ImageList_AddIcon`s it (syspager.cpp L774-L791), ManagedShell
converts it to a bitmap immediately (`NotificationArea.cs` L318-L338). HICONs are session-wide USER objects, so the
32-bit value is usable cross-process. `NIS_SHAREDICON` means "reuse the hIcon of another icon of mine"; look it up
among existing icons (syspager.cpp L777-L785). `NIS_HIDDEN` hides without removing.

**TaskbarCreated**: on startup, `SendNotifyMessage(HWND_BROADCAST, RegisterWindowMessage(L"TaskbarCreated"), 0, 0)`
(`TrayService.cs` L116-L126; syspager.cpp L1361-L1363). Apps "should assume that any taskbar icons it added have
been removed and add them again"; Windows 10 Explorer also broadcasts it on primary-display DPI change
(learn.microsoft.com "The Taskbar", Taskbar Creation Notification). This is the **only** recovery path for apps
started before the host. Apps whose earlier `NIM_ADD` failed because no tray existed generally don't retry by
themselves. Use `SendNotifyMessage`, not `SendMessage`, so a hung window can't stall the shell.

**System icons**: Explorer loads the SysTray shell service object `{35CEC8A3-2BE6-11D2-8773-92E220524153}`
(stobject.dll) via `IOleCommandTarget::Exec(CGID_ShellServiceObject, OLECMDID_NEW)`; ManagedShell does the same when
it is the shell (`ShellServiceObject.cs` L10-L27). On Win10+ the volume/network/battery icons are partly owned by
Explorer's UWP flyouts; ManagedShell hides the volume GUID when it's the shell (`NotificationArea.cs` L291-L294) and
redirects some Win11 icons to the Action Center (`NotifyIcon.cs` L542-L556).

**Balloons (NIF_INFO)**: these reach the host as plain data (szInfo/szInfoTitle/dwInfoFlags); the host renders them
or drops them. ManagedShell raises an event and queues them (`NotificationArea.cs` L452-L470). Toast notifications
(WinRT) are a separate system (ShellExperienceHost) and do not come through this path.

### B2. Prior art and gotchas

- **ManagedShell** (C#, powers Cairo and RetroBar): `TrayService.cs` (window + WM_COPYDATA demux), `NotificationArea.cs`
  (icon bookkeeping), `NotifyIcon.cs` (callbacks), `ExplorerTrayService.cs` (pulls icons out of a running Explorer:
  not needed here). The most complete working reference; it runs on Win11 22H2+.
- **ReactOS explorer**: `base/shell/explorer/syspager.cpp` (icons, callbacks), `trayntfy.cpp` (TrayNotifyWnd +
  clock), `appbar.cpp` (appbar manager + work area), and shell32's sender side `dll/win32/shell32/systray.cpp`.
  Native-pointer layout and v3 semantics only (syspager.cpp L840-L843 warns on v4), so **don't copy its struct for
  x64 Windows**.
- **Windhawk mods** `tray-ghost-cleaner.wh.cpp` (packed 32-bit-handle layout, cbSize caveat), 7-Taskbar-Tweaker
  `wnd_proc.c` (GetRect message).
- **UIPI**: messages from a lower-integrity sender to a higher-integrity window are blocked
  (learn.microsoft.com ChangeWindowMessageFilterEx). Consequences:
  - elevated app → medium host: WM_COPYDATA goes high→low: allowed, so icons arrive.
  - host → elevated app callbacks and `TaskbarCreated`: low→high: **blocked for a plain medium host**. A uiAccess
    host bypasses UIPI, so defenestrate's uiAccess build gets this for free. The non-uiAccess fallback won't be able
    to click elevated apps' icons; Explorer has the same limitation.
- **Topmost fight**: any other process registering `Shell_TrayWnd` steals icons from that point on (FindWindow returns
  the topmost one). ManagedShell's 100 ms re-raise timer is the prior art.
- **Explorer child probing**: some apps walk `Shell_TrayWnd > TrayNotifyWnd > SysPager > ToolbarWindow32` to locate
  icons (pre-GetRect technique; ReactOS creates `SysPager`, syspager.cpp L194). ManagedShell only provides
  `TrayNotifyWnd`, sized to the tray rect via `SetTrayHostSizeData` (TrayService.cs L103-L114), and that's enough for
  the common apps.
- **Unknown messages**: ManagedShell forwards `WM_COPYDATA`, `WM_COMMAND`, `WM_ACTIVATEAPP` and `>= WM_USER` it
  doesn't handle to a real Explorer tray if one exists (L251-L282); without Explorer just DefWindowProc them.
- **Anticheat**: a foreign process owning `Shell_TrayWnd` is normal (Cairo/RetroBar users exist), but nothing here
  was checked against specific anticheats.

### B3. Task list without explorer

- **Shell hook**: `RegisterShellHookWindow(hwnd)` + `RegisterWindowMessage(L"SHELLHOOK")`; wParam = `HSHELL_*`,
  lParam = HWND (learn.microsoft.com RegisterShellHookWindow). `HSHELL_WINDOWCREATED`/`DESTROYED`/`ACTIVATED` concern
  "top-level, unowned" windows (ShellProc docs). `HSHELL_FLASH` (0x8006) and `HSHELL_RUDEAPPACTIVATED` (0x8004) are
  the documented values with the high bit set. ManagedShell uses it when it is the shell, together with
  `SetTaskmanWindow(hookWnd)` (TasksService.cs L91-L101) and handles `FLASH` by marking the window flashing unless
  it's active (L406-L421). **Unverified**: whether every HSHELL_* message is delivered with no Explorer and no
  `SetShellWindow`/`SetTaskmanWindow` caller. defenestrate already tracks windows with `SetWinEventHook`
  (`shell/src/desktop.rs` L418), so the shell hook would only add `HSHELL_FLASH` (no WinEvent equivalent) and
  `HSHELL_GETMINRECT`.
- **Which windows get a button** (ManagedShell `ApplicationWindow.cs` L353-L368, L404-L429): visible, and
  (unowned **or** `WS_EX_APPWINDOW`), not `WS_EX_TOOLWINDOW`, not (`WS_EX_NOACTIVATE` without APPWINDOW), no
  `ITaskList_Deleted` prop, not DWM-cloaked (`DWMWA_CLOAKED`), not an immersive shell class. ManagedShell also
  re-checks on `EVENT_OBJECT_CLOAKED/UNCLOAKED` (TasksService.cs L104-L121).
- **Icons** (ApplicationWindow.cs L498-L545), all with `SendMessageTimeout(SMTO_ABORTIFHUNG, 1000 ms)`:
  `WM_GETICON ICON_SMALL2 (2)` → `ICON_SMALL (0)` → `GetClassLongPtr(GCLP_HICONSM)` → `GCLP_HICON` →
  `WM_QUERYDRAGICON` → the exe's icon. For large: `WM_GETICON ICON_BIG (1)` first. UWP windows
  (`ApplicationFrameWindow`) need the package's logo instead.
- **ITaskbarList**: the in-process implementation finds the tray window and sends `WM_USER+n` messages to the HWND
  stored in the tray's `TaskbandHWND` window property; ManagedShell sets that prop to its hook window and logs/handles
  `WM_USER+50` ActivateTab, `+60` MarkFullscreenWindow, `+64` SetProgressValue, `+65`… (TasksService.cs L520-L668,
  L699-L736). With no `Shell_TrayWnd` there's nobody to receive them; calls fail or are no-ops, and well-behaved apps
  don't call ITaskbarList3 before they get the registered `TaskbarButtonCreated` message, which "must be received by
  your application before it calls any ITaskbarList3 method" (learn.microsoft.com ITaskbarList3). ManagedShell sends
  it to each new taskbar window when it is the shell (TasksService.cs L259-L282). **Unverified**: that every app
  handles the failure harmlessly. Not sending `TaskbarButtonCreated` is the safe default.

### B4. Work area and appbars

- `SystemParametersInfo(SPI_SETWORKAREA, 0, &rect, flags)` sets the work area of **the monitor containing the rect**
  (learn.microsoft.com SystemParametersInfoW, SPI_SETWORKAREA). ReactOS sets one per monitor
  (`RO/base/shell/explorer/appbar.cpp` L340-L371). Note: the comment in defenestrate `shell/src/desktop.rs` L588-L593
  says Windows has only the primary monitor's work area; the docs and ReactOS say otherwise.
- Maximized windows and `MONITORINFO.rcWork`-aware apps respect the work area; the WM tiles itself, so for
  defenestrate the work area matters for apps that maximize or position themselves (and for maximize in
  floating mode).
- `SHAppBarMessage` is just WM_COPYDATA `dwData = 0` to `Shell_TrayWnd` with `APPBARMSGDATAV3`
  `{APPBARDATAV2 abd (cbSize, hWnd u32, uCallbackMessage, uEdge, rc, lParam i64), dwMessage, pad, hSharedMemory i64,
  dwSourceProcessId, pad}`; QUERYPOS/SETPOS/GETTASKBARPOS pass the struct through `SHLockShared(hSharedMemory, pid)`
  (`NativeMethods.Shell32.cs` L24-L43; `AppBarManager.cs` L63-L77). **Without a `Shell_TrayWnd` nobody manages
  appbars**: there's no system appbar service separate from the taskbar. With one, the host is the appbar manager.
  ManagedShell-as-shell answers `ABM_GETTASKBARPOS` (fills the tray rect + edge), `GETSTATE`, `GETAUTOHIDEBAR(EX)`,
  acks `ACTIVATE`/`WINDOWPOSCHANGED`, and ignores QUERYPOS/SETPOS (AppBarManager.cs L40-L60, L86-L90); its own bars
  skip SHAppBarMessage and call `SPI_SETWORKAREA` directly (L248-L281, L491-L496).
- Third-party appbars (e.g. some docks, OBS-style overlays) exist but are rare on a games console. Answering
  `ABM_GETTASKBARPOS` matters more: games and launchers use it to avoid the taskbar.
- `SPIF_SENDCHANGE` broadcasts `WM_SETTINGCHANGE` synchronously; defenestrate already avoids it because it stalled
  first boot (desktop.rs L588-L593). ReactOS sends it only with a desktop window present (appbar.cpp L444). If apps
  need to hear about a changed work area, `SendNotifyMessage(HWND_BROADCAST, WM_SETTINGCHANGE, SPI_SETWORKAREA, 0)`
  is the non-blocking variant.

---

## Implications for defenestrate

1. **Config mirror**: model `entries: Vec<String>` + one `CosmicPanelConfig` per name, same key names and RON-shaped
   values (`Top`, `XS`, `Custom(40)`, `Some((["…"],["…"]))`). Default `entries = []` gives "off by default" without
   inventing a new `enabled` key. Ship COSMIC's Panel/Dock as presets with the data-file values, not the Rust fallback.
   Applet ids to support: `com.system76.CosmicAppList`, `com.system76.CosmicAppletTime`,
   `com.system76.CosmicAppletStatusArea`; others are unknown → ignore or error.
2. **Types to get right**: `exclusive_zone` is effectively forced on unless autohide; encode that (e.g. an enum
   `Reserve::Always | AutohideWith{exclusive: bool}`) rather than a bool the code silently overrides, but keep the
   on-disk key. `PanelSize::Custom` rounds to multiples of 4. `Color` is RGB + `opacity`.
3. **Exact-clone details**: app-list Toggle (focused → minimize), middle-click launches, ≥2 windows → popup;
   status-area left = Activate / right = menu / wheel = Scroll, overflow arrow only on horizontal panels; clock
   splits on `:` vertically. Panel goes opaque/edge-to-edge while a window is maximized unless
   `keep_style_on_maximize`. Panel hides under a focused fullscreen window.
4. **Tiling**: reserve `thickness + (anchor_gap ? margin : 0)` on the anchored edge of each output the panel is on,
   then apply the outer gap inside that. Maximized = full reserved rect, no gaps. Set `SPI_SETWORKAREA` per monitor
   to the same rect so self-positioning apps agree.
5. **Tray host**: one hidden topmost `Shell_TrayWnd` + `TrayNotifyWnd` child, a packed 964-byte parser keyed on
   `cbData` and cbSize bounds, copy HICONs inside the handler, `(hWnd,uID)|guid` identity, v0/v3/v4 callback packing,
   `AllowSetForegroundWindow` before clicks, `TaskbarCreated` broadcast at start. Answer `ABM_GETTASKBARPOS` and
   GetRect once the window exists. Run it in the uiAccess build so elevated apps' icons are clickable.
   **Design fork**: whether the tray host runs when no panel shows a status area. It changes what apps see: with no
   `Shell_TrayWnd`, Shell_NotifyIcon fails and some apps warn or exit-on-close instead of minimizing to tray.
6. **Task list**: reuse the WM's window model; add `RegisterShellHookWindow` only for `HSHELL_FLASH`. Group by
   AppUserModelID/exe to mimic COSMIC's per-desktop-entry grouping; "favorites" need a Windows equivalent of
   desktop-entry ids (AUMID or exe path).

### Riskiest unknowns

- The `Shell_NotifyIconGetRect` second answer (right/bottom vs width/height): test on real Windows.
- HSHELL_* delivery with no Explorer and no `SetTaskmanWindow`/`SetShellWindow`; whether `SetTaskmanWindow` has side
  effects for defenestrate's own hotkeys.
- Kernel anticheats' reaction to a non-Explorer `Shell_TrayWnd` (and to uiAccess) has not been checked.
- Win11 system icons (volume/network/battery) without Explorer's flyouts: likely missing, or clicking them does
  nothing.
- ITaskbarList/ `TaskbarButtonCreated` behaviour across real apps (Steam, Discord, browsers): only ManagedShell's
  experience backs "harmless".
