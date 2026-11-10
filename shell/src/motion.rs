//! COSMIC's window animations as plain geometry: where each window is
//! drawn, and how opaque, at a given moment. The desktop layer draws that
//! with DWM thumbnails while the real windows already sit where they end up
//! (overlay.rs). Pure, so it's tested here.
//!
//! What cosmic-comp (src/shell/) does, which this copies, quirks included:
//!
//! - Tiling (`layout/tiling/mod.rs`). Every change pushes a new tree onto a
//!   queue (`TreeQueue`, :115) with a duration: `ANIMATION_DURATION`, 200 ms,
//!   for nearly everything, zero for interactive and keyboard resizes
//!   (`resize`, :2613, and `grabs/resize.rs`:332). While one animation runs
//!   nothing retargets it; when it ends, every tree queued meanwhile
//!   collapses into the newest, which then animates from where the finished
//!   one ended, for the longest of their durations (`update_animation_state`,
//!   :2363). A queued tree only counts once the windows have taken the
//!   geometry it gave them, or 500 ms on (`TilingBlocker`, blocker.rs), so
//!   nothing animates before the apps have resized.
//! - A tiling frame (`render`, :4033): progress is
//!   `ease(EaseInOutCubic, 0, 1, ms_since / ms_total)` in whole milliseconds.
//!   Windows in both trees go linearly over that progress from old rect to new
//!   (`render_new_tree`, :5770; `EaseRectangle`, utils/tween.rs, rounds
//!   position and size separately), contents stretched. New ones are drawn
//!   where they land with that progress as their opacity; ones only in the
//!   old tree stay where they were at one minus it, drawn above the rest
//!   (`render_old_tree`, :5135, runs first and front-to-back puts first on
//!   top). A closed window has no surface left to draw, so it just vanishes:
//!   so does a destroyed window's thumbnail. Maximized windows are skipped:
//!   the floating layer draws them.
//! - Maximize (`floating/mod.rs` `map_maximized`, :352): the floating layer
//!   eases the window from its tile to the whole area, EaseInOutCubic over
//!   200 ms (`Animation::Tiled`, progress in f64 seconds). An animation
//!   already running for the window is kept instead.
//! - Unmaximize (`workspace.rs` :1004, `floating/mod.rs` `unmap`, :665): the
//!   tiling draws the window back in its tile at once, and the floating
//!   layer, above it, shrinks a copy from the whole area to that tile
//!   (`Animation::Minimize`): 320 ms, EaseInOutCubic, opaque for the first
//!   half and fading linearly to nothing over the second.
//! - Workspaces (`mod.rs` `activate`, :510; `focus/order.rs`, :146): 200 ms
//!   EaseInOutCubic in whole milliseconds. The old one leaves by the output's
//!   height, upwards when going to a higher number, as the new one comes in
//!   from the other side (vertical is COSMIC's default layout). Switching
//!   during a switch starts a new one from the workspace that was coming in,
//!   jumping to it; "switching" to the workspace already active ends it.
//!   Each workspace keeps its own tiling and floating animations meanwhile.
//!
//! Left out because the shell has no such thing: stacks, swap and resize
//! modes, the overview, minimizing to a panel icon, fullscreen requests.

use std::collections::VecDeque;
use std::time::Duration;

use crate::tree::Rect;

/// Tiling, maximize and workspace switching.
pub const ANIMATION: Duration = Duration::from_millis(200);
/// What unmaximizing borrows from minimizing (floating/mod.rs:53).
pub const MINIMIZE_ANIMATION: Duration = Duration::from_millis(320);
/// How long a queued tiling change waits for the windows to take their new
/// sizes before it animates anyway (`TilingBlocker::is_processed`,
/// tiling/blocker.rs:47).
pub const BLOCKER_TIMEOUT: Duration = Duration::from_millis(500);

/// keyframe 1.1's EaseInOutCubic, which COSMIC uses, input clamped as its
/// `ease` clamps it.
pub fn ease_in_out_cubic(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x < 0.5 {
        4.0 * x * x * x
    } else {
        let m = x - 1.0;
        m * (2.0 * x - 2.0) * (2.0 * x - 2.0) + 1.0
    }
}

/// COSMIC's EaseRectangle: position and size each interpolated and
/// rounded, not the four edges.
pub fn ease_rect(from: Rect, to: Rect, p: f64) -> Rect {
    let lerp = |a: i32, b: i32| (f64::from(a) + f64::from(b - a) * p).round() as i32;
    let (left, top) = (lerp(from.left, to.left), lerp(from.top, to.top));
    let (w, h) = (lerp(from.width(), to.width()), lerp(from.height(), to.height()));
    Rect { left, top, right: left + w, bottom: top + h }
}

/// The tiling's and the workspace switch's progress: COSMIC divides whole
/// milliseconds.
fn eased_ms(since: Duration, total: Duration) -> f64 {
    ease_in_out_cubic(since.as_millis() as f64 / total.as_millis() as f64)
}

fn shifted(r: Rect, dy: i32) -> Rect {
    Rect { top: r.top + dy, bottom: r.bottom + dy, ..r }
}

/// One window as it should look right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite<W> {
    pub window: W,
    pub rect: Rect,
    /// 0 to 1.
    pub alpha: f64,
}

/// What the window manager shows on a workspace: the tiles in tree order
/// (a maximized window keeps its tile, as in COSMIC), and which window is
/// maximized over everything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View<W> {
    pub tiles: Vec<(W, Rect)>,
    pub maximized: Option<W>,
    pub area: Rect,
}

/// How a layout change gets onto the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pace {
    #[default]
    Animated,
    /// A resize: the queue's zero-duration tree.
    Instant,
}

type Tiles<W> = Vec<(W, Rect)>;

fn find<W: Eq>(tiles: &[(W, Rect)], w: &W) -> Option<Rect> {
    tiles.iter().find(|(x, _)| x == w).map(|&(_, r)| r)
}

#[derive(Clone, Debug)]
struct Layout<W> {
    tiles: Tiles<W>,
    duration: Duration,
    pushed: Duration,
    /// The windows took their places (or the blocker timed out). Stays so.
    processed: bool,
}

/// COSMIC's TreeQueue. `trees[0]` is what's on screen, or what a running
/// animation starts from; `trees[1]` what it heads for; the rest wait.
#[derive(Clone, Debug)]
struct Queue<W> {
    trees: VecDeque<Layout<W>>,
    start: Option<Duration>,
}

impl<W: Copy + Eq> Queue<W> {
    fn at_rest(tiles: Tiles<W>) -> Self {
        let rest = Layout { tiles, duration: Duration::ZERO, pushed: Duration::ZERO, processed: true };
        Queue { trees: VecDeque::from([rest]), start: None }
    }

    fn latest(&self) -> &Tiles<W> {
        &self.trees.back().expect("never empty").tiles
    }

    fn push(&mut self, tiles: Tiles<W>, duration: Duration, now: Duration) {
        self.trees.push_back(Layout { tiles, duration, pushed: now, processed: false });
    }

    fn busy(&self) -> bool {
        self.start.is_some() || self.trees.len() > 1
    }

    /// `update_animation_state`. `settled`: every window placed so far is
    /// where it was put, which on Windows is the nearest thing to a client
    /// having acked and committed its configure.
    fn update(&mut self, now: Duration, settled: bool) {
        for t in self.trees.iter_mut().skip(1) {
            t.processed |= settled || now.saturating_sub(t.pushed) >= BLOCKER_TIMEOUT;
        }
        let mut ready = self.trees.iter().skip(1).take_while(|t| t.processed).count();
        if let Some(start) = self.start {
            if now.saturating_sub(start) < self.trees[1].duration {
                return;
            }
            self.start = None;
            self.trees.pop_front();
            ready -= 1;
        }
        let merged = if ready > 1 { self.trees.drain(1..ready).map(|t| t.duration).max() } else { None };
        if ready > 0 {
            let duration = &mut self.trees[1].duration;
            *duration = merged.map_or(*duration, |m| m.max(*duration));
            self.start = Some(now);
        }
    }

    /// Front to back.
    fn sprites(&self, now: Duration, maximized: Option<W>, out: &mut Vec<Sprite<W>>) {
        let shown = |w: &W| maximized.as_ref() != Some(w);
        let Some(start) = self.start else {
            out.extend(self.trees[0].tiles.iter().filter(|(w, _)| shown(w)).map(|&(window, rect)| Sprite { window, rect, alpha: 1.0 }));
            return;
        };
        let (reference, target, duration) = (&self.trees[0].tiles, &self.trees[1].tiles, self.trees[1].duration);
        let p = if duration.is_zero() { 1.0 } else { eased_ms(now.saturating_sub(start), duration) };
        for &(window, rect) in reference {
            if shown(&window) && find(target, &window).is_none() {
                out.push(Sprite { window, rect, alpha: 1.0 - p });
            }
        }
        for &(window, to) in target.iter().filter(|(w, _)| shown(w)) {
            out.push(match find(reference, &window) {
                Some(from) => Sprite { window, rect: ease_rect(from, to, p), alpha: 1.0 },
                None => Sprite { window, rect: to, alpha: p },
            });
        }
    }
}

/// The floating layer's animations (floating/mod.rs:68), the two a tiled
/// window gets from maximizing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Float {
    /// `Animation::Tiled`: to the whole area, which `map_maximized` gives it.
    Tiled { start: Duration, from: Rect },
    /// `Animation::Minimize`, which unmaximizing reuses.
    Minimize { start: Duration, from: Rect, to: Rect },
}

impl Float {
    fn start(self) -> Duration {
        match self {
            Float::Tiled { start, .. } | Float::Minimize { start, .. } => start,
        }
    }

    fn duration(self) -> Duration {
        match self {
            Float::Tiled { .. } => ANIMATION,
            Float::Minimize { .. } => MINIMIZE_ANIMATION,
        }
    }

    fn progress(self, now: Duration) -> f64 {
        now.saturating_sub(self.start()).min(self.duration()).as_secs_f64() / self.duration().as_secs_f64()
    }

    fn rect(self, now: Duration, area: Rect) -> Rect {
        let (from, to) = match self {
            Float::Tiled { from, .. } => (from, area),
            Float::Minimize { from, to, .. } => (from, to),
        };
        ease_rect(from, to, ease_in_out_cubic(self.progress(now)))
    }

    fn alpha(self, now: Duration) -> f64 {
        match self {
            Float::Tiled { .. } => 1.0,
            Float::Minimize { .. } => 1.0 - (self.progress(now) - 0.5).max(0.0) * 2.0,
        }
    }
}

#[derive(Clone, Debug)]
struct Space<W> {
    queue: Queue<W>,
    maximized: Option<W>,
    area: Rect,
    floats: Vec<(W, Float)>,
}

impl<W: Copy + Eq> Space<W> {
    fn at_rest(view: View<W>) -> Self {
        Space { queue: Queue::at_rest(view.tiles), maximized: view.maximized, area: view.area, floats: Vec::new() }
    }

    fn busy(&self) -> bool {
        self.queue.busy() || !self.floats.is_empty()
    }

    fn push(&mut self, view: View<W>, pace: Pace, rect_of: &dyn Fn(W) -> Option<Rect>, now: Duration) {
        let before = self.queue.latest().clone();
        self.area = view.area;
        if view.maximized != self.maximized {
            if let Some(w) = self.maximized {
                self.floats.retain(|(x, _)| *x != w);
                if let Some(to) = find(&view.tiles, &w) {
                    self.floats.push((w, Float::Minimize { start: now, from: view.area, to }));
                }
            }
            if let Some(w) = view.maximized
                && !self.floats.iter().any(|(x, _)| *x == w)
                && let Some(from) = find(&before, &w).or_else(|| rect_of(w))
            {
                self.floats.push((w, Float::Tiled { start: now, from }));
            }
            self.maximized = view.maximized;
        }
        // A change that changes nothing pushes nothing. COSMIC would queue
        // a no-op tree for some, holding the next change back by 200 ms.
        if view.tiles != before {
            let duration = match pace {
                Pace::Animated => ANIMATION,
                Pace::Instant => Duration::ZERO,
            };
            self.queue.push(view.tiles, duration, now);
        }
    }

    fn update(&mut self, now: Duration, settled: bool) {
        self.queue.update(now, settled);
        self.floats.retain(|(_, f)| now.saturating_sub(f.start()) < f.duration());
    }

    /// Front to back: the floating layer, then the tiling. `floating` is
    /// what floats here, top first, where it really is.
    fn sprites(&self, now: Duration, floating: &[(W, Rect)], out: &mut Vec<Sprite<W>>) {
        for &(window, f) in &self.floats {
            if let Float::Minimize { .. } = f {
                out.push(Sprite { window, rect: f.rect(now, self.area), alpha: f.alpha(now) });
            }
        }
        if let Some(window) = self.maximized {
            out.push(match self.floats.iter().find(|(x, _)| *x == window) {
                Some(&(_, f)) => Sprite { window, rect: f.rect(now, self.area), alpha: f.alpha(now) },
                None => Sprite { window, rect: self.area, alpha: 1.0 },
            });
        }
        out.extend(floating.iter().map(|&(window, rect)| Sprite { window, rect, alpha: 1.0 }));
        self.queue.sprites(now, self.maximized, out);
    }
}

pub struct Frame<W> {
    /// Front to back.
    pub sprites: Vec<Sprite<W>>,
    /// False once everything shown is at rest.
    pub moving: bool,
}

pub struct Animator<W> {
    spaces: Vec<Space<W>>,
    active: usize,
    /// COSMIC's `previously_active`: the workspace sliding out, and when
    /// the switch began.
    previous: Option<(usize, Duration)>,
}

impl<W: Copy + Eq> Animator<W> {
    /// Everything at rest as `views` has it, `active` shown.
    pub fn new(views: Vec<View<W>>, active: usize) -> Self {
        Animator { spaces: views.into_iter().map(Space::at_rest).collect(), active, previous: None }
    }

    /// Stops everything where the views say, without animating.
    pub fn rest(&mut self, views: Vec<View<W>>, active: usize) {
        *self = Animator::new(views, active);
    }

    /// Takes the window manager's current views, one per workspace, and
    /// animates the ones that changed. `rect_of` is where a window really
    /// is, for one maximized from outside the tiling.
    pub fn push(&mut self, views: Vec<View<W>>, pace: Pace, rect_of: &dyn Fn(W) -> Option<Rect>, now: Duration) {
        for (space, view) in self.spaces.iter_mut().zip(views) {
            space.push(view, pace, rect_of, now);
        }
    }

    /// A switch to workspace `to`, which may be the one already shown.
    pub fn activate(&mut self, to: usize, now: Duration) {
        if to != self.active {
            self.previous = Some((self.active, now));
            self.active = to;
        } else {
            self.previous = None;
        }
    }

    /// Whatever started at `from` starts at `to` instead: the overlay took
    /// that long to come up, and the motion shouldn't be half over by then.
    pub fn restart(&mut self, from: Duration, to: Duration) {
        let moved = |t: &mut Duration| {
            if *t == from {
                *t = to;
            }
        };
        if let Some((_, start)) = &mut self.previous {
            moved(start);
        }
        for space in &mut self.spaces {
            if let Some(start) = &mut space.queue.start {
                moved(start);
            }
            for t in &mut space.queue.trees {
                moved(&mut t.pushed);
            }
            for (_, f) in &mut space.floats {
                match f {
                    Float::Tiled { start, .. } | Float::Minimize { start, .. } => moved(start),
                }
            }
        }
    }

    /// `w` was dragged and is being dropped back into the tiling: in
    /// COSMIC it left the tree when it was picked up, so it lands as a new
    /// window does, fading in.
    pub fn forget(&mut self, w: W) {
        for space in &mut self.spaces {
            for t in &mut space.queue.trees {
                t.tiles.retain(|(x, _)| *x != w);
            }
            space.floats.retain(|(x, _)| *x != w);
            if space.maximized == Some(w) {
                space.maximized = None;
            }
        }
    }

    /// The screen at `now`. `settled`: the windows are all where they were
    /// last put. `floating(i)` is what floats on workspace `i`, top first,
    /// where it really is.
    pub fn frame(&mut self, now: Duration, settled: bool, floating: &dyn Fn(usize) -> Vec<(W, Rect)>) -> Frame<W> {
        if let Some((_, start)) = self.previous
            && now.saturating_sub(start).as_millis() >= ANIMATION.as_millis()
        {
            self.previous = None;
        }
        for space in &mut self.spaces {
            space.update(now, settled);
        }
        let mut sprites = Vec::new();
        let current = &self.spaces[self.active];
        let Some((previous, start)) = self.previous else {
            current.sprites(now, &floating(self.active), &mut sprites);
            return Frame { sprites, moving: current.busy() };
        };
        let height = current.area.height();
        let moved = (f64::from(height) * eased_ms(now.saturating_sub(start), ANIMATION)).round() as i32;
        let (out_by, in_by) = if previous < self.active { (-moved, height - moved) } else { (moved, moved - height) };
        let mut incoming = Vec::new();
        current.sprites(now, &floating(self.active), &mut incoming);
        let mut outgoing = Vec::new();
        self.spaces[previous].sprites(now, &floating(previous), &mut outgoing);
        sprites.extend(incoming.into_iter().map(|s| Sprite { rect: shifted(s.rect, in_by), ..s }));
        sprites.extend(outgoing.into_iter().map(|s| Sprite { rect: shifted(s.rect, out_by), ..s }));
        Frame { sprites, moving: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { left: 0, top: 0, right: 1600, bottom: 1000 };
    const LEFT: Rect = Rect { left: 0, top: 0, right: 800, bottom: 1000 };
    const RIGHT: Rect = Rect { left: 800, top: 0, right: 1600, bottom: 1000 };

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn view(tiles: &[(u32, Rect)], maximized: Option<u32>) -> View<u32> {
        View { tiles: tiles.to_vec(), maximized, area: AREA }
    }

    fn nowhere(_: u32) -> Option<Rect> {
        None
    }

    /// Two workspaces, the first shown with `tiles`.
    fn animator(tiles: &[(u32, Rect)]) -> Animator<u32> {
        Animator::new(vec![view(tiles, None), view(&[], None)], 0)
    }

    fn push(a: &mut Animator<u32>, first: View<u32>, pace: Pace, at: u64) {
        a.push(vec![first, view(&[], None)], pace, &nowhere, ms(at));
    }

    fn frame(a: &mut Animator<u32>, at: u64) -> Frame<u32> {
        a.frame(ms(at), true, &|_| Vec::new())
    }

    fn sprites(a: &mut Animator<u32>, at: u64) -> Vec<Sprite<u32>> {
        frame(a, at).sprites
    }

    fn sprite(s: &[Sprite<u32>], w: u32) -> Sprite<u32> {
        *s.iter().find(|s| s.window == w).unwrap_or_else(|| panic!("no {w} in {s:?}"))
    }

    #[test]
    fn easing_matches_keyframe() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(0.25), 0.0625);
        assert_eq!(ease_in_out_cubic(0.5), 0.5);
        assert_eq!(ease_in_out_cubic(0.75), 0.9375);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert_eq!(ease_in_out_cubic(-1.0), 0.0, "clamped");
        assert_eq!(ease_in_out_cubic(3.0), 1.0, "clamped");
    }

    #[test]
    fn rectangles_ease_by_position_and_size() {
        let from = Rect { left: 0, top: 0, right: 3, bottom: 3 };
        let to = Rect { left: 1, top: 1, right: 2, bottom: 2 };
        // Edges would give left 0.5 → 1 (rounded away from zero) and right
        // 2.5 → 3; position and size give left 1 and width 2.
        assert_eq!(ease_rect(from, to, 0.5), Rect { left: 1, top: 1, right: 3, bottom: 3 });
        assert_eq!(ease_rect(LEFT, AREA, 0.0), LEFT);
        assert_eq!(ease_rect(LEFT, AREA, 1.0), AREA);
    }

    #[test]
    fn a_new_window_fades_in_where_it_lands_as_its_neighbour_shrinks() {
        let mut a = animator(&[(1, AREA)]);
        assert!(!frame(&mut a, 0).moving, "starts at rest");
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 1000);
        let s = sprites(&mut a, 1000);
        assert_eq!(s, [Sprite { window: 1, rect: AREA, alpha: 1.0 }, Sprite { window: 2, rect: RIGHT, alpha: 0.0 }]);
        let s = sprites(&mut a, 1050);
        assert_eq!(sprite(&s, 2).alpha, 0.0625, "eased, not linear");
        let s = sprites(&mut a, 1100);
        assert_eq!(sprite(&s, 1).rect, Rect { left: 0, top: 0, right: 1200, bottom: 1000 });
        assert_eq!(sprite(&s, 2), Sprite { window: 2, rect: RIGHT, alpha: 0.5 });
        let f = frame(&mut a, 1200);
        assert!(!f.moving);
        assert_eq!(f.sprites, [Sprite { window: 1, rect: LEFT, alpha: 1.0 }, Sprite { window: 2, rect: RIGHT, alpha: 1.0 }]);
    }

    #[test]
    fn progress_counts_whole_milliseconds() {
        let mut a = animator(&[(1, AREA)]);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 0);
        frame(&mut a, 0);
        let at = |a: &mut Animator<u32>, us: u64| sprite(&a.frame(Duration::from_micros(us), true, &|_| Vec::new()).sprites, 2).alpha;
        assert_eq!(at(&mut a, 50_000), at(&mut a, 50_999));
    }

    #[test]
    fn windows_leaving_fade_out_above_the_rest() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(1, AREA)], None), Pace::Animated, 0);
        frame(&mut a, 0);
        let s = sprites(&mut a, 100);
        assert_eq!(s[0], Sprite { window: 2, rect: RIGHT, alpha: 0.5 }, "first, so on top");
        assert_eq!(s[1].rect, Rect { left: 0, top: 0, right: 1200, bottom: 1000 });
        assert_eq!(sprites(&mut a, 200), [Sprite { window: 1, rect: AREA, alpha: 1.0 }]);
    }

    #[test]
    fn changes_during_an_animation_wait_then_merge_into_the_newest() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(2, LEFT), (1, RIGHT)], None), Pace::Animated, 0);
        frame(&mut a, 0);
        // Three more while the swap runs; only the last one is ever shown,
        // and 3, which came and went meanwhile, never is.
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 50);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT), (3, AREA)], None), Pace::Animated, 60);
        push(&mut a, view(&[(1, AREA)], None), Pace::Animated, 100);
        assert_eq!(sprite(&sprites(&mut a, 150), 1).rect.left, 750, "the swap carries on");
        // The swap ends and the merged change starts from where it ended.
        let s = sprites(&mut a, 200);
        assert_eq!(s, [Sprite { window: 2, rect: LEFT, alpha: 1.0 }, Sprite { window: 1, rect: RIGHT, alpha: 1.0 }]);
        let s = sprites(&mut a, 300);
        assert_eq!(s[0], Sprite { window: 2, rect: LEFT, alpha: 0.5 }, "left meanwhile, so it fades");
        assert_eq!(s[1].rect, Rect { left: 400, top: 0, right: 1600, bottom: 1000 });
        assert!(s.iter().all(|s| s.window != 3));
        assert!(!frame(&mut a, 400).moving);
    }

    #[test]
    fn a_merge_takes_the_longest_duration() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(2, LEFT), (1, RIGHT)], None), Pace::Animated, 0);
        frame(&mut a, 0);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 50);
        push(&mut a, view(&[(1, Rect { right: 900, ..LEFT }), (2, Rect { left: 900, ..RIGHT })], None), Pace::Instant, 60);
        frame(&mut a, 200);
        assert!(frame(&mut a, 399).moving, "200 ms, not the resize's zero");
        assert!(!frame(&mut a, 400).moving);
    }

    #[test]
    fn a_resize_jumps_but_waits_for_a_running_animation() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        let wider = [(1, Rect { right: 900, ..LEFT }), (2, Rect { left: 900, ..RIGHT })];
        push(&mut a, view(&wider, None), Pace::Instant, 0);
        let f = frame(&mut a, 0);
        assert_eq!(sprite(&f.sprites, 1).rect.right, 900, "at once");
        assert!(f.moving, "still has to be taken off the queue");
        assert!(!frame(&mut a, 16).moving);

        push(&mut a, view(&[(2, LEFT), (1, RIGHT)], None), Pace::Animated, 1000);
        frame(&mut a, 1000);
        push(&mut a, view(&[(2, Rect { right: 700, ..LEFT }), (1, Rect { left: 700, ..RIGHT })], None), Pace::Instant, 1050);
        assert_eq!(sprite(&sprites(&mut a, 1199), 1).rect.left, 800, "held back until the swap is done");
        assert_eq!(sprite(&sprites(&mut a, 1200), 1).rect.left, 700);
    }

    #[test]
    fn nothing_changed_nothing_moves() {
        let mut a = animator(&[(1, AREA)]);
        push(&mut a, view(&[(1, AREA)], None), Pace::Animated, 0);
        assert!(!frame(&mut a, 0).moving);
    }

    #[test]
    fn maximizing_grows_the_window_out_of_its_tile_over_the_rest() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], Some(2)), Pace::Animated, 0);
        let s = sprites(&mut a, 100);
        assert_eq!(s[0], Sprite { window: 2, rect: Rect { left: 400, top: 0, right: 1600, bottom: 1000 }, alpha: 1.0 });
        assert_eq!(s[1..], [Sprite { window: 1, rect: LEFT, alpha: 1.0 }], "the tiling skips it; the rest stays");
        assert!(frame(&mut a, 199).moving);
        let f = frame(&mut a, 200);
        assert!(!f.moving);
        assert_eq!(f.sprites[0], Sprite { window: 2, rect: AREA, alpha: 1.0 });
    }

    #[test]
    fn unmaximizing_shows_the_tile_at_once_under_a_shrinking_fading_copy() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], Some(2)), Pace::Animated, 0);
        frame(&mut a, 1000);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 1000);
        let s = sprites(&mut a, 1080);
        // A quarter of the way: eased 0.0625, still opaque.
        assert_eq!(s[0], Sprite { window: 2, rect: Rect { left: 50, top: 0, right: 1600, bottom: 1000 }, alpha: 1.0 });
        assert_eq!(s[1..], [Sprite { window: 1, rect: LEFT, alpha: 1.0 }, Sprite { window: 2, rect: RIGHT, alpha: 1.0 }]);
        let copy = sprite(&sprites(&mut a, 1240), 2);
        assert!((copy.alpha - 0.5).abs() < 1e-9, "fades over the second half, linearly: {}", copy.alpha);
        assert!(frame(&mut a, 1319).moving);
        assert_eq!(sprites(&mut a, 1320).len(), 2, "320 ms, then just the tiles");
    }

    #[test]
    fn maximizing_again_while_the_copy_shrinks_keeps_shrinking_it() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], Some(2)), Pace::Animated, 0);
        frame(&mut a, 1000);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 1000);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], Some(2)), Pace::Animated, 1100);
        let s = sprites(&mut a, 1160);
        let shrinking = Sprite { window: 2, rect: ease_rect(AREA, RIGHT, 0.5), alpha: 1.0 };
        assert_eq!(s[..2], [shrinking, shrinking], "drawn twice, both as the copy");
        assert_eq!(sprites(&mut a, 1320)[0], Sprite { window: 2, rect: AREA, alpha: 1.0 }, "then it jumps");
    }

    #[test]
    fn floating_windows_are_drawn_where_they_are_between_maximized_and_tiles() {
        let mut a = animator(&[(1, LEFT)]);
        push(&mut a, view(&[(1, LEFT)], Some(1)), Pace::Animated, 0);
        let float = Rect { left: 100, top: 100, right: 300, bottom: 300 };
        let s = a.frame(ms(500), true, &|_| vec![(5, float)]).sprites;
        assert_eq!(s, [Sprite { window: 1, rect: AREA, alpha: 1.0 }, Sprite { window: 5, rect: float, alpha: 1.0 }]);
    }

    #[test]
    fn workspaces_slide_by_the_screen_height() {
        let mut a = Animator::new(vec![view(&[(1, AREA)], None), view(&[(2, LEFT)], None), view(&[(3, RIGHT)], None)], 0);
        a.activate(1, ms(0));
        let f = frame(&mut a, 0);
        assert_eq!(f.sprites, [Sprite { window: 2, rect: shifted(LEFT, 1000), alpha: 1.0 }, Sprite { window: 1, rect: AREA, alpha: 1.0 }]);
        let s = sprites(&mut a, 100);
        assert_eq!(sprite(&s, 1).rect, shifted(AREA, -500), "leaves upwards for a higher number");
        assert_eq!(sprite(&s, 2).rect, shifted(LEFT, 500));
        assert!(frame(&mut a, 199).moving);
        assert_eq!(sprites(&mut a, 200), [Sprite { window: 2, rect: LEFT, alpha: 1.0 }]);

        a.activate(0, ms(1000));
        assert_eq!(sprite(&sprites(&mut a, 1100), 1).rect, shifted(AREA, -500), "comes from above for a lower one");
    }

    #[test]
    fn switching_mid_switch_starts_over_from_the_one_coming_in() {
        let mut a = Animator::new(vec![view(&[(1, AREA)], None), view(&[(2, LEFT)], None), view(&[(3, RIGHT)], None)], 0);
        a.activate(1, ms(0));
        a.activate(2, ms(150));
        let s = sprites(&mut a, 150);
        assert_eq!(s, [Sprite { window: 3, rect: shifted(RIGHT, 1000), alpha: 1.0 }, Sprite { window: 2, rect: LEFT, alpha: 1.0 }]);
        assert!(s.iter().all(|s| s.window != 1), "the first one is gone at once");
        // "Switching" to the one already shown ends the slide there.
        a.activate(2, ms(200));
        let f = frame(&mut a, 200);
        assert!(!f.moving);
        assert_eq!(f.sprites, [Sprite { window: 3, rect: RIGHT, alpha: 1.0 }]);
    }

    #[test]
    fn each_workspace_keeps_animating_while_it_slides() {
        let mut a = Animator::new(vec![view(&[(1, AREA)], None), view(&[], None)], 0);
        a.push(vec![view(&[(1, LEFT), (2, RIGHT)], None), view(&[], None)], Pace::Animated, &nowhere, ms(0));
        frame(&mut a, 0);
        a.activate(1, ms(100));
        let s = sprites(&mut a, 100);
        assert_eq!(sprite(&s, 2), Sprite { window: 2, rect: RIGHT, alpha: 0.5 });
    }

    #[test]
    fn tiling_waits_for_the_windows_to_take_their_places() {
        let mut a = animator(&[(1, AREA)]);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 0);
        let unsettled = |a: &mut Animator<u32>, at: u64| a.frame(ms(at), false, &|_| Vec::new());
        let f = unsettled(&mut a, 100);
        assert!(f.moving, "the overlay holds the old picture meanwhile");
        assert_eq!(f.sprites, [Sprite { window: 1, rect: AREA, alpha: 1.0 }]);
        // The windows moved at 150: it starts then, from the start.
        assert_eq!(sprite(&sprites(&mut a, 150), 2).alpha, 0.0);
        assert_eq!(sprite(&sprites(&mut a, 250), 2).alpha, 0.5);

        // A hung app: 500 ms, then it animates anyway.
        push(&mut a, view(&[(1, AREA)], None), Pace::Animated, 1000);
        assert_eq!(unsettled(&mut a, 1499).sprites.len(), 2);
        unsettled(&mut a, 1500);
        assert_eq!(unsettled(&mut a, 1600).sprites[0], Sprite { window: 2, rect: RIGHT, alpha: 0.5 });
    }

    #[test]
    fn an_animation_runs_on_while_newer_changes_wait_to_settle() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        push(&mut a, view(&[(2, LEFT), (1, RIGHT)], None), Pace::Animated, 0);
        frame(&mut a, 0);
        push(&mut a, view(&[(1, LEFT), (2, RIGHT)], None), Pace::Animated, 50);
        let unsettled = |a: &mut Animator<u32>, at: u64| a.frame(ms(at), false, &|_| Vec::new());
        assert_eq!(sprite(&unsettled(&mut a, 100).sprites, 1).rect.left, 400, "unaffected");
        let f = unsettled(&mut a, 200);
        assert!(f.moving);
        assert_eq!(sprite(&f.sprites, 1).rect, RIGHT, "done, and the next one isn't ready yet");
        assert_eq!(sprite(&sprites(&mut a, 260), 1).rect, RIGHT, "starts once it is");
        assert_eq!(sprite(&sprites(&mut a, 360), 1).rect.left, 400);
    }

    #[test]
    fn a_restart_moves_what_began_then() {
        let mut a = Animator::new(vec![view(&[(1, AREA)], None), view(&[(2, LEFT)], None)], 0);
        a.push(vec![view(&[(1, AREA)], None), view(&[(2, LEFT), (3, RIGHT)], None)], Pace::Animated, &nowhere, ms(100));
        frame(&mut a, 100);
        a.activate(1, ms(100));
        a.restart(ms(100), ms(150));
        let s = sprites(&mut a, 150);
        assert_eq!(sprite(&s, 3).rect, shifted(RIGHT, 1000), "the slide starts at 150");
        assert_eq!(sprite(&s, 3).alpha, 0.0, "and so does the fade");
        assert!(frame(&mut a, 349).moving);
        assert!(!frame(&mut a, 350).moving);
    }

    #[test]
    fn a_dropped_tile_fades_in_where_it_lands() {
        let mut a = animator(&[(1, LEFT), (2, RIGHT)]);
        a.forget(2);
        push(&mut a, view(&[(2, LEFT), (1, RIGHT)], None), Pace::Animated, 0);
        frame(&mut a, 0);
        let s = sprites(&mut a, 100);
        assert_eq!(sprite(&s, 2), Sprite { window: 2, rect: LEFT, alpha: 0.5 });
        assert_eq!(sprite(&s, 1).rect, ease_rect(LEFT, RIGHT, 0.5));
    }
}
