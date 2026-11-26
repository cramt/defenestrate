//! The window manager's state: the monitors, each with its own workspaces,
//! which windows are on which, how they tile (tree.rs), and which has focus.
//! It never touches Win32; every change returns the `Effects` the desktop
//! layer then applies. That keeps it testable here, and means no state is
//! borrowed while Win32 calls re-enter the message loop.
//!
//! The commands are COSMIC's (cosmic-comp's data/keybindings.ron), minus
//! the ones that need UI this doesn't have yet: stacking, swap mode and
//! focusing groups.
//!
//! Monitors are COSMIC's outputs in its default workspace mode,
//! `WorkspaceMode::OutputBound` (cosmic-comp-config/src/workspace.rs): each
//! monitor has workspaces of its own and switches them on its own. COSMIC's
//! other mode, workspaces spanning every monitor, isn't here. Where COSMIC
//! has a workspace list that grows and shrinks, this has nine fixed slots
//! per monitor (Super+1–9), so the places COSMIC appends a workspace to a
//! list (a monitor unplugged) take the first free slot after the last one in
//! use instead, and a workspace coming home (the monitor plugged back in)
//! goes back to the slot it had there.
//!
//! COSMIC's seat has two outputs that matter, and so does this: the active
//! one, where the pointer is, which Super+1–9, Super+Ctrl+direction, focus
//! moves, the launcher and new windows go by; and the focused one, where the
//! window with keyboard focus is, which moving windows goes by.
//!
//! Minimized windows stay on their workspace, out of the tiling, as in
//! COSMIC (`Workspace::minimized_windows`), so the window switcher can list
//! them and bring them back. COSMIC puts a restored tile back in its old
//! spot in the tree; here it splits the focused window like a new one.

use crate::classify::Fit;
use crate::geometry::{Size, letterboxed};
use crate::motion::{Pace, View};
use crate::settings::{ActivationPolicy, TileBehavior, WorkspaceNumber};
use crate::tree::{Axis, Direction, Gaps, Point, Rect, Tree};

pub use crate::settings::WORKSPACES;

/// A workspace number that exists. Keys 1–9 map to 0–8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceId(usize);

impl WorkspaceId {
    pub fn from_key(n: u32) -> Option<Self> {
        (1..=WORKSPACES as u32).contains(&n).then(|| WorkspaceId(n as usize - 1))
    }
}

/// shell.json's workspaces are checked as it's read.
impl From<WorkspaceNumber> for WorkspaceId {
    fn from(n: WorkspaceNumber) -> Self {
        WorkspaceId(n.index())
    }
}

/// A monitor, by something that stays the same while it's unplugged and
/// plugged back in (COSMIC matches EDID and connector; the desktop layer
/// uses Windows' monitor device ID).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutputId(pub String);

/// The monitors there are, at least one, each once, with the area windows
/// tile in, in screen pixels.
#[derive(Clone, Debug)]
pub struct Screens(Vec<(OutputId, Rect)>);

impl Screens {
    /// None for no monitors at all, which Windows never has while a
    /// session is on screen.
    pub fn new(list: Vec<(OutputId, Rect)>) -> Option<Self> {
        let mut unique: Vec<(OutputId, Rect)> = Vec::new();
        for (id, area) in list {
            if !unique.iter().any(|(x, _)| *x == id) {
                unique.push((id, area));
            }
        }
        (!unique.is_empty()).then_some(Screens(unique))
    }
}

/// One hold of the modifiers: a new one starts whenever a modifier goes
/// down or up (COSMIC's `last_modifier_change` serial).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Focus(Direction),
    Move(Direction),
    Resize { dir: Direction, outwards: bool },
    ToggleOrientation,
    /// Tiling off for the whole workspace: windows stay where they are.
    ToggleTiling,
    ToggleFloating,
    Maximize,
    Close,
    Switch(WorkspaceId),
    /// COSMIC's MoveToWorkspace: the window goes, and you go with it.
    MoveTo(WorkspaceId),
    SwitchStep(Step),
    MoveStep(Step),
    /// Super+Alt+direction: the monitor that way gets the pointer and focus.
    SwitchOutput(Direction),
    /// Super+Shift+Alt+direction: the window goes to the monitor that way.
    MoveToOutput(Direction),
}

/// How a Super+drag handles the window it picked up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    /// Tiled: it gets dropped back into the tiling somewhere.
    Tiled,
    /// Floating, or on a workspace with tiling off: it just moves.
    Free,
}

/// What the desktop layer should do to make the screen match the state.
#[derive(Debug, PartialEq, Eq)]
pub struct Effects<W> {
    pub show: Vec<W>,
    pub hide: Vec<W>,
    pub place: Vec<(W, Rect)>,
    pub focus: Option<W>,
    pub close: Option<W>,
    /// How the layout change gets onto the screen (motion.rs).
    pub pace: Pace,
    /// Workspace switches asked for, by monitor, even to the one already
    /// shown: in COSMIC that cuts a running switch short.
    pub activated: Vec<(OutputId, usize)>,
    /// Minimized windows to bring back, before they're placed.
    pub restore: Vec<W>,
    /// Floating windows going to another monitor: from that area to this
    /// one, keeping their place relative to it.
    pub carry: Vec<(W, Rect, Rect)>,
    /// Where the pointer jumps to, if anywhere.
    pub pointer: Option<Warp<W>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warp<W> {
    /// A point, as COSMIC's output moves move it.
    To(Point),
    /// cursor_follows_focus: the middle of this window, wherever it ends up
    /// (a floating one's place isn't known here).
    Follow(W),
}

impl<W> Default for Effects<W> {
    fn default() -> Self {
        Effects {
            show: Vec::new(),
            hide: Vec::new(),
            place: Vec::new(),
            focus: None,
            close: None,
            pace: Pace::Animated,
            activated: Vec::new(),
            restore: Vec::new(),
            carry: Vec::new(),
            pointer: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Tiled,
    Floating,
}

/// Where a minimized window goes back to.
#[derive(Clone, Copy)]
struct Minimized<W> {
    w: W,
    layer: Layer,
    maximized: bool,
}

/// A slot on a monitor a workspace has been on: COSMIC's `output_stack`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Home {
    output: OutputId,
    slot: usize,
}

struct Workspace<W> {
    tiled: Tree<W>,
    /// Managed (hidden and shown with the workspace) but never placed.
    floating: Vec<W>,
    focus: Option<W>,
    maximized: Option<W>,
    tiling: bool,
    minimized: Vec<Minimized<W>>,
    /// Where it's been since it was last put somewhere on purpose, oldest
    /// first, the last entry being where it is: it goes back to the first
    /// of those to come back.
    homes: Vec<Home>,
}

impl<W: Copy + Eq> Workspace<W> {
    fn new(output: OutputId, slot: usize, tiling: bool) -> Self {
        Workspace {
            tiled: Tree::default(),
            floating: Vec::new(),
            focus: None,
            maximized: None,
            tiling,
            minimized: Vec::new(),
            homes: vec![Home { output, slot }],
        }
    }

    /// Shown with the workspace: tiled or floating, not minimized.
    fn contains(&self, w: W) -> bool {
        self.tiled.contains(w) || self.floating.contains(&w)
    }

    fn holds(&self, w: W) -> bool {
        self.contains(w) || self.minimized.iter().any(|m| m.w == w)
    }

    fn is_empty(&self) -> bool {
        self.tiled.is_empty() && self.floating.is_empty() && self.minimized.is_empty()
    }

    /// COSMIC's `Workspace::set_output` when it's not the user moving it:
    /// a monitor it's been on before cuts the list back to there.
    fn moved(&mut self, output: &OutputId, slot: usize) {
        match self.homes.iter().position(|h| h.output == *output) {
            Some(i) => {
                self.homes.truncate(i + 1);
                self.homes[i].slot = slot;
            }
            None => self.homes.push(Home { output: output.clone(), slot }),
        }
    }

    /// COSMIC's `prefers_output`: the slot it had there.
    fn prefers(&self, output: &OutputId) -> Option<usize> {
        self.homes.iter().find(|h| h.output == *output).map(|h| h.slot)
    }

    fn minimize(&mut self, w: W) {
        if !self.contains(w) {
            return;
        }
        let layer = if self.tiled.contains(w) { Layer::Tiled } else { Layer::Floating };
        let maximized = self.maximized == Some(w);
        if self.remove(w) {
            self.minimized.push(Minimized { w, layer, maximized });
        }
    }

    fn take_minimized(&mut self, w: W) -> Option<Minimized<W>> {
        let i = self.minimized.iter().position(|m| m.w == w)?;
        Some(self.minimized.remove(i))
    }

    /// Back from being minimized, focused, as it was: tiled or floating,
    /// maximized if it was and nothing else is now.
    fn restore(&mut self, m: Minimized<W>, area: Rect) {
        match m.layer {
            Layer::Tiled => self.insert(m.w, area),
            Layer::Floating => {
                self.floating.push(m.w);
                self.focus = Some(m.w);
            }
        }
        if m.maximized && self.maximized.is_none() {
            self.maximized = Some(m.w);
        }
    }

    fn windows(&self) -> Vec<W> {
        let mut all = self.tiled.windows();
        all.extend(&self.floating);
        all
    }

    fn insert(&mut self, w: W, area: Rect) {
        let next_to = self.focus.filter(|&f| self.tiled.contains(f));
        self.tiled.insert(w, next_to, area);
        self.focus = Some(w);
    }

    fn remove(&mut self, w: W) -> bool {
        let heir = if self.tiled.contains(w) {
            self.tiled.remove(w)
        } else if let Some(i) = self.floating.iter().position(|&x| x == w) {
            self.floating.remove(i);
            None
        } else if let Some(i) = self.minimized.iter().position(|m| m.w == w) {
            self.minimized.remove(i);
            return true;
        } else {
            return false;
        };
        if self.maximized == Some(w) {
            self.maximized = None;
        }
        if self.focus == Some(w) {
            self.focus = heir.or_else(|| self.windows().first().copied());
        }
        true
    }

    /// Everything on `other` joins this one: where nine slots run out and
    /// COSMIC would just have made a tenth workspace.
    fn absorb(&mut self, other: Workspace<W>, area: Rect) {
        for w in other.tiled.windows() {
            self.insert(w, area);
        }
        self.floating.extend(other.floating);
        self.minimized.extend(other.minimized);
    }
}

struct Output<W> {
    id: OutputId,
    area: Rect,
    spaces: [Workspace<W>; WORKSPACES],
    active: usize,
}

impl<W: Copy + Eq> Output<W> {
    fn new(id: OutputId, area: Rect, autotile: bool) -> Self {
        Output { spaces: std::array::from_fn(|slot| Workspace::new(id.clone(), slot, autotile)), id, area, active: 0 }
    }

    fn shown(&self) -> &Workspace<W> {
        &self.spaces[self.active]
    }

    fn shown_mut(&mut self) -> &mut Workspace<W> {
        &mut self.spaces[self.active]
    }

    /// Where a workspace from elsewhere goes: the first free slot after the
    /// last one in use, as COSMIC appends it to the list, or failing that
    /// any free one.
    fn free_slot(&self) -> Option<usize> {
        let free = |i: &usize| self.spaces[*i].is_empty() && *i != self.active;
        let after = self.spaces.iter().rposition(|s| !s.is_empty()).map_or(0, |i| i + 1);
        (after..WORKSPACES).find(free).or_else(|| (0..WORKSPACES).find(free))
    }

    fn arrange(&self, gaps: Gaps) -> Vec<(W, Rect)> {
        let s = self.shown();
        let mut place = if s.tiling { s.tiled.tiles(self.area, gaps) } else { Vec::new() };
        if let Some(m) = s.maximized {
            place.retain(|&(w, _)| w != m);
            place.push((m, self.area));
        }
        place
    }

    fn centre(&self) -> Point {
        Point { x: self.area.left + self.area.width() / 2, y: self.area.top + self.area.height() / 2 }
    }
}

/// What the animator needs of one monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen<W> {
    pub id: OutputId,
    pub active: usize,
    pub views: Vec<View<W>>,
}

/// COSMIC's `previous_workspace_idx`: the workspace a monitor showed when
/// the focus or a window first went past the edge of its tiling during
/// this hold of the modifiers. Going on to another monitor in the same
/// hold puts that workspace back, so looking through the workspaces on
/// the way to the next monitor leaves this one as it was.
struct Peek {
    hold: Hold,
    output: OutputId,
    workspace: usize,
}

/// Which windows the desktop layer has shown and hidden, before a change.
struct Before<W> {
    shown: Vec<W>,
    hidden: Vec<W>,
}

pub struct Wm<W> {
    /// Never empty (`Screens` isn't), in the order they came: the first is
    /// where an unplugged monitor's workspaces go, as in COSMIC.
    outputs: Vec<Output<W>>,
    /// COSMIC's active output: where the pointer is.
    active: usize,
    /// What has keyboard focus, managed or not; its monitor is COSMIC's
    /// focused output.
    keyboard: Option<W>,
    /// Every window that had focus or was managed, most recent first:
    /// the window switcher's order, which COSMIC keeps the same way (the
    /// toplevels by when they were last activated).
    recent: Vec<W>,
    peek: Option<Peek>,
    /// Windows that don't take their tile's size (classify::Fit); the rest
    /// fill theirs.
    fits: Vec<(W, Fit)>,
    tiling: Tiling,
}

/// COSMIC's tiling settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiling {
    pub gaps: Gaps,
    /// cosmic-comp's `autotile`: how workspaces start out, and what
    /// ToggleTiling flips everywhere under `TileBehavior::Global`.
    pub autotile: bool,
    pub behavior: TileBehavior,
    pub cursor_follows_focus: bool,
    pub activation_policy: ActivationPolicy,
}

impl Default for Tiling {
    fn default() -> Self {
        Tiling {
            gaps: Gaps::default(),
            autotile: true,
            behavior: TileBehavior::PerWorkspace,
            cursor_follows_focus: false,
            activation_policy: ActivationPolicy::Focus,
        }
    }
}

impl<W: Copy + Eq> Wm<W> {
    #[cfg(test)]
    pub fn new(screens: Screens) -> Self {
        Self::with_tiling(screens, Tiling::default())
    }

    pub fn with_tiling(screens: Screens, tiling: Tiling) -> Self {
        let outputs = screens.0.into_iter().map(|(id, area)| Output::new(id, area, tiling.autotile)).collect();
        Wm { outputs, active: 0, keyboard: None, recent: Vec::new(), peek: None, fits: Vec::new(), tiling }
    }

    fn locate(&self, w: W) -> Option<(usize, usize)> {
        self.outputs.iter().enumerate().find_map(|(o, out)| out.spaces.iter().position(|s| s.holds(w)).map(|i| (o, i)))
    }

    /// The output and workspace `w` is on, if that's being shown.
    fn shown_at(&self, w: W) -> Option<usize> {
        self.outputs.iter().position(|out| out.shown().contains(w))
    }

    fn output_at(&self, p: Point) -> Option<usize> {
        self.outputs.iter().position(|o| o.area.contains(p))
    }

    /// COSMIC's focused output, or the active one if nothing managed has
    /// focus.
    fn focused_output(&self) -> usize {
        self.keyboard.and_then(|w| self.locate(w)).map_or(self.active, |(o, _)| o)
    }

    pub fn manages(&self, w: W) -> bool {
        self.locate(w).is_some()
    }

    /// On a workspace being shown. Managed windows elsewhere are ones this
    /// hid, so their hide events aren't the window going away.
    pub fn is_shown(&self, w: W) -> bool {
        self.shown_at(w).is_some()
    }

    pub fn is_minimized(&self, w: W) -> bool {
        self.outputs.iter().flat_map(|o| &o.spaces).any(|s| s.minimized.iter().any(|m| m.w == w))
    }

    /// Most recently focused first; may include windows that aren't managed.
    pub fn recent(&self) -> &[W] {
        &self.recent
    }

    fn touch(&mut self, w: W) {
        self.recent.retain(|&x| x != w);
        self.recent.insert(0, w);
    }

    /// Every window this has hidden; shown again if the shell exits.
    pub fn hidden(&self) -> Vec<W> {
        self.outputs
            .iter()
            .flat_map(|o| o.spaces.iter().enumerate().filter(|&(i, _)| i != o.active).flat_map(|(_, s)| s.windows()))
            .collect()
    }

    fn shown_windows(&self) -> Vec<W> {
        self.outputs.iter().flat_map(|o| o.shown().windows()).collect()
    }

    fn before(&self) -> Before<W> {
        Before { shown: self.shown_windows(), hidden: self.hidden() }
    }

    /// What to give focus back to, e.g. after the launcher: the window
    /// that has it if that's on screen, else the active monitor's.
    pub fn focused_window(&self) -> Option<W> {
        self.keyboard.filter(|&w| self.is_shown(w)).or(self.outputs[self.active].shown().focus)
    }

    /// The active monitor's area: where the launcher and the switcher open,
    /// as COSMIC puts a layer surface that asks for no output there.
    pub fn active_area(&self) -> Rect {
        self.outputs[self.active].area
    }

    /// The pointer is at `p`: the monitor under it becomes the active one,
    /// as in COSMIC on every pointer motion.
    pub fn pointer(&mut self, p: Point) {
        if let Some(o) = self.output_at(p) {
            self.active = o;
        }
    }

    /// What each monitor shows, for animating: on every workspace, its
    /// tiles even when one of them is maximized over the rest.
    pub fn screens(&self) -> Vec<Screen<W>> {
        self.outputs
            .iter()
            .map(|o| Screen {
                id: o.id.clone(),
                active: o.active,
                views: o
                    .spaces
                    .iter()
                    .map(|s| View {
                        tiles: if s.tiling { self.fitted(s.tiled.tiles(o.area, self.tiling.gaps), o.area) } else { Vec::new() },
                        maximized: s.maximized,
                        area: o.area,
                    })
                    .collect(),
            })
            .collect()
    }

    /// Windows on that monitor's workspace `i` that the shell doesn't place.
    pub fn floating(&self, output: &OutputId, i: usize) -> Vec<W> {
        let Some(s) = self.outputs.iter().find(|o| o.id == *output).map(|o| &o.spaces[i]) else { return Vec::new() };
        let mut free = if s.tiling { s.floating.clone() } else { s.windows() };
        free.retain(|&w| Some(w) != s.maximized);
        free
    }

    /// Every window the tiling places, on every workspace: the ones that
    /// go without a title bar. Minimized tiles keep going without: giving
    /// it back and taking it again raced the restore, which left restored
    /// windows with one.
    pub fn tiled(&self) -> Vec<W> {
        let minimized = |s: &Workspace<W>| s.minimized.iter().filter(|m| m.layer == Layer::Tiled).map(|m| m.w).collect::<Vec<_>>();
        self.outputs.iter().flat_map(|o| &o.spaces).filter(|s| s.tiling).flat_map(|s| [s.tiled.windows(), minimized(s)].concat()).collect()
    }

    /// Where the active hint goes: around the window with keyboard focus
    /// if it's managed and shown and not maximized, as in COSMIC; with its
    /// tile if it has one, None if it floats.
    pub fn hinted(&self) -> Option<(W, Option<Rect>)> {
        let w = self.keyboard?;
        let o = self.shown_at(w)?;
        if self.outputs[o].shown().maximized == Some(w) {
            return None;
        }
        let out = &self.outputs[o];
        Some((w, self.fitted(out.arrange(self.tiling.gaps), out.area).into_iter().find(|&(x, _)| x == w).map(|(_, r)| r)))
    }

    /// Super+drag picked up `w`; it gets focus, and a maximized one goes
    /// back to its tile, as in COSMIC.
    pub fn grab(&mut self, w: W) -> Option<Grab> {
        let o = self.shown_at(w)?;
        self.keyboard = Some(w);
        let s = self.outputs[o].shown_mut();
        s.focus = Some(w);
        if !(s.tiling && s.tiled.contains(w)) {
            return Some(Grab::Free);
        }
        if s.maximized == Some(w) {
            s.maximized = None;
        }
        Some(Grab::Tiled)
    }

    /// Where dragged `w` would tile if dropped at `p`, on whichever monitor
    /// that is.
    pub fn drop_preview(&self, w: W, p: Point, band: i32) -> Option<Rect> {
        let from = self.shown_at(w)?;
        let to = self.output_at(p).unwrap_or(from);
        let out = &self.outputs[to];
        let s = out.shown();
        if to == from {
            s.tiled.drop_preview(w, p, out.area, band, self.tiling.gaps)
        } else if s.tiling {
            s.tiled.drop_in_preview(w, p, out.area, band, self.tiling.gaps)
        } else {
            None
        }
    }

    /// Drops dragged `w` at `p`; laid out again even if it lands nowhere
    /// new, so it snaps back. Let go over another monitor, it joins that
    /// one's tiling, as COSMIC's move grab lands on the output under the
    /// pointer.
    pub fn drop(&mut self, w: W, p: Point, band: i32) -> Effects<W> {
        let Some(from) = self.shown_at(w) else { return Effects::default() };
        let before = self.before();
        let to = self.output_at(p).unwrap_or(from);
        if to == from || !self.outputs[to].shown().tiling {
            let area = self.outputs[from].area;
            self.outputs[from].shown_mut().tiled.drop(w, p, area, band);
        } else {
            self.outputs[from].shown_mut().remove(w);
            let out = &mut self.outputs[to];
            let area = out.area;
            let s = out.shown_mut();
            s.maximized = None;
            s.tiled.drop_in(w, p, area, band);
            s.focus = Some(w);
            self.active = to;
        }
        self.keyboard = Some(w);
        self.effects(before, Effects { focus: Some(w), ..Effects::default() })
    }

    /// The edge of tiled `w` that Super+right-drag grabbed by `corner`.
    pub fn edge(&self, w: W, corner: [Direction; 2]) -> Option<(Axis, i32)> {
        let out = &self.outputs[self.shown_at(w)?];
        out.shown().tiled.edge(w, corner, out.area)
    }

    pub fn move_edge(&mut self, w: W, corner: [Direction; 2], at: i32) -> Effects<W> {
        let Some(o) = self.shown_at(w) else { return Effects::default() };
        let out = &mut self.outputs[o];
        let area = out.area;
        out.shown_mut().tiled.move_edge(w, corner, at, area);
        self.placement()
    }

    fn arrange(&self) -> Vec<(W, Rect)> {
        self.outputs.iter().flat_map(|o| self.fitted(o.arrange(self.tiling.gaps), o.area)).collect()
    }

    fn fit(&self, w: W) -> Fit {
        self.fits.iter().find(|&&(x, _)| x == w).map_or(Fit::Fills, |&(_, f)| f)
    }

    /// Tiles as the windows in them take them: a window that won't take its
    /// tile's size sits centred in it at its own, kept on its monitor's
    /// `area` (geometry::letterboxed).
    fn fitted(&self, tiles: Vec<(W, Rect)>, area: Rect) -> Vec<(W, Rect)> {
        tiles.into_iter().map(|(w, tile)| (w, letterboxed(tile, self.fit(w).size_in(Size::of(tile)), area))).collect()
    }

    fn set_fit(&mut self, w: W, fit: Fit) {
        self.fits.retain(|&(x, _)| x != w);
        if fit != Fit::Fills {
            self.fits.push((w, fit));
        }
    }

    /// `w`, placed by its fit, ended up `took` in size with its own style
    /// `style`: if that makes it fit its tile differently (Fit::after),
    /// it's placed again by its new fit, and the new fit comes back to be
    /// logged. None if it isn't tiled on screen or fits as it did, so
    /// checking a window again never moves it again; one that resized
    /// itself in its tile is centred again at its new size.
    pub fn took(&mut self, w: W, style: u32, took: Size) -> Option<(Fit, Effects<W>)> {
        let (_, tile) = self.outputs.iter().flat_map(|o| o.arrange(self.tiling.gaps)).find(|&(x, _)| x == w)?;
        let fit = Fit::after(Size::of(tile), took, style);
        if fit == self.fit(w) {
            return None;
        }
        self.set_fit(w, fit);
        Some((fit, self.placement()))
    }

    /// `w`'s own style is `style` now: a window letterboxed under another
    /// one is asked for its tile again (Fit::restyled). None if that
    /// changes nothing.
    pub fn restyled(&mut self, w: W, style: u32) -> Option<(Fit, Effects<W>)> {
        let fit = self.fit(w).restyled(style);
        if fit == self.fit(w) {
            return None;
        }
        self.set_fit(w, fit);
        Some((fit, self.placement()))
    }

    /// The windows left at their own size, for desktop.rs to watch their
    /// style.
    pub fn letterboxed(&self) -> Vec<W> {
        self.fits.iter().map(|&(w, _)| w).collect()
    }

    /// Fills in what every change needs: every shown window placed, and
    /// windows shown or hidden as their workspaces came and went.
    fn effects(&mut self, before: Before<W>, fx: Effects<W>) -> Effects<W> {
        let shown = self.shown_windows();
        let hidden = self.hidden();
        let show = shown.iter().copied().filter(|w| before.hidden.contains(w)).collect();
        let hide = hidden.iter().copied().filter(|w| before.shown.contains(w)).collect();
        if let Some(w) = fx.focus {
            self.touch(w);
            self.keyboard = Some(w);
        }
        Effects { show, hide, place: self.arrange(), ..fx }
    }

    /// The focus a change on monitor `o` leaves: its workspace's, if
    /// that's where focus was.
    fn focus_after(&self, o: usize) -> Option<W> {
        (self.focused_output() == o).then(|| self.outputs[o].shown().focus).flatten()
    }

    /// A new window, on the active monitor (COSMIC's `map_window`).
    pub fn add(&mut self, w: W) -> Effects<W> {
        let o = self.active;
        self.add_on(w, o)
    }

    /// A window the shell found already open, on the monitor it's on.
    pub fn add_at(&mut self, w: W, centre: Point) -> Effects<W> {
        let o = self.output_at(centre).unwrap_or(self.active);
        self.add_on(w, o)
    }

    fn add_on(&mut self, w: W, o: usize) -> Effects<W> {
        if self.manages(w) {
            return Effects::default();
        }
        let before = self.before();
        let out = &mut self.outputs[o];
        let area = out.area;
        out.shown_mut().insert(w, area);
        self.effects(before, Effects { focus: Some(w), ..Effects::default() })
    }

    /// A new window: COSMIC maps it with the pointer following, if
    /// cursor_follows_focus is on.
    pub fn add_new(&mut self, w: W) -> Effects<W> {
        let fx = self.add(w);
        let follow = (self.tiling.cursor_follows_focus && fx.focus == Some(w)).then_some(w);
        Effects { pointer: fx.pointer.or(follow.map(Warp::Follow)), ..fx }
    }

    /// Managed `w`, on a workspace that isn't shown, showed itself or took
    /// the foreground: what COSMIC does with an xdg-activation request,
    /// by `activation_policy`. None if it's shown or not managed, where
    /// there's nothing to decide. Windows doesn't say whether a window on
    /// screen raised itself or was clicked, so the policy only governs
    /// windows elsewhere.
    pub fn raised(&mut self, w: W) -> Option<Effects<W>> {
        if !self.manages(w) || self.is_shown(w) || self.is_minimized(w) {
            return None;
        }
        Some(match self.tiling.activation_policy {
            ActivationPolicy::Focus => self.activate(w),
            // COSMIC marks it urgent too; with no workspace indicator,
            // nothing here would show that.
            ActivationPolicy::FocusIfActiveWorkspace | ActivationPolicy::Urgent => {
                Effects { hide: vec![w], focus: self.focused_window(), pace: Pace::Instant, ..Effects::default() }
            }
        })
    }

    /// A window that was already minimized when the shell started.
    pub fn add_minimized(&mut self, w: W) {
        if self.manages(w) {
            return;
        }
        let o = self.active;
        self.outputs[o].shown_mut().minimized.push(Minimized { w, layer: Layer::Tiled, maximized: false });
        self.recent.push(w);
    }

    pub fn remove(&mut self, w: W) -> Effects<W> {
        self.recent.retain(|&x| x != w);
        // Handles get reused.
        self.fits.retain(|&(x, _)| x != w);
        let Some((o, i)) = self.locate(w) else { return Effects::default() };
        let before = self.before();
        let had_focus = self.keyboard == Some(w);
        self.outputs[o].spaces[i].remove(w);
        if !had_focus {
            return self.effects(before, Effects::default());
        }
        // Focus goes to the heir, on the monitor the window was on.
        self.keyboard = None;
        let focus = (self.outputs[o].active == i).then(|| self.outputs[o].shown().focus).flatten();
        self.effects(before, Effects { focus, ..Effects::default() })
    }

    /// `w` got focus, from the shell or anything else (a click, a window
    /// raising itself). It needn't be managed: the switcher lists dialogs
    /// and full-screen games too, as COSMIC lists every toplevel.
    pub fn focused(&mut self, w: W) {
        self.touch(w);
        self.keyboard = Some(w);
        if let Some(o) = self.shown_at(w) {
            self.outputs[o].shown_mut().focus = Some(w);
        }
    }

    /// Shown `w` was minimized: it leaves the tiling but not its workspace.
    pub fn minimize(&mut self, w: W) -> Effects<W> {
        let Some(o) = self.shown_at(w) else { return Effects::default() };
        let before = self.before();
        self.outputs[o].shown_mut().minimize(w);
        let focus = self.focus_after(o);
        if self.keyboard == Some(w) {
            self.keyboard = focus;
        }
        self.effects(before, Effects { focus, ..Effects::default() })
    }

    /// Minimized `w` came back by itself (the app restored it). It lands
    /// on the active monitor's workspace, since that's where you are.
    pub fn restored(&mut self, w: W) -> Effects<W> {
        let before = self.before();
        let Some(m) = self.outputs.iter_mut().flat_map(|o| &mut o.spaces).find_map(|s| s.take_minimized(w)) else {
            return Effects::default();
        };
        let out = &mut self.outputs[self.active];
        let area = out.area;
        out.shown_mut().restore(m, area);
        self.effects(before, Effects { focus: Some(w), ..Effects::default() })
    }

    /// The switcher picked `w`: COSMIC's activate goes to its workspace on
    /// its monitor, unminimizes it and focuses it. The pointer stays put
    /// (`cursor_follows_focus` is off by default).
    pub fn activate(&mut self, w: W) -> Effects<W> {
        let Some((o, i)) = self.locate(w) else { return Effects::default() };
        let before = self.before();
        let activated = if i == self.outputs[o].active { Vec::new() } else { vec![self.switch(o, i)] };
        let out = &mut self.outputs[o];
        let area = out.area;
        let s = out.shown_mut();
        let restore = match s.take_minimized(w) {
            Some(m) => {
                s.restore(m, area);
                vec![w]
            }
            None => {
                s.focus = Some(w);
                Vec::new()
            }
        };
        self.effects(before, Effects { focus: Some(w), activated, restore, ..Effects::default() })
    }

    /// The monitors changed (WM_DISPLAYCHANGE). New ones get their own
    /// workspaces, and take back the ones that were theirs (COSMIC's
    /// `add_output`); a monitor that's gone hands every workspace with
    /// anything on it to the first monitor (`remove_output`), keeping it on
    /// screen there if it was and that one showed nothing.
    pub fn set_screens(&mut self, screens: Screens) -> Effects<W> {
        let before = self.before();
        let active = self.outputs[self.active].id.clone();
        for (id, area) in &screens.0 {
            match self.outputs.iter_mut().find(|o| o.id == *id) {
                Some(o) => o.area = *area,
                None => self.add_output(id.clone(), *area),
            }
        }
        while let Some(gone) = self.outputs.iter().position(|o| !screens.0.iter().any(|(id, _)| *id == o.id)) {
            self.remove_output(gone);
        }
        self.active = self.outputs.iter().position(|o| o.id == active).unwrap_or(0);
        if self.keyboard.is_some_and(|w| self.manages(w) && !self.is_shown(w)) {
            self.keyboard = None;
        }
        self.effects(before, Effects { pace: Pace::Instant, ..Effects::default() })
    }

    fn add_output(&mut self, id: OutputId, area: Rect) {
        let autotile = self.tiling.autotile;
        let mut new = Output::new(id.clone(), area, autotile);
        let mut first: Option<usize> = None;
        for other in &mut self.outputs {
            for i in 0..WORKSPACES {
                let Some(slot) = other.spaces[i].prefers(&id) else { continue };
                let mut ws = std::mem::replace(&mut other.spaces[i], Workspace::new(other.id.clone(), i, autotile));
                let slot = if new.spaces[slot].is_empty() { Some(slot) } else { new.free_slot() };
                match slot {
                    Some(slot) => {
                        ws.moved(&id, slot);
                        new.spaces[slot] = ws;
                        first = Some(first.map_or(slot, |f| f.min(slot)));
                    }
                    None => new.spaces[WORKSPACES - 1].absorb(ws, area),
                }
            }
        }
        // COSMIC's new set shows the first workspace it got back.
        new.active = first.unwrap_or(0);
        self.outputs.push(new);
    }

    fn remove_output(&mut self, gone: usize) {
        let out = self.outputs.remove(gone);
        if self.outputs.is_empty() {
            // Can't happen with `Screens`: the new ones were added first.
            self.outputs.push(out);
            return;
        }
        let target = &mut self.outputs[0];
        let area = target.area;
        for (i, ws) in out.spaces.into_iter().enumerate() {
            if ws.is_empty() {
                continue;
            }
            match target.free_slot() {
                Some(slot) => {
                    let mut ws = ws;
                    ws.moved(&target.id, slot);
                    target.spaces[slot] = ws;
                    if i == out.active && target.shown().is_empty() {
                        target.active = slot;
                    }
                }
                None => target.spaces[WORKSPACES - 1].absorb(ws, area),
            }
        }
    }

    /// Every window where it belongs, touching nothing else.
    pub fn placement(&self) -> Effects<W> {
        Effects { place: self.arrange(), pace: Pace::Instant, ..Effects::default() }
    }

    /// Monitor `o` shows workspace `to`; the activation to animate.
    fn switch(&mut self, o: usize, to: usize) -> (OutputId, usize) {
        self.outputs[o].active = to;
        (self.outputs[o].id.clone(), to)
    }

    /// COSMIC's Workspace(n) and Next/PreviousWorkspace, on the active
    /// monitor; focus goes to what that workspace had focused.
    fn switch_active(&mut self, before: Before<W>, to: usize) -> Effects<W> {
        let o = self.active;
        let activated = vec![self.switch(o, to)];
        let focus = self.outputs[o].shown().focus;
        if focus.is_none() {
            self.keyboard = None;
        }
        self.effects(before, Effects { focus, activated, ..Effects::default() })
    }

    /// Workspace switching wraps around, COSMIC's `workspace_wraparound`
    /// default.
    fn stepped(from: usize, step: Step) -> usize {
        match step {
            Step::Next => (from + 1) % WORKSPACES,
            Step::Previous => (from + WORKSPACES - 1) % WORKSPACES,
        }
    }

    /// COSMIC's `Shell::next_output`: among the monitors overlapping this
    /// one across `dir`, the one whose origin is nearest that way.
    fn next_output(&self, from: usize, dir: Direction) -> Option<usize> {
        let cur = self.outputs[from].area;
        self.outputs
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != from)
            .filter(|(_, o)| match dir {
                Direction::Left | Direction::Right => o.area.top < cur.bottom && o.area.bottom > cur.top,
                Direction::Up | Direction::Down => o.area.left < cur.right && o.area.right > cur.left,
            })
            .filter_map(|(i, o)| {
                let by = match dir {
                    Direction::Up => cur.top - o.area.top,
                    Direction::Down => o.area.top - cur.top,
                    Direction::Left => cur.left - o.area.left,
                    Direction::Right => o.area.left - cur.left,
                };
                (by > 0).then_some((i, by))
            })
            .min_by_key(|&(_, by)| by)
            .map(|(i, _)| i)
    }

    /// Focus or a window went past the edge of the tiling: remember what
    /// the active monitor showed, once per hold.
    fn remember(&mut self, hold: Hold) {
        if self.peek.as_ref().is_some_and(|p| p.hold == hold) {
            return;
        }
        let out = &self.outputs[self.active];
        self.peek = Some(Peek { hold, output: out.id.clone(), workspace: out.active });
    }

    /// COSMIC's SwitchOutput: the monitor that way becomes the active one,
    /// the pointer jumps to its middle and focus goes to what its workspace
    /// had focused. `further`: it came from focus going past the edge.
    fn switch_output(&mut self, before: Before<W>, dir: Direction, further: Option<Hold>) -> Effects<W> {
        let from = self.active;
        let Some(to) = self.next_output(from, dir) else { return Effects::default() };
        let mut activated = Vec::new();
        if let Some(hold) = further
            && let Some(peek) = self.peek.take()
            && peek.hold == hold
            && peek.output == self.outputs[from].id
        {
            activated.push(self.switch(from, peek.workspace));
        }
        let shown = self.outputs[to].active;
        activated.push(self.switch(to, shown));
        self.active = to;
        let focus = self.outputs[to].shown().focus;
        if focus.is_none() {
            self.keyboard = None;
        }
        let pointer = Some(Warp::To(self.outputs[to].centre()));
        self.effects(before, Effects { focus, activated, pointer, ..Effects::default() })
    }

    /// COSMIC's `move_element` with `follow`: the focused window leaves the
    /// focused monitor's workspace for workspace `slot` on monitor `to`,
    /// which is shown, and keeps focus. Arriving `dir`-wards it takes that
    /// side of the whole layout; otherwise it splits the focused window. A
    /// workspace with tiling on tiles it, even if it floated, and unmaximizes
    /// what's there; one with tiling off has it float.
    fn move_window(&mut self, before: Before<W>, to: usize, slot: usize, dir: Option<Direction>) -> Effects<W> {
        let from = self.focused_output();
        let Some(w) = self.outputs[from].shown().focus else { return Effects::default() };
        let from_area = self.outputs[from].area;
        self.outputs[from].shown_mut().remove(w);
        let mut activated = Vec::new();
        if self.outputs[to].active != slot || to != from {
            activated.push(self.switch(to, slot));
        }
        let out = &mut self.outputs[to];
        let area = out.area;
        let s = out.shown_mut();
        let mut carry = Vec::new();
        if s.tiling {
            s.maximized = None;
            match dir {
                Some(dir) => s.tiled.insert_from(w, dir),
                None => s.tiled.insert(w, s.focus.filter(|&f| s.tiled.contains(f)), area),
            }
        } else {
            s.floating.push(w);
            if to != from {
                carry.push((w, from_area, area));
            }
        }
        s.focus = Some(w);
        self.keyboard = Some(w);
        self.effects(before, Effects { focus: Some(w), activated, carry, ..Effects::default() })
    }

    /// COSMIC's MoveToOutput: the window goes to the monitor that way, onto
    /// the workspace it shows, and the pointer goes with it. `further`: it
    /// came from the window going past the edge of the tiling.
    fn move_to_output(&mut self, before: Before<W>, dir: Direction, further: Option<Hold>) -> Effects<W> {
        let from = self.focused_output();
        let Some(to) = self.next_output(from, dir) else { return Effects::default() };
        if self.outputs[from].shown().focus.is_none() {
            return Effects::default();
        }
        let slot = self.outputs[to].active;
        let mut fx = self.move_window(before, to, slot, Some(dir));
        self.active = to;
        // Unlike SwitchOutput, any hold will do.
        if further.is_some()
            && let Some(peek) = self.peek.take()
            && peek.output == self.outputs[from].id
        {
            let before = self.before();
            fx.activated.push(self.switch(from, peek.workspace));
            let again = self.effects(before, Effects::default());
            fx.show.extend(again.show);
            fx.hide.extend(again.hide);
            fx.place = again.place;
        }
        fx.pointer = Some(Warp::To(self.outputs[to].centre()));
        fx
    }

    /// COSMIC's MoveToNext/PreviousWorkspace on the focused monitor. Past
    /// the last or first slot it wraps when asked for directly, and goes to
    /// the monitor that way when the window went past the tiling's edge.
    fn move_step(&mut self, before: Before<W>, step: Step, further: Option<(Direction, Hold)>) -> Effects<W> {
        let o = self.focused_output();
        let at = self.outputs[o].active;
        let next = match step {
            Step::Next => (at + 1 < WORKSPACES).then_some(at + 1),
            Step::Previous => at.checked_sub(1),
        };
        match (next, further) {
            (Some(slot), _) => self.move_window(before, o, slot, further.map(|(dir, _)| dir)),
            (None, Some((dir, hold))) => self.move_to_output(before, dir, Some(hold)),
            (None, None) => self.move_window(before, o, Self::stepped(at, step), None),
        }
    }

    /// Runs `c`, and with cursor_follows_focus on, has the pointer follow
    /// where COSMIC's would (`set_focus` with `update_cursor`): to a window
    /// the focus moved to within the tiling, and to a window moved to
    /// another workspace or monitor. Not on a workspace switch, a move within
    /// the tiling, or where the command moves the pointer itself.
    pub fn command(&mut self, c: Command, hold: Hold) -> Effects<W> {
        let focused = self.keyboard;
        let was_at = focused.and_then(|w| self.locate(w));
        let fx = self.run(c, hold);
        if !self.tiling.cursor_follows_focus || fx.pointer.is_some() {
            return fx;
        }
        let now = self.keyboard;
        let follow = match c {
            Command::Focus(_) => now.filter(|_| now != focused && fx.activated.is_empty()),
            Command::Move(_) => now.filter(|&w| now == focused && self.locate(w) != was_at),
            Command::MoveTo(_) | Command::MoveStep(_) | Command::MoveToOutput(_) => now.filter(|_| now == focused),
            _ => None,
        };
        Effects { pointer: fx.pointer.or(follow.map(Warp::Follow)), ..fx }
    }

    fn run(&mut self, c: Command, hold: Hold) -> Effects<W> {
        let before = self.before();
        match c {
            Command::Focus(dir) => {
                // COSMIC's next_focus looks on the active monitor.
                let o = self.active;
                let s = self.outputs[o].shown();
                let area = self.outputs[o].area;
                if !s.tiling {
                    return Effects::default();
                }
                let target = match s.focus {
                    Some(f) if s.tiled.contains(f) => s.tiled.focus(f, dir, area),
                    // A floating window has focus: start from the first tile.
                    Some(_) => s.tiled.windows().first().copied(),
                    None => None,
                };
                if let Some(target) = target {
                    self.outputs[o].shown_mut().focus = Some(target);
                    self.touch(target);
                    self.keyboard = Some(target);
                    return Effects { focus: Some(target), ..Effects::default() };
                }
                // Nothing further that way: vertically, the next or previous
                // workspace (COSMIC's default vertical layout); sideways, the
                // next monitor.
                self.remember(hold);
                match dir {
                    Direction::Up => self.switch_active(before, Self::stepped(self.outputs[o].active, Step::Previous)),
                    Direction::Down => self.switch_active(before, Self::stepped(self.outputs[o].active, Step::Next)),
                    Direction::Left | Direction::Right => self.switch_output(before, dir, Some(hold)),
                }
            }
            Command::Move(dir) => {
                let o = self.focused_output();
                let s = self.outputs[o].shown_mut();
                let Some(f) = s.focus else { return Effects::default() };
                if !(s.tiling && s.tiled.contains(f)) {
                    return Effects::default();
                }
                if s.maximized == Some(f) {
                    s.maximized = None;
                }
                if s.tiled.r#move(f, dir) {
                    return self.effects(before, Effects { focus: Some(f), ..Effects::default() });
                }
                self.remember(hold);
                match dir {
                    Direction::Up => self.move_step(before, Step::Previous, Some((dir, hold))),
                    Direction::Down => self.move_step(before, Step::Next, Some((dir, hold))),
                    Direction::Left | Direction::Right => self.move_to_output(before, dir, Some(hold)),
                }
            }
            Command::Resize { dir, outwards } => {
                let o = self.focused_output();
                let s = self.outputs[o].shown_mut();
                let Some(f) = s.focus else { return Effects::default() };
                s.tiled.resize(f, dir, outwards);
                // COSMIC's keyboard resize isn't animated either.
                self.placement()
            }
            Command::ToggleOrientation => {
                let o = self.focused_output();
                let s = self.outputs[o].shown_mut();
                let Some(f) = s.focus else { return Effects::default() };
                s.tiled.toggle_orientation(f);
                self.effects(before, Effects::default())
            }
            // As cosmic-comp's ToggleTiling: under Global it flips `autotile`
            // and brings every workspace in line with it.
            Command::ToggleTiling => {
                match self.tiling.behavior {
                    TileBehavior::PerWorkspace => {
                        let s = self.outputs[self.active].shown_mut();
                        s.tiling = !s.tiling;
                    }
                    TileBehavior::Global => {
                        self.tiling.autotile = !self.tiling.autotile;
                        let on = self.tiling.autotile;
                        self.outputs.iter_mut().flat_map(|o| &mut o.spaces).for_each(|s| s.tiling = on);
                    }
                }
                self.effects(before, Effects::default())
            }
            Command::ToggleFloating => {
                let o = self.focused_output();
                let area = self.outputs[o].area;
                let s = self.outputs[o].shown_mut();
                let Some(f) = s.focus else { return Effects::default() };
                if s.tiled.contains(f) {
                    s.tiled.remove(f);
                    s.floating.push(f);
                } else {
                    s.floating.retain(|&w| w != f);
                    s.tiled.insert(f, None, area);
                }
                self.effects(before, Effects::default())
            }
            Command::Maximize => {
                let o = self.focused_output();
                let s = self.outputs[o].shown_mut();
                s.maximized = if s.maximized == s.focus { None } else { s.focus };
                self.effects(before, Effects::default())
            }
            // Removal happens when the window actually goes away; it may
            // ask "save changes?" first.
            Command::Close => Effects { close: self.outputs[self.focused_output()].shown().focus, ..Effects::default() },
            Command::Switch(WorkspaceId(to)) => self.switch_active(before, to),
            Command::SwitchStep(step) => self.switch_active(before, Self::stepped(self.outputs[self.active].active, step)),
            Command::MoveTo(WorkspaceId(to)) => {
                let o = self.focused_output();
                if to == self.outputs[o].active {
                    return Effects::default();
                }
                self.move_window(before, o, to, None)
            }
            Command::MoveStep(step) => self.move_step(before, step, None),
            Command::SwitchOutput(dir) => self.switch_output(before, dir, None),
            Command::MoveToOutput(dir) => self.move_to_output(before, dir, None),
        }
    }
}

/// The tiles drawn over a focused window the shell doesn't place (a
/// dialog, an installer): above it in the z-order (`z`, top first) and
/// overlapping it, top first. COSMIC keeps floating windows over the tiles;
/// Windows stacks by activation, and on saturn (2026-11-19) a focused
/// installer stayed under the tile next to it.
pub fn tiles_over<W: Copy + PartialEq>(z: &[W], w: W, at: Rect, tiles: &[(W, Rect)]) -> Vec<W> {
    let Some(i) = z.iter().position(|&x| x == w) else { return Vec::new() };
    z[..i].iter().copied().filter(|&x| tiles.iter().any(|&(t, r)| t == x && r.overlaps(at))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_focused_dialog_under_a_tile_is_found() {
        let r = |left, right| Rect { left, top: 0, right, bottom: 100 };
        let tiles = [(1, r(0, 50)), (2, r(50, 100))];
        // z top first: tile 1, the dialog (9), tile 2. The dialog spans
        // both tiles; only the one above it is drawn over it.
        assert_eq!(tiles_over(&[1, 9, 2], 9, r(40, 60), &tiles), [1]);
        assert_eq!(tiles_over(&[9, 1, 2], 9, r(40, 60), &tiles), Vec::<i32>::new(), "on top already");
        assert_eq!(tiles_over(&[2, 9, 1], 9, r(10, 40), &tiles), Vec::<i32>::new(), "tile 2 is above but elsewhere");
        assert_eq!(tiles_over(&[1, 2], 9, r(10, 40), &tiles), Vec::<i32>::new(), "gone from the z-order");
    }

    const AREA: Rect = Rect { left: 0, top: 0, right: 1600, bottom: 1000 };
    /// A second monitor to the right of the first, a different size.
    const RIGHT_AREA: Rect = Rect { left: 1600, top: 0, right: 2880, bottom: 1024 };
    const H: Hold = Hold(1);

    fn id(name: &str) -> OutputId {
        OutputId(name.into())
    }

    fn one() -> Screens {
        Screens::new(vec![(id("A"), AREA)]).unwrap()
    }

    fn two() -> Screens {
        Screens::new(vec![(id("A"), AREA), (id("B"), RIGHT_AREA)]).unwrap()
    }

    fn wm_with(n: u32) -> Wm<u32> {
        let mut wm = Wm::new(one());
        for w in 1..=n {
            wm.add(w);
        }
        wm
    }

    /// 1 and 2 on A (2 focused), 3 on B, pointer on A.
    fn dual() -> Wm<u32> {
        let mut wm = Wm::new(two());
        wm.add(1);
        wm.add(2);
        wm.pointer(Point { x: 2000, y: 500 });
        wm.add(3);
        wm.pointer(Point { x: 10, y: 10 });
        wm.focused(2);
        wm
    }

    fn ws(n: u32) -> WorkspaceId {
        WorkspaceId::from_key(n).unwrap()
    }

    fn rect(fx: &Effects<u32>, w: u32) -> Option<Rect> {
        fx.place.iter().find(|&&(x, _)| x == w).map(|&(_, r)| r)
    }

    fn cmd(wm: &mut Wm<u32>, c: Command) -> Effects<u32> {
        wm.command(c, H)
    }

    fn left_half(a: Rect) -> Rect {
        Rect { right: a.left + a.width() / 2, ..a }
    }

    fn right_half(a: Rect) -> Rect {
        Rect { left: a.left + a.width() / 2, ..a }
    }

    #[test]
    fn workspace_keys() {
        assert_eq!(WorkspaceId::from_key(0), None);
        assert_eq!(WorkspaceId::from_key(1), Some(WorkspaceId(0)));
        assert_eq!(WorkspaceId::from_key(9), Some(WorkspaceId(8)));
        assert_eq!(WorkspaceId::from_key(10), None);
    }

    #[test]
    fn screens_are_never_empty_and_never_repeat() {
        assert!(Screens::new(Vec::new()).is_none());
        let s = Screens::new(vec![(id("A"), AREA), (id("A"), RIGHT_AREA)]).unwrap();
        assert_eq!(s.0, [(id("A"), AREA)]);
    }

    #[test]
    fn new_windows_take_focus_and_split_it() {
        let mut wm = wm_with(1);
        let fx = wm.add(2);
        assert_eq!(fx.focus, Some(2));
        assert_eq!(rect(&fx, 1), Some(left_half(AREA)));
        assert_eq!(wm.add(2), Effects::default(), "adding twice is a no-op");
    }

    #[test]
    fn directional_focus() {
        // 1 | (2 / 3), focus on 3.
        let mut wm = wm_with(3);
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Up)).focus, Some(2));
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)).focus, Some(1));
        assert_eq!(wm.focused_window(), Some(1));
    }

    #[test]
    fn focus_past_the_left_or_right_edge_with_one_monitor_does_nothing() {
        let mut wm = wm_with(2);
        cmd(&mut wm, Command::Focus(Direction::Left));
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)), Effects::default());
        assert_eq!(wm.focused_window(), Some(1));
    }

    #[test]
    fn focus_past_the_top_or_bottom_goes_to_the_previous_or_next_workspace() {
        let mut wm = wm_with(2);
        let fx = cmd(&mut wm, Command::Focus(Direction::Down));
        assert_eq!(fx.activated, [(id("A"), 1)]);
        assert_eq!(fx.hide, [1, 2]);
        let fx = cmd(&mut wm, Command::Focus(Direction::Up));
        assert_eq!(fx.activated, [(id("A"), 0)], "an empty workspace has nothing to focus, so up goes on up");
        assert_eq!(fx.focus, Some(2), "and focus comes back where it was");
        let fx = cmd(&mut wm, Command::Focus(Direction::Up));
        assert_eq!(fx.activated, [(id("A"), 8)], "wraps around");
    }

    #[test]
    fn closing_the_focused_window_focuses_its_sibling() {
        let mut wm = wm_with(3);
        assert_eq!(wm.remove(3).focus, Some(2));
        assert_eq!(wm.remove(2).focus, Some(1));
        assert_eq!(wm.remove(1).focus, None);
    }

    #[test]
    fn maximize_toggles_full_screen_for_the_focused_window() {
        let mut wm = wm_with(2);
        let fx = cmd(&mut wm, Command::Maximize);
        assert_eq!(rect(&fx, 2), Some(AREA));
        let fx = cmd(&mut wm, Command::Maximize);
        assert_eq!(rect(&fx, 2), Some(right_half(AREA)));
    }

    #[test]
    fn floating_leaves_the_tiles_and_comes_back() {
        let mut wm = wm_with(2);
        let fx = cmd(&mut wm, Command::ToggleFloating);
        assert_eq!(rect(&fx, 2), None);
        assert_eq!(rect(&fx, 1), Some(AREA));
        assert!(wm.manages(2) && wm.is_shown(2));
        let fx = cmd(&mut wm, Command::ToggleFloating);
        assert!(rect(&fx, 2).is_some());
    }

    #[test]
    fn tiling_off_places_nothing() {
        let mut wm = wm_with(2);
        assert!(cmd(&mut wm, Command::ToggleTiling).place.is_empty());
        assert!(wm.add(3).place.is_empty());
        assert_eq!(cmd(&mut wm, Command::ToggleTiling).place.len(), 3);
    }

    #[test]
    fn switching_hides_and_shows_and_remembers() {
        let mut wm = wm_with(2);
        let fx = cmd(&mut wm, Command::Switch(ws(2)));
        assert_eq!(fx.activated, [(id("A"), 1)]);
        assert_eq!(fx.hide, [1, 2]);
        assert!(fx.show.is_empty());
        assert_eq!(fx.focus, None);
        assert!(wm.manages(1) && !wm.is_shown(1));
        assert_eq!(wm.hidden(), [1, 2]);
        wm.add(3);
        let fx = cmd(&mut wm, Command::SwitchStep(Step::Previous));
        assert_eq!((fx.hide, fx.show, fx.focus), (vec![3], vec![1, 2], Some(2)));
        let fx = cmd(&mut wm, Command::Switch(ws(1)));
        assert_eq!(fx.activated, [(id("A"), 0)], "the one shown counts as a switch");
        assert!(fx.show.is_empty() && fx.hide.is_empty());
    }

    #[test]
    fn workspace_steps_wrap_around() {
        let mut wm = wm_with(1);
        assert_eq!(cmd(&mut wm, Command::SwitchStep(Step::Previous)).activated, [(id("A"), 8)]);
        assert_eq!(cmd(&mut wm, Command::SwitchStep(Step::Next)).activated, [(id("A"), 0)]);
    }

    #[test]
    fn moving_to_a_workspace_takes_you_along() {
        let mut wm = wm_with(2);
        let fx = cmd(&mut wm, Command::MoveTo(ws(3)));
        assert_eq!(fx.activated, [(id("A"), 2)]);
        assert_eq!(fx.hide, [1]);
        assert_eq!(fx.focus, Some(2));
        assert_eq!(rect(&fx, 2), Some(AREA));
        assert!(wm.manages(1) && !wm.is_shown(1));
        assert_eq!(cmd(&mut wm, Command::MoveTo(ws(3))), Effects::default(), "already there");
        // A window on another workspace closing touches nothing on screen.
        assert!(wm.remove(1).place.iter().all(|&(w, _)| w == 2));
        assert!(!wm.manages(1));
    }

    #[test]
    fn moving_a_floating_window_to_a_tiling_workspace_tiles_it() {
        let mut wm = wm_with(2);
        cmd(&mut wm, Command::ToggleFloating);
        cmd(&mut wm, Command::Switch(ws(2)));
        wm.add(5);
        cmd(&mut wm, Command::Switch(ws(1)));
        cmd(&mut wm, Command::MoveTo(ws(2)));
        assert_eq!(wm.tiled(), [1, 5, 2], "COSMIC's move_element maps into the tiling");
    }

    #[test]
    fn move_steps_wrap_when_asked_directly() {
        let mut wm = wm_with(1);
        let fx = cmd(&mut wm, Command::MoveStep(Step::Previous));
        assert_eq!((fx.activated, fx.focus), (vec![(id("A"), 8)], Some(1)));
        let fx = cmd(&mut wm, Command::MoveStep(Step::Next));
        assert_eq!(fx.activated, [(id("A"), 0)]);
    }

    #[test]
    fn moving_past_the_top_or_bottom_carries_the_window_to_the_next_workspace() {
        let mut wm = wm_with(1);
        wm.command(Command::Switch(ws(2)), H);
        wm.add(7);
        wm.command(Command::Switch(ws(1)), H);
        // A lone window has nowhere to go within the tiling.
        let fx = cmd(&mut wm, Command::Move(Direction::Down));
        assert_eq!(fx.activated, [(id("A"), 1)]);
        assert_eq!(fx.focus, Some(1));
        assert_eq!(rect(&fx, 1), Some(Rect { bottom: 500, ..AREA }), "comes in from above: the top half");
        assert_eq!(rect(&fx, 7), Some(Rect { top: 500, ..AREA }));
    }

    #[test]
    fn moving_up_past_the_first_workspace_with_one_monitor_does_nothing() {
        let mut wm = wm_with(1);
        assert_eq!(cmd(&mut wm, Command::Move(Direction::Up)), Effects::default());
        assert!(wm.is_shown(1));
    }

    #[test]
    fn moving_a_maximized_tile_unmaximizes_it_first() {
        let mut wm = wm_with(2);
        cmd(&mut wm, Command::Maximize);
        let fx = cmd(&mut wm, Command::Move(Direction::Left));
        assert_eq!(rect(&fx, 2), Some(left_half(AREA)));
    }

    #[test]
    fn only_tiled_windows_on_tiling_workspaces_lose_their_title_bars() {
        let mut wm = wm_with(3);
        cmd(&mut wm, Command::ToggleFloating);
        assert_eq!(wm.tiled(), [1, 2]);
        cmd(&mut wm, Command::Switch(ws(2)));
        wm.add(4);
        cmd(&mut wm, Command::ToggleTiling);
        assert_eq!(wm.tiled(), [1, 2]);
    }

    #[test]
    fn dragging_a_tile_drops_it_into_the_tiling() {
        let mut wm = wm_with(3);
        cmd(&mut wm, Command::Maximize);
        assert_eq!(wm.grab(1), Some(Grab::Tiled));
        assert_eq!(wm.focused_window(), Some(1));
        assert_eq!(wm.grab(3), Some(Grab::Tiled));
        let to_left_edge = Point { x: 0, y: 500 };
        let third = Rect { left: 0, top: 0, right: 533, bottom: 1000 };
        assert_eq!(wm.drop_preview(3, to_left_edge, 32), Some(third));
        let fx = wm.drop(3, to_left_edge, 32);
        assert_eq!(rect(&fx, 3), Some(third), "no longer maximized");
        assert_eq!(rect(&fx, 2), Some(Rect { left: 1067, top: 0, right: 1600, bottom: 1000 }));
        cmd(&mut wm, Command::ToggleFloating);
        assert_eq!(wm.grab(3), Some(Grab::Free));
        assert_eq!(wm.grab(9), None);
    }

    #[test]
    fn dragging_an_edge_resizes_both_sides() {
        let mut wm = wm_with(2);
        let corner = [Direction::Left, Direction::Down];
        assert_eq!(wm.edge(2, corner), Some((Axis::Horizontal, 800)));
        let fx = wm.move_edge(2, corner, 1000);
        assert_eq!(rect(&fx, 1), Some(Rect { left: 0, top: 0, right: 1000, bottom: 1000 }));
    }

    #[test]
    fn screens_keep_a_maximized_windows_tile() {
        let mut wm = wm_with(2);
        cmd(&mut wm, Command::Maximize);
        let v = &wm.screens()[0].views[0];
        assert_eq!(v.maximized, Some(2));
        assert_eq!(v.tiles.len(), 2);
        assert_eq!(rect(&wm.placement(), 2), Some(AREA), "while the real window fills the screen");
        cmd(&mut wm, Command::ToggleTiling);
        assert!(wm.screens()[0].views[0].tiles.is_empty());
        assert_eq!(wm.floating(&id("A"), 0), [1], "all of them float, but the maximized one is drawn as such");
    }

    #[test]
    fn resizes_are_instant_and_the_rest_animate() {
        let mut wm = wm_with(2);
        assert_eq!(wm.add(3).pace, Pace::Animated);
        assert_eq!(cmd(&mut wm, Command::Resize { dir: Direction::Left, outwards: true }).pace, Pace::Instant);
        assert_eq!(wm.move_edge(1, [Direction::Right, Direction::Down], 900).pace, Pace::Instant);
        assert_eq!(cmd(&mut wm, Command::Maximize).pace, Pace::Animated);
    }

    #[test]
    fn minimized_windows_leave_the_tiling_but_not_their_workspace() {
        let mut wm = wm_with(2);
        let fx = wm.minimize(2);
        assert_eq!(fx.place, [(1, AREA)]);
        assert_eq!(fx.focus, Some(1));
        assert!(wm.manages(2) && wm.is_minimized(2) && !wm.is_shown(2));
        assert_eq!(wm.tiled(), [1, 2], "keeps going without a title bar while minimized");
        assert!(wm.hidden().is_empty(), "minimized isn't hidden: switching workspaces leaves it be");
        assert_eq!(wm.minimize(2), Effects::default(), "only a shown window minimizes");
        wm.remove(2);
        assert!(!wm.manages(2) && !wm.is_minimized(2));
    }

    #[test]
    fn activating_a_minimized_window_goes_to_its_workspace_and_restores_it() {
        let mut wm = wm_with(2);
        wm.minimize(2);
        cmd(&mut wm, Command::Switch(ws(3)));
        wm.add(3);
        let fx = wm.activate(2);
        assert_eq!(fx.activated, [(id("A"), 0)]);
        assert_eq!((&fx.hide, &fx.show), (&vec![3], &vec![1]));
        assert_eq!(fx.restore, [2]);
        assert_eq!(fx.focus, Some(2));
        assert_eq!(rect(&fx, 2), Some(right_half(AREA)));
        assert!(wm.is_shown(2) && !wm.is_minimized(2));
        assert_eq!(wm.restored(2), Effects::default(), "its restore event changes nothing more");
    }

    #[test]
    fn activating_a_shown_window_just_focuses_it() {
        let mut wm = wm_with(2);
        let fx = wm.activate(1);
        assert_eq!((fx.focus, fx.activated.len(), fx.restore.len()), (Some(1), 0, 0));
        cmd(&mut wm, Command::MoveTo(ws(2)));
        cmd(&mut wm, Command::Switch(ws(1)));
        let fx = wm.activate(1);
        assert_eq!((fx.focus, fx.activated), (Some(1), vec![(id("A"), 1)]));
        assert_eq!(wm.activate(9), Effects::default(), "not managed: not the Wm's to do");
    }

    #[test]
    fn minimized_windows_come_back_as_they_were() {
        let mut wm = wm_with(2);
        cmd(&mut wm, Command::ToggleFloating);
        wm.minimize(2);
        let fx = wm.activate(2);
        assert_eq!(rect(&fx, 2), None, "still floating");
        wm.focused(1);
        cmd(&mut wm, Command::Maximize);
        wm.minimize(1);
        assert_eq!(rect(&wm.activate(1), 1), Some(AREA), "still maximized");
    }

    #[test]
    fn a_window_restoring_itself_lands_where_you_are() {
        let mut wm = wm_with(2);
        wm.minimize(2);
        cmd(&mut wm, Command::Switch(ws(2)));
        let fx = wm.restored(2);
        assert_eq!((fx.focus, rect(&fx, 2)), (Some(2), Some(AREA)));
        assert!(wm.is_shown(2));
    }

    #[test]
    fn recent_is_most_recently_focused_first() {
        let mut wm = wm_with(3);
        assert_eq!(wm.recent(), [3, 2, 1]);
        wm.focused(1);
        wm.focused(42);
        assert_eq!(wm.recent(), [42, 1, 3, 2], "unmanaged windows count too");
        wm.activate(2);
        assert_eq!(wm.recent(), [2, 42, 1, 3]);
        wm.remove(42);
        wm.remove(3);
        assert_eq!(wm.recent(), [2, 1]);
        wm.add_minimized(7);
        assert_eq!(wm.recent(), [2, 1, 7], "found minimized at startup: least recent");
        assert!(wm.is_minimized(7));
    }

    #[test]
    fn close_asks_and_removal_waits_for_the_window() {
        let mut wm = wm_with(2);
        assert_eq!(cmd(&mut wm, Command::Close).close, Some(2));
        assert!(wm.manages(2));
    }

    // Several monitors.

    #[test]
    fn each_monitor_tiles_in_its_own_area() {
        let wm = dual();
        let fx = wm.placement();
        assert_eq!(rect(&fx, 1), Some(left_half(AREA)));
        assert_eq!(rect(&fx, 2), Some(right_half(AREA)));
        assert_eq!(rect(&fx, 3), Some(RIGHT_AREA));
    }

    #[test]
    fn new_windows_open_where_the_pointer_is() {
        let mut wm = dual();
        wm.pointer(Point { x: 2000, y: 10 });
        let fx = wm.add(4);
        assert_eq!(rect(&fx, 4), Some(right_half(RIGHT_AREA)), "splits B's focused window, not A's");
        assert_eq!(rect(&fx, 2), Some(right_half(AREA)));
        wm.pointer(Point { x: -50, y: 10 });
        assert_eq!(wm.active_area(), RIGHT_AREA, "off every monitor: stays where it was");
    }

    #[test]
    fn windows_found_at_startup_stay_on_their_monitor() {
        let mut wm = Wm::new(two());
        wm.add_at(1, Point { x: 2000, y: 500 });
        wm.add_at(2, Point { x: 100, y: 500 });
        let fx = wm.placement();
        assert_eq!((rect(&fx, 1), rect(&fx, 2)), (Some(RIGHT_AREA), Some(AREA)));
    }

    #[test]
    fn switching_workspaces_on_one_monitor_leaves_the_other_alone() {
        let mut wm = dual();
        let fx = cmd(&mut wm, Command::Switch(ws(2)));
        assert_eq!(fx.activated, [(id("A"), 1)]);
        assert_eq!(fx.hide, [1, 2]);
        assert!(wm.is_shown(3));
        assert_eq!(fx.place, [(3, RIGHT_AREA)]);
        wm.pointer(Point { x: 2000, y: 10 });
        let fx = cmd(&mut wm, Command::Switch(ws(5)));
        assert_eq!(fx.activated, [(id("B"), 4)]);
        assert_eq!(fx.hide, [3]);
        let s = wm.screens();
        assert_eq!((s[0].active, s[1].active), (1, 4));
    }

    #[test]
    fn focus_past_the_side_goes_to_the_next_monitor_with_the_pointer() {
        let mut wm = dual();
        let fx = cmd(&mut wm, Command::Focus(Direction::Right));
        assert_eq!(fx.focus, Some(3));
        assert_eq!(fx.pointer, Some(Warp::To(Point { x: 2240, y: 512 })));
        // COSMIC puts back the workspace A showed when focus left the tiling
        // (the same one), then activates the one B shows.
        assert_eq!(fx.activated, [(id("A"), 0), (id("B"), 0)]);
        assert_eq!(wm.active_area(), RIGHT_AREA);
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)).focus, Some(2), "back to what A had focused");
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)).focus, Some(1), "then within A again");
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)), Effects::default(), "nothing left of A");
    }

    #[test]
    fn focus_onto_an_empty_monitor_leaves_nothing_focused_there() {
        let mut wm = dual();
        wm.focused(3);
        cmd(&mut wm, Command::Move(Direction::Left));
        assert_eq!(wm.focused_window(), Some(3));
        let fx = cmd(&mut wm, Command::SwitchOutput(Direction::Right));
        assert_eq!((fx.focus, fx.pointer), (None, Some(Warp::To(Point { x: 2240, y: 512 }))));
        assert_eq!(cmd(&mut wm, Command::Move(Direction::Left)), Effects::default(), "no window has focus to move");
    }

    #[test]
    fn super_alt_switches_monitors_only_where_there_is_one() {
        let mut wm = dual();
        assert_eq!(cmd(&mut wm, Command::SwitchOutput(Direction::Left)), Effects::default());
        assert_eq!(cmd(&mut wm, Command::SwitchOutput(Direction::Up)), Effects::default());
        assert_eq!(cmd(&mut wm, Command::SwitchOutput(Direction::Right)).focus, Some(3));
    }

    #[test]
    fn next_output_picks_the_nearest_overlapping_one() {
        // A in the middle; B right, C further right, D below A, E up and
        // off to the right (not overlapping A vertically... it does
        // horizontally with B only).
        let screens = Screens::new(vec![
            (id("A"), Rect { left: 0, top: 0, right: 100, bottom: 100 }),
            (id("C"), Rect { left: 200, top: 0, right: 300, bottom: 100 }),
            (id("B"), Rect { left: 100, top: 50, right: 200, bottom: 150 }),
            (id("D"), Rect { left: 50, top: 100, right: 150, bottom: 200 }),
            (id("E"), Rect { left: 100, top: -100, right: 200, bottom: 0 }),
        ])
        .unwrap();
        let wm: Wm<u32> = Wm::new(screens);
        assert_eq!(wm.next_output(0, Direction::Right), Some(2), "B beats C");
        assert_eq!(wm.next_output(0, Direction::Down), Some(3));
        assert_eq!(wm.next_output(0, Direction::Up), None, "E doesn't overlap A sideways");
        assert_eq!(wm.next_output(2, Direction::Up), Some(4), "but does B");
        assert_eq!(wm.next_output(0, Direction::Left), None);
    }

    #[test]
    fn moving_past_the_side_takes_the_window_to_the_next_monitor() {
        let mut wm = dual();
        let fx = cmd(&mut wm, Command::Move(Direction::Right));
        assert_eq!(fx.focus, Some(2));
        assert_eq!(fx.pointer, Some(Warp::To(Point { x: 2240, y: 512 })));
        assert_eq!(rect(&fx, 2), Some(left_half(RIGHT_AREA)), "moving right it comes in on the left");
        assert_eq!(rect(&fx, 3), Some(right_half(RIGHT_AREA)));
        assert_eq!(rect(&fx, 1), Some(AREA));
        assert_eq!(wm.active_area(), RIGHT_AREA);
        let fx = cmd(&mut wm, Command::Move(Direction::Left));
        assert_eq!(rect(&fx, 2), Some(right_half(AREA)), "and back in on the right");
        assert_eq!(rect(&fx, 3), Some(RIGHT_AREA));
    }

    #[test]
    fn super_shift_alt_moves_the_window_whatever_the_tiling() {
        let mut wm = dual();
        wm.focused(1);
        let fx = cmd(&mut wm, Command::MoveToOutput(Direction::Right));
        assert_eq!(rect(&fx, 1), Some(left_half(RIGHT_AREA)), "even from the left of A");
        assert_eq!(rect(&fx, 2), Some(AREA));
        assert_eq!(cmd(&mut wm, Command::MoveToOutput(Direction::Right)), Effects::default(), "nothing right of B");
    }

    #[test]
    fn moving_onto_a_monitor_unmaximizes_what_it_shows() {
        let mut wm = dual();
        wm.focused(3);
        cmd(&mut wm, Command::Maximize);
        wm.focused(2);
        let fx = cmd(&mut wm, Command::MoveToOutput(Direction::Right));
        assert_eq!(rect(&fx, 3), Some(right_half(RIGHT_AREA)));
    }

    #[test]
    fn a_floating_window_moves_to_a_monitor_with_tiling_off_keeping_its_place() {
        let mut wm = dual();
        wm.pointer(Point { x: 2000, y: 10 });
        cmd(&mut wm, Command::ToggleTiling);
        wm.focused(2);
        cmd(&mut wm, Command::ToggleFloating);
        let fx = cmd(&mut wm, Command::MoveToOutput(Direction::Right));
        assert_eq!(fx.carry, [(2, AREA, RIGHT_AREA)]);
        assert_eq!(rect(&fx, 2), None);
    }

    #[test]
    fn moving_a_window_to_a_workspace_on_another_monitor_uses_the_focused_one() {
        let mut wm = dual();
        wm.focused(3);
        // The pointer is on A, the focus on B: Super+Shift+2 moves B's window.
        let fx = cmd(&mut wm, Command::MoveTo(ws(2)));
        assert_eq!(fx.activated, [(id("B"), 1)]);
        assert_eq!(rect(&fx, 3), Some(RIGHT_AREA));
        assert!(wm.is_shown(1) && wm.is_shown(2));
    }

    #[test]
    fn looking_through_workspaces_on_the_way_to_another_monitor_leaves_them_as_they_were() {
        let mut wm = dual();
        cmd(&mut wm, Command::Switch(ws(3)));
        wm.add(5);
        // On A's workspace 3, focus up past the top looks at 2, then 1...
        wm.command(Command::Focus(Direction::Up), Hold(7));
        wm.command(Command::Focus(Direction::Up), Hold(7));
        assert_eq!(wm.screens()[0].active, 0);
        // ...and right, in the same hold, puts 3 back and goes to B.
        let fx = wm.command(Command::Focus(Direction::Right), Hold(7));
        assert_eq!(fx.activated, [(id("A"), 2), (id("B"), 0)]);
        assert_eq!(wm.screens()[0].active, 2);
        assert!(fx.show.contains(&5) && fx.focus == Some(3));
    }

    #[test]
    fn a_new_hold_keeps_the_workspace_you_looked_at() {
        let mut wm = dual();
        cmd(&mut wm, Command::Switch(ws(3)));
        wm.add(5);
        wm.command(Command::Focus(Direction::Up), Hold(7));
        let fx = wm.command(Command::Focus(Direction::Right), Hold(8));
        assert_eq!(fx.activated, [(id("A"), 1), (id("B"), 0)], "remembered afresh: the one it's on");
        assert_eq!(wm.screens()[0].active, 1);
    }

    #[test]
    fn super_alt_alone_never_puts_workspaces_back() {
        let mut wm = dual();
        cmd(&mut wm, Command::Switch(ws(3)));
        wm.add(5);
        wm.command(Command::Focus(Direction::Up), H);
        assert_eq!(cmd(&mut wm, Command::SwitchOutput(Direction::Right)).activated, [(id("B"), 0)]);
        assert_eq!(wm.screens()[0].active, 1);
    }

    #[test]
    fn carrying_a_window_through_workspaces_onto_a_monitor_puts_them_back() {
        let mut wm = Wm::new(Screens::new(vec![(id("A"), AREA), (id("U"), Rect { left: 0, top: -900, right: 1600, bottom: 0 })]).unwrap());
        wm.add(1);
        cmd(&mut wm, Command::Switch(ws(2)));
        wm.add(2);
        // From A's workspace 2, up: onto workspace 1, coming in from below;
        // up again swaps it with 1; up once more and it's off to U.
        wm.command(Command::Move(Direction::Up), Hold(3));
        assert_eq!(wm.screens()[0].active, 0);
        assert_eq!(rect(&wm.placement(), 2), Some(Rect { top: 500, ..AREA }));
        wm.command(Command::Move(Direction::Up), Hold(3));
        let fx = wm.command(Command::Move(Direction::Up), Hold(3));
        assert_eq!(rect(&fx, 2), Some(Rect { left: 0, top: -900, right: 1600, bottom: 0 }));
        assert_eq!(wm.screens()[0].active, 1, "A shows workspace 2 again");
        assert!(fx.hide.contains(&1));
    }

    #[test]
    fn dragging_a_tile_onto_another_monitor_drops_it_into_that_tiling() {
        let mut wm = dual();
        wm.grab(2);
        let on_b = Point { x: 2800, y: 500 };
        assert_eq!(wm.drop_preview(2, on_b, 32), Some(right_half(RIGHT_AREA)));
        let fx = wm.drop(2, on_b, 32);
        assert_eq!(rect(&fx, 2), Some(right_half(RIGHT_AREA)));
        assert_eq!(rect(&fx, 1), Some(AREA));
        assert_eq!((fx.focus, wm.active_area()), (Some(2), RIGHT_AREA));
    }

    #[test]
    fn dragging_onto_an_empty_monitor_fills_it() {
        let mut wm = Wm::new(two());
        wm.add(1);
        wm.add(2);
        wm.grab(1);
        let fx = wm.drop(1, Point { x: 2000, y: 100 }, 32);
        assert_eq!(rect(&fx, 1), Some(RIGHT_AREA));
    }

    #[test]
    fn maximize_fills_the_monitor_its_window_is_on() {
        let mut wm = dual();
        wm.focused(3);
        assert_eq!(rect(&cmd(&mut wm, Command::Maximize), 3), Some(RIGHT_AREA));
        wm.focused(1);
        assert_eq!(rect(&cmd(&mut wm, Command::Maximize), 1), Some(AREA));
    }

    #[test]
    fn closing_a_window_on_another_monitor_leaves_focus_alone() {
        let mut wm = dual();
        let fx = wm.remove(3);
        assert_eq!(fx.focus, None);
        assert_eq!(wm.focused_window(), Some(2));
    }

    #[test]
    fn the_switcher_activates_on_the_windows_own_monitor_without_moving_the_pointer() {
        let mut wm = dual();
        wm.pointer(Point { x: 2000, y: 10 });
        cmd(&mut wm, Command::Switch(ws(4)));
        let fx = wm.activate(3);
        assert_eq!(fx.activated, [(id("B"), 0)]);
        assert_eq!((fx.focus, fx.pointer), (Some(3), None));
    }

    #[test]
    fn unplugging_a_monitor_moves_its_workspaces_to_the_first() {
        let mut wm = dual();
        wm.pointer(Point { x: 2000, y: 10 });
        cmd(&mut wm, Command::Switch(ws(4)));
        wm.add(4);
        cmd(&mut wm, Command::Switch(ws(1)));
        let fx = wm.set_screens(one());
        // B's workspace 1 (3) and 4 (4) go after A's last used slot, 1.
        assert_eq!(fx.pace, Pace::Instant);
        assert_eq!(fx.hide, [3], "A keeps showing its own workspace");
        let s = wm.screens();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].views[1].tiles, [(3, AREA)]);
        assert_eq!(s[0].views[2].tiles, [(4, AREA)]);
        assert_eq!(wm.active_area(), AREA);
    }

    #[test]
    fn an_unplugged_monitors_shown_workspace_stays_shown_if_the_first_showed_nothing() {
        let mut wm = dual();
        cmd(&mut wm, Command::Switch(ws(5)));
        let fx = wm.set_screens(one());
        assert!(fx.hide.is_empty() && fx.show.is_empty());
        assert_eq!(rect(&fx, 3), Some(AREA));
        assert_eq!(wm.screens()[0].active, 1, "the first free slot after workspace 1");
    }

    #[test]
    fn plugging_a_monitor_back_in_brings_its_workspaces_home() {
        let mut wm = dual();
        wm.pointer(Point { x: 2000, y: 10 });
        cmd(&mut wm, Command::Switch(ws(4)));
        wm.add(4);
        cmd(&mut wm, Command::Switch(ws(1)));
        wm.set_screens(one());
        let fx = wm.set_screens(two());
        assert_eq!(fx.show, [3], "B shows its first workspace back");
        assert_eq!(rect(&fx, 3), Some(RIGHT_AREA));
        let s = wm.screens();
        assert_eq!((s[1].id.clone(), s[1].active), (id("B"), 0));
        assert_eq!(s[1].views[3].tiles, [(4, RIGHT_AREA)], "4 back on B's workspace 4");
        assert!(s[0].views[1].tiles.is_empty() && s[0].views[2].tiles.is_empty(), "and gone from A");
        assert_eq!(rect(&fx, 1), Some(left_half(AREA)), "A untouched");
    }

    #[test]
    fn a_workspace_that_came_over_and_got_switched_to_goes_home_and_leaves_an_empty_one() {
        let mut wm = dual();
        wm.set_screens(one());
        cmd(&mut wm, Command::Switch(ws(2)));
        wm.focused(3);
        let fx = wm.set_screens(two());
        assert_eq!(rect(&fx, 3), Some(RIGHT_AREA));
        assert!(fx.hide.is_empty() && fx.show.is_empty(), "it was shown, and still is");
        assert_eq!(wm.screens()[0].active, 1, "A stays on that slot, now empty");
        assert!(wm.focused_window() == Some(3));
    }

    #[test]
    fn a_new_monitor_starts_empty_and_resizes_relayout() {
        let mut wm = wm_with(2);
        let fx = wm.set_screens(two());
        assert_eq!(fx.place.len(), 2);
        let s = wm.screens();
        assert_eq!((s.len(), s[1].active), (2, 0));
        let bigger = Rect { right: 1920, bottom: 1080, ..AREA };
        let fx = wm.set_screens(Screens::new(vec![(id("A"), bigger), (id("B"), RIGHT_AREA)]).unwrap());
        assert_eq!(rect(&fx, 1), Some(left_half(bigger)));
    }

    #[test]
    fn swapping_one_monitor_for_another_keeps_every_window() {
        let mut wm = wm_with(2);
        let fx = wm.set_screens(Screens::new(vec![(id("C"), RIGHT_AREA)]).unwrap());
        assert_eq!(rect(&fx, 1), Some(left_half(RIGHT_AREA)));
        assert!(fx.hide.is_empty());
        assert_eq!(wm.active_area(), RIGHT_AREA);
        // A comes back: its workspace goes home with it.
        let fx = wm.set_screens(Screens::new(vec![(id("A"), AREA), (id("C"), RIGHT_AREA)]).unwrap());
        assert_eq!(rect(&fx, 1), Some(left_half(AREA)));
    }

    #[test]
    fn a_workspace_remembers_every_monitor_it_was_moved_through() {
        let three = |list: &[&str]| {
            Screens::new(list.iter().enumerate().map(|(i, n)| (id(n), Rect { left: 2000 * i as i32, right: 2000 * i as i32 + 1600, ..AREA })).collect())
                .unwrap()
        };
        let mut wm = Wm::new(three(&["A", "B", "C"]));
        wm.pointer(Point { x: 4100, y: 10 });
        wm.add(1);
        wm.set_screens(three(&["A", "B"]));
        // C's workspace is on A now. A goes too, so it lands on B...
        wm.set_screens(three(&["B"]));
        assert!(wm.is_shown(1));
        // ...and when C comes back, it goes all the way home.
        wm.set_screens(three(&["B", "C"]));
        assert_eq!(wm.screens()[1].views[0].tiles.iter().map(|t| t.0).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn nine_full_workspaces_have_no_room_so_the_last_takes_the_rest() {
        let mut wm = Wm::new(two());
        for i in 1..=9 {
            cmd(&mut wm, Command::Switch(ws(i)));
            wm.add(i);
        }
        wm.pointer(Point { x: 2000, y: 10 });
        wm.add(10);
        wm.set_screens(one());
        assert!(wm.manages(10));
        assert_eq!(wm.screens()[0].views[8].tiles.len(), 2);
    }

    #[test]
    fn invariant_every_window_is_on_exactly_one_workspace() {
        let mut wm = dual();
        wm.focused(1);
        cmd(&mut wm, Command::MoveToOutput(Direction::Right));
        cmd(&mut wm, Command::MoveTo(ws(3)));
        wm.set_screens(one());
        wm.set_screens(two());
        cmd(&mut wm, Command::Move(Direction::Left));
        for w in 1..=3 {
            let count = wm.outputs.iter().flat_map(|o| &o.spaces).filter(|s| s.holds(w)).count();
            assert_eq!(count, 1, "window {w}");
        }
    }

    #[test]
    fn autotile_and_its_behaviour() {
        let per = Tiling { autotile: false, ..Tiling::default() };
        let mut wm = Wm::with_tiling(one(), per);
        assert!(wm.add(1).place.is_empty(), "autotile off: new windows float");
        cmd(&mut wm, Command::ToggleTiling);
        assert!(cmd(&mut wm, Command::Switch(ws(2))).place.is_empty(), "the others still don't tile");
        let global = Tiling { behavior: TileBehavior::Global, ..Tiling::default() };
        let mut wm = Wm::with_tiling(two(), global);
        wm.add(1);
        cmd(&mut wm, Command::ToggleTiling);
        wm.pointer(Point { x: 2000, y: 500 });
        assert!(wm.add(2).place.is_empty(), "Global: tiling went off on the other monitor too");
        cmd(&mut wm, Command::ToggleTiling);
        assert_eq!(rect(&wm.placement(), 1), Some(AREA));
    }

    #[test]
    fn tiles_keep_their_gaps_and_maximized_ones_dont() {
        let gaps = Tiling { gaps: Gaps { outer: 0, inner: 1 }, ..Tiling::default() };
        let mut wm = Wm::with_tiling(one(), gaps);
        let fx = wm.add(1);
        assert_eq!(rect(&fx, 1), Some(Rect { left: 1, top: 1, right: 1599, bottom: 999 }));
        let fx = cmd(&mut wm, Command::Maximize);
        assert_eq!(rect(&fx, 1), Some(AREA), "maximized fills the screen, as COSMIC's does");
    }

    #[test]
    fn the_hint_goes_on_the_focused_window_unless_maximized() {
        let mut wm = wm_with(2);
        assert_eq!(wm.hinted(), Some((2, Some(right_half(AREA)))));
        cmd(&mut wm, Command::ToggleFloating);
        assert_eq!(wm.hinted(), Some((2, None)), "floating: wherever it is");
        wm.focused(1);
        cmd(&mut wm, Command::Maximize);
        assert_eq!(wm.hinted(), None);
        wm.focused(42);
        assert_eq!(wm.hinted(), None, "not managed");
    }

    #[test]
    fn the_pointer_follows_focus_where_cosmics_would() {
        let on = Tiling { cursor_follows_focus: true, ..Tiling::default() };
        let mut wm = Wm::with_tiling(one(), on);
        wm.add(1);
        assert_eq!(wm.add_new(2).pointer, Some(Warp::Follow(2)), "a new window");
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)).pointer, Some(Warp::Follow(1)));
        assert_eq!(cmd(&mut wm, Command::Focus(Direction::Left)).pointer, None, "nothing that way");
        assert_eq!(cmd(&mut wm, Command::Move(Direction::Right)).pointer, None, "a move within the tiling");
        let fx = cmd(&mut wm, Command::MoveTo(ws(3)));
        assert_eq!(fx.pointer, Some(Warp::Follow(1)), "taken to another workspace");
        assert_eq!(cmd(&mut wm, Command::Switch(ws(1))).pointer, None);
        let mut off = wm_with(2);
        assert_eq!(cmd(&mut off, Command::Focus(Direction::Left)).pointer, None, "off by default");
        assert_eq!(off.add_new(3).pointer, None);
    }

    #[test]
    fn windows_raising_themselves_elsewhere_follow_the_policy() {
        let policy = |activation_policy| Tiling { activation_policy, ..Tiling::default() };
        let mut wm = Wm::with_tiling(one(), policy(ActivationPolicy::FocusIfActiveWorkspace));
        wm.add(1);
        cmd(&mut wm, Command::Switch(ws(2)));
        wm.add(2);
        assert_eq!(wm.raised(2), None, "shown: nothing to decide");
        let fx = wm.raised(1).unwrap();
        assert_eq!((fx.hide, fx.focus, fx.activated.len()), (vec![1], Some(2), 0), "stays hidden, focus stays");
        assert!(!wm.is_shown(1));
        let mut wm = Wm::with_tiling(one(), policy(ActivationPolicy::Focus));
        wm.add(1);
        cmd(&mut wm, Command::Switch(ws(2)));
        let fx = wm.raised(1).unwrap();
        assert_eq!((fx.focus, fx.show), (Some(1), vec![1]), "COSMIC's stock policy: you go there");
        assert!(wm.is_shown(1));
        assert_eq!(wm.raised(42), None, "not managed");
    }

    const SIZABLE: u32 = 0x10CF_0000;
    /// Riot Client's: caption and minimize box, no sizing border.
    const FIXED: u32 = 0x14C2_0000;

    fn size(width: i32, height: i32) -> Size {
        Size { width, height }
    }

    /// League's client, which refused its half of a 1600x1000 screen and
    /// stayed 1280x720.
    fn league() -> Wm<u32> {
        let mut wm = wm_with(2);
        wm.took(1, FIXED, size(1280, 720)).unwrap();
        wm
    }

    #[test]
    fn a_fixed_size_window_is_asked_for_its_tile_first() {
        let mut wm = Wm::new(one());
        assert_eq!(rect(&wm.add(1), 1), Some(AREA), "no sizing border, asked all the same");
        assert_eq!(wm.took(1, FIXED, Size::of(AREA)), None, "it took it");
        assert_eq!(rect(&wm.add(2), 1), Some(left_half(AREA)));
    }

    #[test]
    fn a_window_bigger_than_its_tile_goes_over_its_neighbour_not_off_the_screen() {
        let wm = league();
        // Centred it would start at -240; it starts at the screen's edge.
        assert_eq!(rect(&wm.placement(), 1), Some(Rect { left: 0, top: 140, right: 1280, bottom: 860 }));
        assert_eq!(rect(&wm.placement(), 2), Some(right_half(AREA)), "its neighbour keeps its whole tile");
    }

    /// saturn 2026-11-26: Firefox in the left half of the 5120x1440
    /// monitor, League's game in the right half at 2576x1440 after it went
    /// borderless. Asked for its 2560x1440 tile it fills it; if it keeps
    /// 2576 anyway, it ends at the screen's edge and covers 16 px of
    /// Firefox's tile, the overflow and no more.
    #[test]
    fn leagues_game_never_leaves_the_monitor() {
        let monitor = Rect { left: 0, top: 0, right: 5120, bottom: 1440 };
        let mut wm = Wm::new(Screens::new(vec![(id("S"), monitor)]).unwrap());
        wm.add(1);
        let tile = rect(&wm.add(2), 2).unwrap();
        assert_eq!(tile, right_half(monitor), "the tile's size is what it's asked for first");
        const BORDERLESS: u32 = 0x940a_0000;
        assert_eq!(wm.took(2, BORDERLESS, size(2560, 1440)), None, "took it: nothing over Firefox");
        let (_, fx) = wm.took(2, BORDERLESS, size(2576, 1440)).unwrap();
        let at = rect(&fx, 2).unwrap();
        assert_eq!(at, Rect { left: 2544, top: 0, right: 5120, bottom: 1440 });
        assert!(at.left >= monitor.left && at.right <= monitor.right);
        assert_eq!(rect(&fx, 1), Some(left_half(monitor)));
        assert_eq!(left_half(monitor).right - at.left, 16);
    }

    /// saturn 2026-11-26: the Claude Code console lost WS_THICKFRAME (the
    /// shell takes it with the title bar, and conhost flips its style)
    /// while filling a tile, was latched at that size, and stayed a narrow
    /// column after League closed and its tile grew. Now a style without a
    /// sizing border means nothing while the window takes its tile, and
    /// when the tile grows it's asked for the new size and fills it.
    #[test]
    fn a_console_losing_its_sizing_border_still_follows_its_tile() {
        const CONSOLE: u32 = 0x14ff_0000;
        const STRIPPED: u32 = 0x142b_0000;
        let mut wm = wm_with(3);
        let small = rect(&wm.placement(), 3).unwrap();
        assert_eq!(wm.took(3, CONSOLE, Size::of(small)), None);
        assert_eq!(wm.took(3, STRIPPED, Size::of(small)), None, "the style flickered; it still took its tile");
        // League (2) closes: the console's tile grows, and it's placed at it.
        let fx = wm.remove(2);
        let grown = rect(&fx, 3).unwrap();
        assert!(grown.width() > small.width() || grown.height() > small.height());
        assert_eq!(wm.took(3, STRIPPED, Size::of(grown)), None, "it took the new size: fills it");
        assert_eq!(rect(&wm.placement(), 3), Some(grown));
    }

    /// The same console, had it really refused its small tile once (char
    /// cells snapping it narrower, say): the next tile size is asked for,
    /// and taking it ends the letterbox.
    #[test]
    fn a_window_that_refused_one_size_is_asked_for_the_next() {
        let mut wm = wm_with(2);
        let (fit, _) = wm.took(2, SIZABLE, size(780, 990)).unwrap();
        assert_eq!(fit, Fit::Refused { asked: Size::of(right_half(AREA)), took: size(780, 990), style: SIZABLE });
        // Its neighbour closes: the whole screen, asked for and taken.
        assert_eq!(rect(&wm.remove(1), 2), Some(AREA));
        assert_eq!(wm.took(2, SIZABLE, Size::of(AREA)).map(|(f, _)| f), Some(Fit::Fills));
    }

    #[test]
    fn a_window_that_refuses_its_tile_is_letterboxed_and_not_asked_again() {
        let mut wm = wm_with(2);
        let tile = right_half(AREA);
        assert_eq!(wm.took(2, SIZABLE, Size::of(tile)), None, "took it: nothing to do");
        let (fit, fx) = wm.took(2, SIZABLE, size(800, 450)).unwrap();
        assert_eq!(fit, Fit::Refused { asked: Size::of(tile), took: size(800, 450), style: SIZABLE });
        let boxed = Rect { left: 800, top: 275, right: 1600, bottom: 725 };
        assert_eq!(rect(&fx, 2), Some(boxed));
        assert_eq!(rect(&fx, 1), Some(left_half(AREA)));
        // Placed there, it's still that size: no new placement, so no loop.
        assert_eq!(wm.took(2, SIZABLE, size(800, 450)), None);
        cmd(&mut wm, Command::Focus(Direction::Left));
        assert_eq!(rect(&wm.placement(), 2), Some(boxed));
    }

    #[test]
    fn a_letterboxed_window_whose_style_changes_is_asked_again() {
        let mut wm = league();
        assert_eq!(wm.letterboxed(), vec![1]);
        assert_eq!(wm.restyled(1, FIXED), None, "the same style: still letterboxed");
        let (fit, fx) = wm.restyled(1, SIZABLE).unwrap();
        assert_eq!(fit, Fit::Fills);
        assert_eq!(rect(&fx, 1), Some(left_half(AREA)));
        assert!(wm.letterboxed().is_empty());
        assert_eq!(wm.restyled(2, FIXED), None, "never letterboxed: nothing to ask again");
    }

    #[test]
    fn the_hint_and_the_animation_go_where_the_window_is() {
        let mut wm = league();
        wm.focused(1);
        let at = rect(&wm.placement(), 1);
        assert_eq!(wm.hinted().map(|(_, r)| r), Some(at));
        assert_eq!(wm.screens()[0].views[0].tiles.iter().find(|&&(w, _)| w == 1).map(|&(_, r)| r), at);
    }

    /// Focus changes, workspace switches and coming back keep it where it
    /// is, at its size: nothing asks it for its tile's again.
    #[test]
    fn letterboxing_holds_through_focus_and_workspace_switches() {
        let mut wm = league();
        let at = rect(&wm.placement(), 1);
        cmd(&mut wm, Command::Focus(Direction::Left));
        assert_eq!(rect(&wm.placement(), 1), at, "focus moves nothing");
        cmd(&mut wm, Command::Switch(ws(2)));
        assert_eq!(rect(&cmd(&mut wm, Command::Switch(ws(1))), 1), at);
        // Moved to the other half, a tile of the same size: still
        // letterboxed, kept on the screen from the right this time.
        let moved = rect(&cmd(&mut wm, Command::Move(Direction::Right)), 1).unwrap();
        assert_eq!(moved, Rect { left: 320, top: 140, right: 1600, bottom: 860 });
    }

    #[test]
    fn a_fixed_window_that_resizes_itself_is_centred_again() {
        let mut wm = league();
        let (_, fx) = wm.took(1, FIXED, size(640, 360)).unwrap();
        assert_eq!(rect(&fx, 1), Some(Rect { left: 80, top: 320, right: 720, bottom: 680 }));
        assert_eq!(wm.took(1, FIXED, size(640, 360)), None);
    }

    #[test]
    fn untiled_windows_have_no_fit_to_check() {
        let mut wm = wm_with(1);
        cmd(&mut wm, Command::ToggleFloating);
        assert_eq!(wm.took(1, SIZABLE, size(10, 10)), None);
        assert_eq!(wm.took(42, SIZABLE, size(10, 10)), None);
    }

    #[test]
    fn a_closed_windows_fit_goes_with_it() {
        let mut wm = league();
        wm.remove(1);
        // The same handle again, a new window.
        assert_eq!(rect(&wm.add(1), 1), Some(right_half(AREA)));
    }
}
