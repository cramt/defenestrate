//! The window manager's state: which windows are on which workspace, how
//! they tile (tree.rs), and which has focus. It never touches Win32; every
//! change returns the `Effects` the desktop layer then applies. That keeps
//! it testable here, and means no state is borrowed while Win32 calls
//! re-enter the message loop.
//!
//! The commands are COSMIC's (cosmic-comp's data/keybindings.ron), minus
//! the ones that need UI this doesn't have yet: stacking, swap mode,
//! focusing groups, and other monitors.

use crate::tree::{Direction, Rect, Tree, neighbour};

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

/// What the desktop layer should do to make the screen match the state.
#[derive(Debug, PartialEq, Eq)]
pub struct Effects<W> {
    pub show: Vec<W>,
    pub hide: Vec<W>,
    pub place: Vec<(W, Rect)>,
    pub focus: Option<W>,
    pub close: Option<W>,
}

impl<W> Default for Effects<W> {
    fn default() -> Self {
        Effects { show: Vec::new(), hide: Vec::new(), place: Vec::new(), focus: None, close: None }
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

    fn insert(&mut self, w: W, area: Rect, gap: i32) {
        let next_to = self.focus.filter(|&f| self.tiled.contains(f));
        self.tiled.insert(w, next_to, area, gap);
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
    gap: i32,
}

impl<W: Copy + Eq> Wm<W> {
    pub fn new(area: Rect, gap: i32) -> Self {
        Wm { spaces: std::array::from_fn(|_| Workspace::new()), current: 0, area, gap }
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

    fn arrange(&self) -> Vec<(W, Rect)> {
        let s = self.current();
        let mut place = if s.tiling { s.tiled.layout(self.area, self.gap) } else { Vec::new() };
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
        let (area, gap) = (self.area, self.gap);
        self.current_mut().insert(w, area, gap);
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
        Effects { place: self.arrange(), ..Effects::default() }
    }

    fn switch(&mut self, to: usize) -> Effects<W> {
        if to == self.current {
            return Effects::default();
        }
        let hide = self.current().windows();
        self.current = to;
        Effects { show: self.current().windows(), hide, ..self.relayout() }
    }

    fn send(&mut self, to: usize) -> Effects<W> {
        let Some(w) = self.current().focus else { return Effects::default() };
        if to == self.current {
            return Effects::default();
        }
        let floating = self.current().floating.contains(&w);
        self.current_mut().remove(w);
        let (area, gap) = (self.area, self.gap);
        let target = &mut self.spaces[to];
        if floating {
            target.floating.push(w);
            target.focus = Some(w);
        } else {
            target.insert(w, area, gap);
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
        let (area, gap) = (self.area, self.gap);
        let focus = self.current().focus;
        let s = self.current_mut();
        match c {
            Command::Focus(dir) => {
                let place = self.arrange();
                let from = focus.and_then(|f| place.iter().find(|&&(w, _)| w == f)).map(|&(_, r)| r);
                let target = match from {
                    Some(r) => neighbour(r, dir, &place),
                    // Nothing tiled has focus (a floating window does): start
                    // from the first tile.
                    None => place.first().map(|&(w, _)| w),
                };
                let Some(target) = target else { return Effects::default() };
                self.current_mut().focus = Some(target);
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
                self.relayout()
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
                    s.tiled.insert(f, None, area, gap);
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
        let mut wm = Wm::new(AREA, 0);
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
        assert_eq!(fx.hide, [1, 2]);
        assert!(fx.show.is_empty());
        assert_eq!(fx.focus, None);
        assert!(wm.manages(1) && !wm.is_shown(1));
        assert_eq!(wm.hidden(), [1, 2]);
        wm.add(3);
        let fx = wm.command(Command::SwitchStep(Step::Previous));
        assert_eq!((fx.hide, fx.show, fx.focus), (vec![3], vec![1, 2], Some(2)));
        assert_eq!(wm.command(Command::SwitchStep(Step::Previous)), Effects::default(), "no wrapping");
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
    fn close_asks_and_removal_waits_for_the_window() {
        let mut wm = wm_with(2);
        assert_eq!(wm.command(Command::Close).close, Some(2));
        assert!(wm.manages(2));
    }
}
