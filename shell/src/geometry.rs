//! Rectangle sums desktop.rs does on real windows. Pure, so they're tested
//! on the build machine.

use crate::tree::{Direction, Point, Rect};

/// A window's width and height, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

impl Size {
    pub fn of(r: Rect) -> Size {
        Size { width: r.width(), height: r.height() }
    }
}

impl std::fmt::Display for Size {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// A window of `size` centred in `tile`, on `monitor`: where a window that
/// won't take its tile's size goes. One bigger than its tile overflows it,
/// as nothing can make it smaller, but never off its monitor: centred, it
/// went past the screen's edge as far as it went over its neighbour, and
/// a game confining the cursor to its window lost that strip (League at
/// 2576 in a 2560 tile, saturn 2026-11-26). So it's pushed back onto the
/// monitor, which puts all of the overflow over the neighbour on the other
/// side; that much is unavoidable. Bigger than the monitor itself, it
/// starts at the monitor's top left corner, so its title bar or the top
/// left of its client stays reachable.
pub fn letterboxed(tile: Rect, size: Size, monitor: Rect) -> Rect {
    let along = |start: i32, len: i32, lo: i32, hi: i32, size: i32| {
        let centred = start + (len - size).div_euclid(2);
        centred.min(hi - size).max(lo)
    };
    let left = along(tile.left, tile.width(), monitor.left, monitor.right, size.width);
    let top = along(tile.top, tile.height(), monitor.top, monitor.bottom, size.height);
    Rect { left, top, right: left + size.width, bottom: top + size.height }
}

/// COSMIC keeps a floating window's place relative to its output when it
/// moves it to another (`position_relative`): `r` on `from`, carried to `to`.
pub fn carried(r: Rect, from: Rect, to: Rect) -> Rect {
    let (w, h) = (r.width(), r.height());
    let along = |at: i32, start: i32, len: i32, size: i32, to_start: i32, to_len: i32| {
        let room = (len - size).max(1);
        to_start + ((at - start).clamp(0, room) as i64 * (to_len - size).max(0) as i64 / room as i64) as i32
    };
    let left = along(r.left, from.left, from.width(), w, to.left, to.width());
    let top = along(r.top, from.top, from.height(), h, to.top, to.height());
    Rect { left, top, right: left + w, bottom: top + h }
}

/// The part of a thumbnail drawn at `at` that's inside `clip`, and the
/// part of its `source` that shows there; None if none of it is.
pub fn clipped(at: Rect, source: Rect, clip: Rect) -> Option<(Rect, Rect)> {
    let vis = Rect { left: at.left.max(clip.left), top: at.top.max(clip.top), right: at.right.min(clip.right), bottom: at.bottom.min(clip.bottom) };
    if vis.width() <= 0 || vis.height() <= 0 || at.width() <= 0 || at.height() <= 0 {
        return None;
    }
    if vis == at {
        return Some((at, source));
    }
    let scale = |d: i32, src: i32, dst: i32| (i64::from(d) * i64::from(src) / i64::from(dst)) as i32;
    let (sw, sh) = (source.width(), source.height());
    let src = Rect {
        left: source.left + scale(vis.left - at.left, sw, at.width()),
        top: source.top + scale(vis.top - at.top, sh, at.height()),
        right: source.right - scale(at.right - vis.right, sw, at.width()),
        bottom: source.bottom - scale(at.bottom - vis.bottom, sh, at.height()),
    };
    Some((vis, src))
}

/// `r` moved by how far the pointer went since `from`.
pub fn shifted(r: Rect, from: Point, to: Point) -> Rect {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    Rect { left: r.left + dx, top: r.top + dy, right: r.right + dx, bottom: r.bottom + dy }
}

/// A floating window `start` resized from `corner` by how far the pointer
/// went since `from`, never smaller than `min` either way.
pub fn corner_resized(start: Rect, corner: [Direction; 2], from: Point, to: Point, min: i32) -> Rect {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let mut r = start;
    match corner[0] {
        Direction::Left => r.left = (r.left + dx).min(r.right - min),
        _ => r.right = (r.right + dx).max(r.left + min),
    }
    match corner[1] {
        Direction::Up => r.top = (r.top + dy).min(r.bottom - min),
        _ => r.bottom = (r.bottom + dy).max(r.top + min),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    /// A 1920x1080 monitor, its left half the tile.
    const SCREEN: Rect = Rect { left: 0, top: 0, right: 1920, bottom: 1080 };

    #[test]
    fn letterboxing_centres_a_smaller_window_in_its_tile() {
        let tile = rect(0, 0, 960, 1080);
        assert_eq!(letterboxed(tile, Size { width: 800, height: 600 }, SCREEN), rect(80, 240, 880, 840));
        assert_eq!(letterboxed(tile, Size::of(tile), SCREEN), tile);
    }

    #[test]
    fn letterboxing_overflows_a_bigger_window_over_its_neighbour_only() {
        // League's client at its 1280x720 preset in the left half: centred
        // it would start at -160, off the screen; it goes over the right
        // half instead.
        let tile = rect(0, 0, 960, 1080);
        assert_eq!(letterboxed(tile, Size { width: 1280, height: 720 }, SCREEN), rect(0, 180, 1280, 900));
        // In the middle of three tiles there's room either side: centred.
        assert_eq!(letterboxed(rect(640, 0, 1280, 1080), Size { width: 800, height: 600 }, SCREEN), rect(560, 240, 1360, 840));
    }

    /// saturn 2026-11-26: League's game, 2576x1440 (a 2560x1440 client
    /// and the 8 px frame it sized itself for), in the right half of the
    /// 5120x1440 monitor. Centred it sat at 2552..5128, 8 px off the
    /// screen; now it ends at the screen's edge and its 16 px of overflow
    /// are all it covers of the left tile.
    #[test]
    fn leagues_game_stays_on_its_monitor() {
        let monitor = rect(0, 0, 5120, 1440);
        let tile = rect(2560, 0, 5120, 1440);
        let at = letterboxed(tile, Size { width: 2576, height: 1440 }, monitor);
        assert_eq!(at, rect(2544, 0, 5120, 1440));
        assert!(at.left >= monitor.left && at.right <= monitor.right && at.top >= monitor.top && at.bottom <= monitor.bottom);
        assert_eq!(tile.left - at.left, 16, "over the left tile by the overflow, no more");
        // Taking its tile's 2560x1440, as it's asked to first, it covers
        // nothing of its neighbour.
        assert_eq!(letterboxed(tile, Size::of(tile), monitor), tile);
    }

    #[test]
    fn letterboxing_a_window_bigger_than_its_monitor_starts_at_its_corner() {
        assert_eq!(letterboxed(rect(960, 0, 1920, 1080), Size { width: 2000, height: 1200 }, SCREEN), rect(0, 0, 2000, 1200));
        // A second monitor's coordinates, not the first's.
        let right = rect(1920, 0, 3200, 1024);
        assert_eq!(letterboxed(right, Size { width: 1400, height: 1100 }, right), rect(1920, 0, 3320, 1100));
    }

    #[test]
    fn letterboxing_rounds_an_odd_pixel_towards_the_top_left() {
        let tile = rect(10, 10, 111, 111);
        // 51 to spare: 25 before, 26 after.
        assert_eq!(letterboxed(tile, Size { width: 50, height: 50 }, SCREEN), rect(35, 35, 85, 85));
    }

    #[test]
    fn carrying_keeps_the_place_relative_to_the_output() {
        let from = rect(0, 0, 1000, 1000);
        let to = rect(1000, 0, 3000, 2000);
        // A quarter of the room in on each axis there is a quarter in here.
        assert_eq!(carried(rect(225, 225, 325, 325), from, to), rect(1475, 475, 1575, 575));
        assert_eq!(carried(rect(0, 0, 100, 100), from, to), rect(1000, 0, 1100, 100));
        assert_eq!(carried(rect(900, 900, 1000, 1000), from, to), rect(2900, 1900, 3000, 2000));
    }

    #[test]
    fn carrying_clamps_to_the_output_and_keeps_the_size() {
        let from = rect(0, 0, 1000, 1000);
        let to = rect(0, 0, 500, 500);
        // Partly off the left: as if at the edge.
        assert_eq!(carried(rect(-50, 0, 50, 100), from, to), rect(0, 0, 100, 100));
        // Bigger than the output it goes to: at its corner, not resized.
        assert_eq!(carried(rect(100, 100, 700, 700), from, to), rect(0, 0, 600, 600));
        // As big as the output it was on: no room to divide by.
        assert_eq!(carried(rect(0, 0, 1000, 1000), from, rect(0, 0, 2000, 2000)), rect(0, 0, 1000, 1000));
    }

    #[test]
    fn a_thumbnail_inside_its_clip_is_drawn_whole() {
        let src = rect(8, 0, 808, 600);
        assert_eq!(clipped(rect(0, 0, 400, 300), src, rect(0, 0, 1000, 1000)), Some((rect(0, 0, 400, 300), src)));
    }

    #[test]
    fn a_thumbnail_across_its_clip_shows_that_part_of_its_source() {
        // Drawn at half size, its right half outside the clip.
        let src = rect(0, 0, 800, 600);
        assert_eq!(clipped(rect(0, 0, 400, 300), src, rect(0, 0, 200, 1000)), Some((rect(0, 0, 200, 300), rect(0, 0, 400, 600))));
        // Its top third outside.
        assert_eq!(clipped(rect(0, 0, 400, 300), src, rect(0, 100, 1000, 1000)), Some((rect(0, 100, 400, 300), rect(0, 200, 800, 600))));
    }

    #[test]
    fn a_thumbnail_outside_its_clip_or_empty_is_not_drawn() {
        let src = rect(0, 0, 800, 600);
        assert_eq!(clipped(rect(0, 0, 400, 300), src, rect(400, 0, 1000, 1000)), None);
        assert_eq!(clipped(rect(0, 0, 0, 300), src, rect(0, 0, 1000, 1000)), None);
    }

    #[test]
    fn shifting_moves_by_the_pointers_travel() {
        assert_eq!(shifted(rect(10, 10, 110, 60), Point { x: 5, y: 5 }, Point { x: 25, y: -5 }), rect(30, 0, 130, 50));
    }

    #[test]
    fn resizing_moves_the_grabbed_corner_only() {
        let start = rect(100, 100, 500, 400);
        let (from, to) = (Point { x: 0, y: 0 }, Point { x: -20, y: 30 });
        assert_eq!(corner_resized(start, [Direction::Left, Direction::Up], from, to, 50), rect(80, 130, 500, 400));
        assert_eq!(corner_resized(start, [Direction::Right, Direction::Down], from, to, 50), rect(100, 100, 480, 430));
    }

    #[test]
    fn resizing_stops_at_the_minimum_size() {
        let start = rect(100, 100, 500, 400);
        let from = Point { x: 0, y: 0 };
        let far = Point { x: 1000, y: 1000 };
        assert_eq!(corner_resized(start, [Direction::Left, Direction::Up], from, far, 100), rect(400, 300, 500, 400));
        let back = Point { x: -1000, y: -1000 };
        assert_eq!(corner_resized(start, [Direction::Right, Direction::Down], from, back, 100), rect(100, 100, 200, 200));
    }
}
