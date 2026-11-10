//! The window manager's state: which windows are on which workspace, how
//! they tile (tree.rs), and which has focus. It never touches Win32; every
//! change returns the `Effects` the desktop layer then applies. That keeps
//! it testable here, and means no state is borrowed while Win32 calls
//! re-enter the message loop.
//!
//! The commands are COSMIC's (cosmic-comp's data/keybindings.ron), minus
//! the ones that need UI this doesn't have yet: stacking, swap mode,
//! focusing groups, and other monitors.

use crate::motion::{Pace, View};
use crate::tree::{Axis, Direction, Point, Rect, Tree};

pub const WORKSPACES: usize = 9;

/// A workspace number that exists. Keys 1–9 map to 0–8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceId(usize);

impl WorkspaceId {
    pub fn from_key(n: u32) -> Option<Self> {
        (1..=WORKSPACES as u32).contains(&n).then(|| WorkspaceId(n as usize - 1))
    }
}

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
    Send(WorkspaceId),
    SwitchStep(Step),
    SendStep(Step),
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
    /// A workspace switch asked for this one, even if it's already shown:
    /// in COSMIC that cuts a running switch short.
    pub activated: Option<usize>,
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
            activated: None,
        }
    }
}

struct Workspace<W> {
    tiled: Tree<W>,
    /// Managed (hidden and shown with the workspace) but never placed.
    floating: Vec<W>,
    focus: Option<W>,
    maximized: Option<W>,
    tiling: bool,
}

impl<W: Copy + Eq> Workspace<W> {
    fn new() -> Self {
        Workspace { tiled: Tree::default(), floating: Vec::new(), focus: None, maximized: None, tiling: true }
    }

    fn contains(&self, w: W) -> bool {
        self.tiled.contains(w) || self.floating.contains(&w)
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
}

pub struct Wm<W> {
    spaces: [Workspace<W>; WORKSPACES],
    current: usize,
    area: Rect,
}

impl<W: Copy + Eq> Wm<W> {
    pub fn new(area: Rect) -> Self {
        Wm { spaces: std::array::from_fn(|_| Workspace::new()), current: 0, area }
    }

    fn current(&self) -> &Workspace<W> {
        &self.spaces[self.current]
    }

    fn current_mut(&mut self) -> &mut Workspace<W> {
        &mut self.spaces[self.current]
    }

    fn workspace_of(&self, w: W) -> Option<usize> {
        self.spaces.iter().position(|s| s.contains(w))
    }

    pub fn manages(&self, w: W) -> bool {
        self.workspace_of(w).is_some()
    }

    /// On the workspace being shown. Managed windows elsewhere are ones
    /// this hid, so their hide events aren't the window going away.
    pub fn is_shown(&self, w: W) -> bool {
        self.current().contains(w)
    }

    /// Every window this has hidden; shown again if the shell exits.
    pub fn hidden(&self) -> Vec<W> {
        self.spaces.iter().enumerate().filter(|&(i, _)| i != self.current).flat_map(|(_, s)| s.windows()).collect()
    }

    pub fn focused_window(&self) -> Option<W> {
        self.current().focus
    }

    pub fn active(&self) -> usize {
        self.current
    }

    /// What workspace `i` shows, for animating: its tiles even when
    /// one of them is maximized over the rest.
    pub fn view(&self, i: usize) -> View<W> {
        let s = &self.spaces[i];
        View { tiles: if s.tiling { s.tiled.layout(self.area) } else { Vec::new() }, maximized: s.maximized, area: self.area }
    }

    pub fn views(&self) -> Vec<View<W>> {
        (0..WORKSPACES).map(|i| self.view(i)).collect()
    }

    /// Windows on workspace `i` that the shell doesn't place.
    pub fn floating(&self, i: usize) -> Vec<W> {
        let s = &self.spaces[i];
        let mut free = if s.tiling { s.floating.clone() } else { s.windows() };
        free.retain(|&w| Some(w) != s.maximized);
        free
    }

    /// Every window the tiling places, on every workspace: the ones that
    /// go without a title bar.
    pub fn tiled(&self) -> Vec<W> {
        self.spaces.iter().filter(|s| s.tiling).flat_map(|s| s.tiled.windows()).collect()
    }

    /// Super+drag picked up `w`; it gets focus, and a maximized one goes
    /// back to its tile, as in COSMIC.
    pub fn grab(&mut self, w: W) -> Option<Grab> {
        let s = self.current_mut();
        if !s.contains(w) {
            return None;
        }
        s.focus = Some(w);
        if !(s.tiling && s.tiled.contains(w)) {
            return Some(Grab::Free);
        }
        if s.maximized == Some(w) {
            s.maximized = None;
        }
        Some(Grab::Tiled)
    }

    /// Where dragged `w` would tile if dropped at `p`.
    pub fn drop_preview(&self, w: W, p: Point, band: i32) -> Option<Rect> {
        self.current().tiled.drop_preview(w, p, self.area, band)
    }

    /// Drops dragged `w` at `p`; laid out again even if it lands nowhere
    /// new, so it snaps back.
    pub fn drop(&mut self, w: W, p: Point, band: i32) -> Effects<W> {
        let area = self.area;
        self.current_mut().tiled.drop(w, p, area, band);
        self.relayout()
    }

    /// The edge of tiled `w` that Super+right-drag grabbed by `corner`.
    pub fn edge(&self, w: W, corner: [Direction; 2]) -> Option<(Axis, i32)> {
        self.current().tiled.edge(w, corner, self.area)
    }

    pub fn move_edge(&mut self, w: W, corner: [Direction; 2], at: i32) -> Effects<W> {
        let area = self.area;
        self.current_mut().tiled.move_edge(w, corner, at, area);
        Effects { pace: Pace::Instant, ..self.placement() }
    }

    fn arrange(&self) -> Vec<(W, Rect)> {
        let s = self.current();
        let mut place = if s.tiling { s.tiled.layout(self.area) } else { Vec::new() };
        if let Some(m) = s.maximized {
            place.retain(|&(w, _)| w != m);
            place.push((m, self.area));
        }
        place
    }

    fn relayout(&self) -> Effects<W> {
        Effects { place: self.arrange(), focus: self.current().focus, ..Effects::default() }
    }

    pub fn add(&mut self, w: W) -> Effects<W> {
        if self.manages(w) {
            return Effects::default();
        }
        let area = self.area;
        self.current_mut().insert(w, area);
        self.relayout()
    }

    pub fn remove(&mut self, w: W) -> Effects<W> {
        let Some(i) = self.workspace_of(w) else { return Effects::default() };
        self.spaces[i].remove(w);
        if i == self.current { self.relayout() } else { Effects::default() }
    }

    /// Something else focused `w` (a click, a window raising itself).
    pub fn focused(&mut self, w: W) {
        let s = self.current_mut();
        if s.contains(w) {
            s.focus = Some(w);
        }
    }

    pub fn set_area(&mut self, area: Rect) -> Effects<W> {
        self.area = area;
        self.placement()
    }

    /// Every window where it belongs, touching nothing else.
    pub fn placement(&self) -> Effects<W> {
        Effects { place: self.arrange(), pace: Pace::Instant, ..Effects::default() }
    }

    fn switch(&mut self, to: usize) -> Effects<W> {
        if to == self.current {
            return Effects { activated: Some(to), ..Effects::default() };
        }
        let hide = self.current().windows();
        self.current = to;
        Effects { show: self.current().windows(), hide, activated: Some(to), ..self.relayout() }
    }

    fn send(&mut self, to: usize) -> Effects<W> {
        let Some(w) = self.current().focus else { return Effects::default() };
        if to == self.current {
            return Effects::default();
        }
        let floating = self.current().floating.contains(&w);
        self.current_mut().remove(w);
        let area = self.area;
        let target = &mut self.spaces[to];
        if floating {
            target.floating.push(w);
            target.focus = Some(w);
        } else {
            target.insert(w, area);
        }
        Effects { hide: vec![w], ..self.relayout() }
    }

    fn step(&self, step: Step) -> usize {
        match step {
            Step::Next => (self.current + 1).min(WORKSPACES - 1),
            Step::Previous => self.current.saturating_sub(1),
        }
    }

    pub fn command(&mut self, c: Command) -> Effects<W> {
        let area = self.area;
        let focus = self.current().focus;
        let s = self.current_mut();
        match c {
            Command::Focus(dir) => {
                if !s.tiling {
                    return Effects::default();
                }
                let target = match focus.filter(|&f| s.tiled.contains(f)) {
                    Some(f) => s.tiled.focus(f, dir, area),
                    // Nothing tiled has focus (a floating window does): start
                    // from the first tile.
                    None => s.tiled.windows().first().copied(),
                };
                let Some(target) = target else { return Effects::default() };
                s.focus = Some(target);
                Effects { focus: Some(target), ..Effects::default() }
            }
            Command::Move(dir) => {
                let Some(f) = focus else { return Effects::default() };
                s.tiled.r#move(f, dir);
                self.relayout()
            }
            Command::Resize { dir, outwards } => {
                let Some(f) = focus else { return Effects::default() };
                s.tiled.resize(f, dir, outwards);
                // COSMIC's keyboard resize isn't animated either.
                Effects { pace: Pace::Instant, ..self.relayout() }
            }
            Command::ToggleOrientation => {
                let Some(f) = focus else { return Effects::default() };
                s.tiled.toggle_orientation(f);
                self.relayout()
            }
            Command::ToggleTiling => {
                s.tiling = !s.tiling;
                self.relayout()
            }
            Command::ToggleFloating => {
                let Some(f) = focus else { return Effects::default() };
                if s.tiled.contains(f) {
                    s.tiled.remove(f);
                    s.floating.push(f);
                } else {
                    s.floating.retain(|&w| w != f);
                    s.tiled.insert(f, None, area);
                }
                self.relayout()
            }
            Command::Maximize => {
                s.maximized = if s.maximized == focus { None } else { focus };
                self.relayout()
            }
            // Removal happens when the window actually goes away; it may
            // ask "save changes?" first.
            Command::Close => Effects { close: focus, ..Effects::default() },
            Command::Switch(WorkspaceId(to)) => self.switch(to),
            Command::Send(WorkspaceId(to)) => self.send(to),
            Command::SwitchStep(step) => self.switch(self.step(step)),
            Command::SendStep(step) => self.send(self.step(step)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { left: 0, top: 0, right: 1600, bottom: 1000 };

    fn wm_with(n: u32) -> Wm<u32> {
        let mut wm = Wm::new(AREA);
        for w in 1..=n {
            wm.add(w);
        }
        wm
    }

    fn ws(n: u32) -> WorkspaceId {
        WorkspaceId::from_key(n).unwrap()
    }

    fn rect(fx: &Effects<u32>, w: u32) -> Option<Rect> {
        fx.place.iter().find(|&&(x, _)| x == w).map(|&(_, r)| r)
    }

    #[test]
    fn workspace_keys() {
        assert_eq!(WorkspaceId::from_key(0), None);
        assert_eq!(WorkspaceId::from_key(1), Some(WorkspaceId(0)));
        assert_eq!(WorkspaceId::from_key(9), Some(WorkspaceId(8)));
        assert_eq!(WorkspaceId::from_key(10), None);
    }

    #[test]
    fn new_windows_take_focus_and_split_it() {
        let mut wm = wm_with(1);
        let fx = wm.add(2);
        assert_eq!(fx.focus, Some(2));
        assert_eq!(rect(&fx, 1), Some(Rect { left: 0, top: 0, right: 800, bottom: 1000 }));
        assert_eq!(wm.add(2), Effects::default(), "adding twice is a no-op");
    }

    #[test]
    fn directional_focus() {
        // 1 | (2 / 3), focus on 3.
        let mut wm = wm_with(3);
        assert_eq!(wm.command(Command::Focus(Direction::Up)).focus, Some(2));
        assert_eq!(wm.command(Command::Focus(Direction::Left)).focus, Some(1));
        assert_eq!(wm.command(Command::Focus(Direction::Left)), Effects::default());
        assert_eq!(wm.focused_window(), Some(1));
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
        let fx = wm.command(Command::Maximize);
        assert_eq!(rect(&fx, 2), Some(AREA));
        let fx = wm.command(Command::Maximize);
        assert_eq!(rect(&fx, 2), Some(Rect { left: 800, top: 0, right: 1600, bottom: 1000 }));
    }

    #[test]
    fn floating_leaves_the_tiles_and_comes_back() {
        let mut wm = wm_with(2);
        let fx = wm.command(Command::ToggleFloating);
        assert_eq!(rect(&fx, 2), None);
        assert_eq!(rect(&fx, 1), Some(AREA));
        assert!(wm.manages(2) && wm.is_shown(2));
        let fx = wm.command(Command::ToggleFloating);
        assert!(rect(&fx, 2).is_some());
    }

    #[test]
    fn tiling_off_places_nothing() {
        let mut wm = wm_with(2);
        assert!(wm.command(Command::ToggleTiling).place.is_empty());
        assert!(wm.add(3).place.is_empty());
        assert_eq!(wm.command(Command::ToggleTiling).place.len(), 3);
    }

    #[test]
    fn switching_hides_and_shows_and_remembers() {
        let mut wm = wm_with(2);
        let fx = wm.command(Command::Switch(ws(2)));
        assert_eq!(fx.activated, Some(1));
        assert_eq!(fx.hide, [1, 2]);
        assert!(fx.show.is_empty());
        assert_eq!(fx.focus, None);
        assert!(wm.manages(1) && !wm.is_shown(1));
        assert_eq!(wm.hidden(), [1, 2]);
        wm.add(3);
        let fx = wm.command(Command::SwitchStep(Step::Previous));
        assert_eq!((fx.hide, fx.show, fx.focus), (vec![3], vec![1, 2], Some(2)));
        let fx = wm.command(Command::SwitchStep(Step::Previous));
        assert_eq!(fx, Effects { activated: Some(0), ..Effects::default() }, "no wrapping, but it counts as a switch");
    }

    #[test]
    fn sending_moves_the_focused_window_away() {
        let mut wm = wm_with(2);
        let fx = wm.command(Command::Send(ws(3)));
        assert_eq!(fx.hide, [2]);
        assert_eq!(fx.focus, Some(1));
        assert_eq!(rect(&fx, 1), Some(AREA));
        assert!(wm.manages(2) && !wm.is_shown(2));
        // A window on another workspace closing touches nothing on screen.
        assert_eq!(wm.remove(2), Effects::default());
        assert!(!wm.manages(2));
    }

    #[test]
    fn only_tiled_windows_on_tiling_workspaces_lose_their_title_bars() {
        let mut wm = wm_with(3);
        wm.command(Command::ToggleFloating);
        assert_eq!(wm.tiled(), [1, 2]);
        wm.command(Command::Send(ws(2)));
        wm.command(Command::Focus(Direction::Left));
        wm.command(Command::Send(ws(2)));
        assert_eq!(wm.tiled(), [2, 1], "hidden on another workspace, still tiled");
        wm.command(Command::Switch(ws(2)));
        wm.command(Command::ToggleTiling);
        assert_eq!(wm.tiled(), [2]);
    }

    #[test]
    fn dragging_a_tile_drops_it_into_the_tiling() {
        let mut wm = wm_with(3);
        wm.command(Command::Maximize);
        assert_eq!(wm.grab(1), Some(Grab::Tiled));
        assert_eq!(wm.focused_window(), Some(1));
        assert_eq!(wm.grab(3), Some(Grab::Tiled));
        let to_left_edge = Point { x: 0, y: 500 };
        let third = Rect { left: 0, top: 0, right: 533, bottom: 1000 };
        assert_eq!(wm.drop_preview(3, to_left_edge, 32), Some(third));
        let fx = wm.drop(3, to_left_edge, 32);
        assert_eq!(rect(&fx, 3), Some(third), "no longer maximized");
        assert_eq!(rect(&fx, 2), Some(Rect { left: 1067, top: 0, right: 1600, bottom: 1000 }));
        wm.command(Command::ToggleFloating);
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
    fn views_keep_a_maximized_windows_tile() {
        let mut wm = wm_with(2);
        wm.command(Command::Maximize);
        let v = wm.view(0);
        assert_eq!(v.maximized, Some(2));
        assert_eq!(v.tiles.len(), 2);
        assert_eq!(rect(&wm.placement(), 2), Some(AREA), "while the real window fills the screen");
        wm.command(Command::ToggleTiling);
        assert!(wm.view(0).tiles.is_empty());
        assert_eq!(wm.floating(0), [1], "all of them float, but the maximized one is drawn as such");
    }

    #[test]
    fn resizes_are_instant_and_the_rest_animate() {
        let mut wm = wm_with(2);
        assert_eq!(wm.add(3).pace, Pace::Animated);
        assert_eq!(wm.command(Command::Resize { dir: Direction::Left, outwards: true }).pace, Pace::Instant);
        assert_eq!(wm.move_edge(1, [Direction::Right, Direction::Down], 900).pace, Pace::Instant);
        assert_eq!(wm.command(Command::Maximize).pace, Pace::Animated);
    }

    #[test]
    fn close_asks_and_removal_waits_for_the_window() {
        let mut wm = wm_with(2);
        assert_eq!(wm.command(Command::Close).close, Some(2));
        assert!(wm.manages(2));
    }
}
