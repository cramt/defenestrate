//! COSMIC's tiling, as a binary tree: every window is a leaf, every split
//! divides its rect between two children along one axis. A new window
//! splits the focused one along its longer side; moving and resizing walk
//! up the tree to the nearest split on the right axis.
//!
//! Nodes live in an arena with parent links, so walking up is cheap and
//! nothing fights the borrow checker. Pure, so it's tested here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(self) -> i32 {
        self.right - self.left
    }
    pub fn height(self) -> i32 {
        self.bottom - self.top
    }
    pub fn inset(self, by: i32) -> Rect {
        Rect { left: self.left + by, top: self.top + by, right: self.right - by, bottom: self.bottom - by }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }
    /// Which child of a split on this axis lies that way: 0 is left/top.
    fn side(self) -> usize {
        match self {
            Direction::Left | Direction::Up => 0,
            Direction::Right | Direction::Down => 1,
        }
    }
}

/// How a split lays out its two children: side by side, or one above the
/// other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn flip(self) -> Axis {
        match self {
            Axis::Horizontal => Axis::Vertical,
            Axis::Vertical => Axis::Horizontal,
        }
    }
    /// The way a new window splits `r`: across its longer side, so both
    /// halves stay as square as they can.
    fn for_rect(r: Rect) -> Axis {
        if r.width() >= r.height() { Axis::Horizontal } else { Axis::Vertical }
    }
}

/// The first child's share of a split, in percent. Only buildable through
/// its methods, so no window gets squeezed out of sight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio(u8);

impl Ratio {
    const MIN: u8 = 10;
    const MAX: u8 = 90;
    const STEP: u8 = 5;
    pub const HALF: Ratio = Ratio(50);

    fn grow(self) -> Ratio {
        Ratio((self.0 + Self::STEP).min(Self::MAX))
    }
    fn shrink(self) -> Ratio {
        Ratio(self.0.saturating_sub(Self::STEP).max(Self::MIN))
    }
}

type Id = usize;

#[derive(Clone, Copy)]
enum Kind<W> {
    Leaf(W),
    Split { axis: Axis, ratio: Ratio, kids: [Id; 2] },
}

#[derive(Clone, Copy)]
struct Node<W> {
    parent: Option<Id>,
    kind: Kind<W>,
}

pub struct Tree<W> {
    /// Freed slots are None and get reused.
    nodes: Vec<Option<Node<W>>>,
    root: Option<Id>,
}

impl<W: Copy + Eq> Default for Tree<W> {
    fn default() -> Self {
        Tree { nodes: Vec::new(), root: None }
    }
}

impl<W: Copy + Eq> Tree<W> {
    fn node(&self, id: Id) -> &Node<W> {
        self.nodes[id].as_ref().expect("live node")
    }

    fn node_mut(&mut self, id: Id) -> &mut Node<W> {
        self.nodes[id].as_mut().expect("live node")
    }

    fn alloc(&mut self, node: Node<W>) -> Id {
        match self.nodes.iter().position(Option::is_none) {
            Some(id) => {
                self.nodes[id] = Some(node);
                id
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        }
    }

    fn leaf(&self, w: W) -> Option<Id> {
        self.nodes.iter().position(|n| matches!(n, Some(Node { kind: Kind::Leaf(x), .. }) if *x == w))
    }

    fn kids(&self, id: Id) -> Option<[Id; 2]> {
        match self.node(id).kind {
            Kind::Split { kids, .. } => Some(kids),
            Kind::Leaf(_) => None,
        }
    }

    /// Which of its parent's children `id` is.
    fn side_in(&self, parent: Id, id: Id) -> usize {
        usize::from(self.kids(parent).expect("parent is a split")[1] == id)
    }

    /// Puts `new` where `old` was, under `old`'s parent (or as the root).
    fn replace(&mut self, old: Id, new: Id) {
        let parent = self.node(old).parent;
        self.node_mut(new).parent = parent;
        match parent {
            None => self.root = Some(new),
            Some(p) => {
                let side = self.side_in(p, old);
                if let Kind::Split { kids, .. } = &mut self.node_mut(p).kind {
                    kids[side] = new;
                }
            }
        }
    }

    /// Splits `at` along `axis` with `leaf` on `side`.
    fn split_at(&mut self, at: Id, leaf: Id, axis: Axis, side: usize) {
        let kids = if side == 0 { [leaf, at] } else { [at, leaf] };
        let split = self.alloc(Node { parent: None, kind: Kind::Split { axis, ratio: Ratio::HALF, kids } });
        self.replace(at, split);
        self.node_mut(at).parent = Some(split);
        self.node_mut(leaf).parent = Some(split);
    }

    /// Takes leaf `id` out of the tree without freeing it; its sibling
    /// takes the parent split's place.
    fn detach(&mut self, id: Id) {
        let Some(parent) = self.node(id).parent else {
            self.root = None;
            return;
        };
        let sibling = self.kids(parent).expect("split")[1 - self.side_in(parent, id)];
        self.replace(parent, sibling);
        self.nodes[parent] = None;
        self.node_mut(id).parent = None;
    }

    pub fn contains(&self, w: W) -> bool {
        self.leaf(w).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// In layout order: left to right, top to bottom.
    pub fn windows(&self) -> Vec<W> {
        let mut out = Vec::new();
        let mut stack: Vec<Id> = self.root.into_iter().collect();
        while let Some(id) = stack.pop() {
            match self.node(id).kind {
                Kind::Leaf(w) => out.push(w),
                Kind::Split { kids, .. } => stack.extend([kids[1], kids[0]]),
            }
        }
        out
    }

    /// Adds `w` by splitting `next_to` (the root if it's None or absent)
    /// across its longer side, with `w` taking the right or bottom half.
    pub fn insert(&mut self, w: W, next_to: Option<W>, area: Rect, gap: i32) {
        if self.contains(w) {
            return;
        }
        let leaf = self.alloc(Node { parent: None, kind: Kind::Leaf(w) });
        let Some(root) = self.root else {
            self.root = Some(leaf);
            return;
        };
        let target = next_to.and_then(|t| self.leaf(t)).unwrap_or(root);
        let rect = self.rect_of(target, area, gap);
        self.split_at(target, leaf, Axis::for_rect(rect), 1);
    }

    /// Removes `w` and returns the window that should get focus: one from
    /// the subtree that took its place.
    pub fn remove(&mut self, w: W) -> Option<W> {
        let id = self.leaf(w)?;
        let parent = self.node(id).parent;
        let heir = parent.map(|p| self.kids(p).expect("split")[1 - self.side_in(p, id)]);
        self.detach(id);
        self.nodes[id] = None;
        heir.and_then(|h| self.first_window(h))
    }

    fn first_window(&self, mut id: Id) -> Option<W> {
        loop {
            match self.node(id).kind {
                Kind::Leaf(w) => return Some(w),
                Kind::Split { kids, .. } => id = kids[0],
            }
        }
    }

    pub fn layout(&self, area: Rect, gap: i32) -> Vec<(W, Rect)> {
        let mut out = Vec::new();
        let mut stack: Vec<(Id, Rect)> = self.root.map(|r| (r, area.inset(gap))).into_iter().collect();
        while let Some((id, rect)) = stack.pop() {
            match self.node(id).kind {
                Kind::Leaf(w) => out.push((w, rect)),
                Kind::Split { axis, ratio, kids } => {
                    let [a, b] = divide(rect, axis, ratio, gap);
                    stack.extend([(kids[1], b), (kids[0], a)]);
                }
            }
        }
        out
    }

    /// Where node `id` is laid out: its ancestors' splits, top down.
    fn rect_of(&self, id: Id, area: Rect, gap: i32) -> Rect {
        let mut path = vec![id];
        while let Some(p) = self.node(*path.last().unwrap()).parent {
            path.push(p);
        }
        let mut rect = area.inset(gap);
        for pair in path.windows(2).rev() {
            let (child, parent) = (pair[0], pair[1]);
            if let Kind::Split { axis, ratio, .. } = self.node(parent).kind {
                rect = divide(rect, axis, ratio, gap)[self.side_in(parent, child)];
            }
        }
        rect
    }

    /// COSMIC's Move: with a neighbour that way under the same split, swap
    /// with it; otherwise climb to the nearest split on that axis and land
    /// beside the subtree there; at the edge of everything, become a new
    /// half of the whole screen on that side.
    pub fn r#move(&mut self, w: W, dir: Direction) {
        let Some(leaf) = self.leaf(w) else { return };
        let (axis, toward) = (dir.axis(), dir.side());
        let mut child = leaf;
        while let Some(split) = self.node(child).parent {
            let Kind::Split { axis: split_axis, kids, .. } = self.node(split).kind else { unreachable!() };
            if split_axis == axis && self.side_in(split, child) != toward {
                if child == leaf {
                    if let Kind::Split { kids, .. } = &mut self.node_mut(split).kind {
                        kids.swap(0, 1);
                    }
                } else {
                    let beside = kids[toward];
                    self.detach(leaf);
                    self.split_at(beside, leaf, axis, 1 - toward);
                }
                return;
            }
            child = split;
        }
        if self.root == Some(leaf) {
            return;
        }
        self.detach(leaf);
        let root = self.root.expect("other windows remain");
        self.split_at(root, leaf, axis, toward);
    }

    /// COSMIC's ToggleOrientation: flips the split `w` is directly in.
    pub fn toggle_orientation(&mut self, w: W) {
        let Some(parent) = self.leaf(w).and_then(|l| self.node(l).parent) else { return };
        if let Kind::Split { axis, .. } = &mut self.node_mut(parent).kind {
            *axis = axis.flip();
        }
    }

    /// COSMIC's Resizing: moves `w`'s edge on the `dir` side, outwards
    /// (growing it) or inwards. The edge belongs to the nearest split on
    /// that axis with `w` on the near side of it.
    pub fn resize(&mut self, w: W, dir: Direction, outwards: bool) {
        let Some(mut child) = self.leaf(w) else { return };
        while let Some(split) = self.node(child).parent {
            let side = self.side_in(split, child);
            if let Kind::Split { axis, ratio, .. } = &mut self.node_mut(split).kind
                && *axis == dir.axis()
                && side != dir.side()
            {
                // The first child grows when the edge moves right/down.
                let first_grows = outwards == (side == 0);
                *ratio = if first_grows { ratio.grow() } else { ratio.shrink() };
                return;
            }
            child = split;
        }
    }
}

fn divide(r: Rect, axis: Axis, ratio: Ratio, gap: i32) -> [Rect; 2] {
    match axis {
        Axis::Horizontal => {
            let split = r.left + (r.width() - gap) * i32::from(ratio.0) / 100;
            [Rect { right: split, ..r }, Rect { left: split + gap, ..r }]
        }
        Axis::Vertical => {
            let split = r.top + (r.height() - gap) * i32::from(ratio.0) / 100;
            [Rect { bottom: split, ..r }, Rect { top: split + gap, ..r }]
        }
    }
}

/// The nearest of `others` from `from` going `dir`, as COSMIC's Focus
/// picks it: entirely on that side, overlapping on the other axis if any
/// does, then closest.
pub fn neighbour<W: Copy>(from: Rect, dir: Direction, others: &[(W, Rect)]) -> Option<W> {
    let ahead = |r: &Rect| match dir {
        Direction::Left => r.right <= from.left,
        Direction::Right => r.left >= from.right,
        Direction::Up => r.bottom <= from.top,
        Direction::Down => r.top >= from.bottom,
    };
    let overlaps = |r: &Rect| match dir.axis() {
        Axis::Horizontal => r.top < from.bottom && from.top < r.bottom,
        Axis::Vertical => r.left < from.right && from.left < r.right,
    };
    let distance = |r: &Rect| {
        let along = match dir {
            Direction::Left => from.left - r.right,
            Direction::Right => r.left - from.right,
            Direction::Up => from.top - r.bottom,
            Direction::Down => r.top - from.bottom,
        };
        let across = match dir.axis() {
            Axis::Horizontal => (r.top + r.bottom - from.top - from.bottom).abs(),
            Axis::Vertical => (r.left + r.right - from.left - from.right).abs(),
        };
        (!overlaps(r), along, across)
    };
    others.iter().filter(|(_, r)| ahead(r)).min_by_key(|(_, r)| distance(r)).map(|&(w, _)| w)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 16:10, wider than tall, like a real screen.
    const AREA: Rect = Rect { left: 0, top: 0, right: 1600, bottom: 1000 };

    fn tree(n: u32) -> Tree<u32> {
        let mut t = Tree::default();
        for w in 1..=n {
            t.insert(w, Some(w - 1), AREA, 0);
        }
        t
    }

    fn rect(t: &Tree<u32>, w: u32) -> Rect {
        t.layout(AREA, 0).into_iter().find(|&(x, _)| x == w).unwrap().1
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    fn assert_tiles(t: &Tree<u32>) {
        let l = t.layout(AREA, 0);
        let area: i64 = l.iter().map(|(_, r)| i64::from(r.width()) * i64::from(r.height())).sum();
        assert_eq!(area, 1600 * 1000, "tiles don't cover the screen: {:?}", l.iter().map(|x| x.1).collect::<Vec<_>>());
        for (i, a) in l.iter().enumerate() {
            for b in &l[i + 1..] {
                assert!(!overlaps(a.1, b.1), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn new_windows_split_the_focused_one_along_its_longer_side() {
        let t = tree(3);
        // 1 | 2, then 2 (800x1000, taller than wide) splits top/bottom.
        assert_eq!(rect(&t, 1), Rect { left: 0, top: 0, right: 800, bottom: 1000 });
        assert_eq!(rect(&t, 2), Rect { left: 800, top: 0, right: 1600, bottom: 500 });
        assert_eq!(rect(&t, 3), Rect { left: 800, top: 500, right: 1600, bottom: 1000 });
        assert_eq!(t.windows(), [1, 2, 3]);
        assert_tiles(&t);
    }

    #[test]
    fn gaps_separate_tiles_and_edges() {
        let t = tree(2);
        let l = t.layout(AREA, 10);
        assert_eq!(l[0].1, Rect { left: 10, top: 10, right: 795, bottom: 990 });
        assert_eq!(l[1].1, Rect { left: 805, top: 10, right: 1590, bottom: 990 });
    }

    #[test]
    fn removal_hands_the_space_and_focus_to_the_sibling() {
        let mut t = tree(3);
        assert_eq!(t.remove(3), Some(2));
        assert_eq!(rect(&t, 2), Rect { left: 800, top: 0, right: 1600, bottom: 1000 });
        assert_eq!(t.remove(1), Some(2));
        assert_eq!(rect(&t, 2), AREA);
        assert_eq!(t.remove(2), None);
        assert!(t.is_empty());
        assert_eq!(t.remove(2), None);
    }

    #[test]
    fn slots_are_reused() {
        let mut t = tree(3);
        for _ in 0..20 {
            t.remove(3);
            t.insert(3, Some(2), AREA, 0);
        }
        assert!(t.nodes.len() <= 5);
        assert_tiles(&t);
    }

    #[test]
    fn move_swaps_with_a_sibling() {
        let mut t = tree(2);
        t.r#move(1, Direction::Right);
        assert_eq!(t.windows(), [2, 1]);
        // Already at the right edge of everything: nothing to do.
        t.r#move(1, Direction::Right);
        assert_eq!(t.windows(), [2, 1]);
    }

    #[test]
    fn move_climbs_out_of_a_group() {
        // 1 | (2 / 3). Moving 3 left leaves the right group and lands
        // beside 1, on its right: 1 | 3 | 2, roughly.
        let mut t = tree(3);
        t.r#move(3, Direction::Left);
        assert_tiles(&t);
        let (r1, r3, r2) = (rect(&t, 1), rect(&t, 3), rect(&t, 2));
        assert!(r1.right <= r3.left && r3.right <= r2.left, "{r1:?} {r3:?} {r2:?}");
    }

    #[test]
    fn move_past_the_edge_takes_a_whole_side() {
        // 1 | (2 / 3). Moving 2 up: no vertical split above it with room,
        // so it becomes the top half of everything.
        let mut t = tree(3);
        t.r#move(3, Direction::Up);
        assert_eq!(t.windows(), [1, 3, 2], "a sibling swap first");
        t.r#move(3, Direction::Up);
        assert_tiles(&t);
        assert_eq!(rect(&t, 3), Rect { left: 0, top: 0, right: 1600, bottom: 500 });
    }

    #[test]
    fn orientation_flips_the_direct_split() {
        let mut t = tree(2);
        t.toggle_orientation(2);
        assert_eq!(rect(&t, 1), Rect { left: 0, top: 0, right: 1600, bottom: 500 });
        assert_tiles(&t);
    }

    #[test]
    fn resize_moves_the_right_edge_and_clamps() {
        let mut t = tree(2);
        t.resize(1, Direction::Right, true);
        assert_eq!(rect(&t, 1).right, 880);
        t.resize(2, Direction::Left, true);
        assert_eq!(rect(&t, 1).right, 800);
        t.resize(1, Direction::Right, false);
        assert_eq!(rect(&t, 1).right, 720);
        // 1's left edge is the screen's; nothing to move.
        t.resize(1, Direction::Left, true);
        assert_eq!(rect(&t, 1).left, 0);
        for _ in 0..50 {
            t.resize(1, Direction::Right, true);
        }
        assert_eq!(rect(&t, 1).right, 1440);
        assert_tiles(&t);
    }

    #[test]
    fn focus_goes_to_the_overlapping_neighbour() {
        let t = tree(3);
        let l = t.layout(AREA, 0);
        let from = |w| l.iter().find(|x| x.0 == w).unwrap().1;
        assert_eq!(neighbour(from(1), Direction::Right, &l), Some(2));
        assert_eq!(neighbour(from(3), Direction::Left, &l), Some(1));
        assert_eq!(neighbour(from(2), Direction::Down, &l), Some(3));
        assert_eq!(neighbour(from(1), Direction::Left, &l), None);
    }
}
