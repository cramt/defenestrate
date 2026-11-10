//! COSMIC's tiling tree (cosmic-comp's src/shell/layout/tiling): every
//! window is a leaf, every group lays any number of children out along one
//! axis, each with its share of the group's length. A new window splits the
//! focused one in two; moving into a group, or dropping a dragged window on
//! a group's edge or between two of its children, adds a child and gives it
//! an equal share. A group left with one child is replaced by that child.
//!
//! Nodes live in an arena with parent links, so walking up is cheap and
//! nothing fights the borrow checker. Pure, so it's tested here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

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
    pub fn contains(self, p: Point) -> bool {
        (self.left..self.right).contains(&p.x) && (self.top..self.bottom).contains(&p.y)
    }
    fn centre(self) -> Point {
        Point { x: (self.left + self.right) / 2, y: (self.top + self.bottom) / 2 }
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
    pub fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }
    /// Towards the end of a group's children: right or down.
    fn forward(self) -> bool {
        matches!(self, Direction::Right | Direction::Down)
    }
}

/// How a group lays out its children: side by side, or one above the
/// other. (COSMIC names groups by the line between them, so its Vertical
/// is this Horizontal.)
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
    /// halves stay as square as they can. Square splits top/bottom, as in
    /// COSMIC.
    fn for_rect(r: Rect) -> Axis {
        if r.width() > r.height() { Axis::Horizontal } else { Axis::Vertical }
    }
    pub fn coord(self, p: Point) -> i32 {
        match self {
            Axis::Horizontal => p.x,
            Axis::Vertical => p.y,
        }
    }
    fn span(self, r: Rect) -> (i32, i32) {
        match self {
            Axis::Horizontal => (r.left, r.right),
            Axis::Vertical => (r.top, r.bottom),
        }
    }
    fn with_span(self, r: Rect, from: i32, to: i32) -> Rect {
        match self {
            Axis::Horizontal => Rect { left: from, right: to, ..r },
            Axis::Vertical => Rect { top: from, bottom: to, ..r },
        }
    }
}

/// Keyboard resizing moves an edge by this much of its group's length, and
/// leaves every child at least `MIN_SHARE` of it, so none gets squeezed
/// out of sight.
const STEP: f64 = 0.05;
const MIN_SHARE: f64 = 0.10;

type Id = usize;

#[derive(Clone, Copy, Debug)]
struct Child {
    id: Id,
    /// Of the group's length; a group's shares add up to 1.
    share: f64,
}

#[derive(Clone)]
enum Kind<W> {
    Leaf(W),
    Group { axis: Axis, kids: Vec<Child> },
}

#[derive(Clone)]
struct Node<W> {
    parent: Option<Id>,
    kind: Kind<W>,
}

/// Where a dragged window lands, as COSMIC's TargetZone without stacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    /// Splits that window, taking the half on that side.
    Split(Id, Direction),
    /// A new child of the whole layout on that side, or a new half of the
    /// screen if the layout runs the other way.
    Edge(Direction),
    /// A new child of `group`, between those two.
    Between { group: Id, before: Id, after: Id },
}

#[derive(Clone)]
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

    fn kids(&self, id: Id) -> &[Child] {
        match &self.node(id).kind {
            Kind::Group { kids, .. } => kids,
            Kind::Leaf(_) => &[],
        }
    }

    fn kids_mut(&mut self, id: Id) -> &mut Vec<Child> {
        match &mut self.node_mut(id).kind {
            Kind::Group { kids, .. } => kids,
            Kind::Leaf(_) => panic!("a leaf has no children"),
        }
    }

    fn axis_of(&self, id: Id) -> Option<Axis> {
        match self.node(id).kind {
            Kind::Group { axis, .. } => Some(axis),
            Kind::Leaf(_) => None,
        }
    }

    fn index_in(&self, group: Id, id: Id) -> usize {
        self.kids(group).iter().position(|c| c.id == id).expect("child of its parent")
    }

    /// Takes `id` out of its group without freeing it; the rest keep their
    /// proportions (COSMIC's remove_window). The group may be left with one
    /// child until `flatten`.
    fn detach(&mut self, id: Id) {
        let Some(parent) = self.node(id).parent else {
            if self.root == Some(id) {
                self.root = None;
            }
            return;
        };
        let kids = self.kids_mut(parent);
        let i = kids.iter().position(|c| c.id == id).expect("child of its parent");
        kids.remove(i);
        let rest: f64 = kids.iter().map(|c| c.share).sum();
        for c in kids.iter_mut() {
            c.share /= rest;
        }
        self.node_mut(id).parent = None;
    }

    /// Adds `id` to `group` at `at` with an equal share, the others
    /// shrinking in proportion (COSMIC's add_window).
    fn attach(&mut self, group: Id, at: usize, id: Id) {
        let kids = self.kids_mut(group);
        let n = kids.len() as f64;
        for c in kids.iter_mut() {
            c.share *= n / (n + 1.0);
        }
        kids.insert(at.min(kids.len()), Child { id, share: 1.0 / (n + 1.0) });
        self.node_mut(id).parent = Some(group);
    }

    /// Puts `new` where `old` was, with its share, under `old`'s parent
    /// (or as the root).
    fn replace(&mut self, old: Id, new: Id) {
        let parent = self.node(old).parent;
        self.node_mut(new).parent = parent;
        match parent {
            None => self.root = Some(new),
            Some(p) => {
                let i = self.index_in(p, old);
                self.kids_mut(p)[i].id = new;
            }
        }
    }

    /// A new group along `axis` takes `old`'s place, with `old` and the
    /// detached `new` sharing it half and half (COSMIC's new_group).
    fn wrap(&mut self, old: Id, new: Id, axis: Axis, new_first: bool) {
        let group = self.alloc(Node { parent: None, kind: Kind::Group { axis, kids: Vec::new() } });
        self.replace(old, group);
        let pair = if new_first { [new, old] } else { [old, new] };
        *self.kids_mut(group) = pair.iter().map(|&id| Child { id, share: 0.5 }).collect();
        for id in pair {
            self.node_mut(id).parent = Some(group);
        }
    }

    /// Replaces every one-child group with its child, as COSMIC does on
    /// every relayout.
    fn flatten(&mut self) {
        let single = |n: &Option<Node<W>>| matches!(n, Some(Node { kind: Kind::Group { kids, .. }, .. }) if kids.len() == 1);
        while let Some(group) = self.nodes.iter().position(single) {
            let only = self.kids(group)[0].id;
            self.replace(group, only);
            self.nodes[group] = None;
        }
    }

    fn preorder(&self) -> Vec<Id> {
        let mut out = Vec::new();
        let mut stack: Vec<Id> = self.root.into_iter().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.kids(id).iter().rev().map(|c| c.id));
        }
        out
    }

    pub fn contains(&self, w: W) -> bool {
        self.leaf(w).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// In layout order: left to right, top to bottom.
    pub fn windows(&self) -> Vec<W> {
        self.preorder()
            .into_iter()
            .filter_map(|id| match self.node(id).kind {
                Kind::Leaf(w) => Some(w),
                Kind::Group { .. } => None,
            })
            .collect()
    }

    /// Adds `w` by splitting `next_to` across its longer side, with `w`
    /// taking the right or bottom half. Without `next_to`, it splits the
    /// whole layout the screen's long way (COSMIC's map_to_tree).
    pub fn insert(&mut self, w: W, next_to: Option<W>, area: Rect) {
        if self.contains(w) {
            return;
        }
        let leaf = self.alloc(Node { parent: None, kind: Kind::Leaf(w) });
        let Some(root) = self.root else {
            self.root = Some(leaf);
            return;
        };
        let (target, rect) = match next_to.and_then(|t| self.leaf(t)) {
            Some(t) => (t, self.rects(area)[t].expect("laid out")),
            None => (root, area),
        };
        self.wrap(target, leaf, Axis::for_rect(rect), false);
    }

    /// Removes `w` and returns the window that should get focus: one from
    /// the neighbour before it, or after it if it was first.
    pub fn remove(&mut self, w: W) -> Option<W> {
        let id = self.leaf(w)?;
        let heir = self.node(id).parent.map(|p| {
            let i = self.index_in(p, id);
            self.kids(p)[if i > 0 { i - 1 } else { 1 }].id
        });
        self.detach(id);
        self.nodes[id] = None;
        self.flatten();
        heir.map(|h| self.first_window(h))
    }

    fn first_window(&self, mut id: Id) -> W {
        loop {
            match &self.node(id).kind {
                Kind::Leaf(w) => return *w,
                Kind::Group { kids, .. } => id = kids[0].id,
            }
        }
    }

    /// Every live node's rect, by id.
    fn rects(&self, area: Rect) -> Vec<Option<Rect>> {
        let mut out = vec![None; self.nodes.len()];
        let mut stack: Vec<(Id, Rect)> = self.root.map(|r| (r, area)).into_iter().collect();
        while let Some((id, rect)) = stack.pop() {
            out[id] = Some(rect);
            if let Kind::Group { axis, kids } = &self.node(id).kind {
                stack.extend(kids.iter().map(|c| c.id).zip(divide(rect, *axis, kids)));
            }
        }
        out
    }

    pub fn layout(&self, area: Rect) -> Vec<(W, Rect)> {
        let rects = self.rects(area);
        self.preorder()
            .into_iter()
            .filter_map(|id| match self.node(id).kind {
                Kind::Leaf(w) => Some((w, rects[id].expect("laid out"))),
                Kind::Group { .. } => None,
            })
            .collect()
    }

    /// COSMIC's move_current_node, without stacks. Climbing from `w`: a
    /// group running the other way gets wrapped, with `w` beside it on that
    /// side; a group we climbed into gets `w` as a new child next to where
    /// we came from; in `w`'s own group it swaps with a lone sibling, moves
    /// into a neighbouring group, or forks with the neighbour. At the edge
    /// of everything there is no other monitor to go to, so nothing happens.
    pub fn r#move(&mut self, w: W, dir: Direction) {
        let Some(node) = self.leaf(w) else { return };
        let (axis, forward) = (dir.axis(), dir.forward());
        let mut child = node;
        while let Some(group) = self.node(child).parent {
            if self.axis_of(group) != Some(axis) {
                self.detach(node);
                self.wrap(group, node, axis, !forward);
                break;
            }
            let i = self.index_in(group, child);
            if child != node {
                self.detach(node);
                self.attach(group, if forward { i + 1 } else { i }, node);
                break;
            }
            let len = self.kids(group).len();
            let next = if forward { (i + 1 < len).then_some(i + 1) } else { i.checked_sub(1) };
            if let Some(next) = next {
                let neighbour = self.kids(group)[next].id;
                match self.axis_of(neighbour) {
                    Some(inner) if len == 2 => {
                        self.detach(node);
                        let inner_len = self.kids(neighbour).len();
                        if inner == axis {
                            self.attach(neighbour, if forward { 0 } else { inner_len }, node);
                        } else if inner_len.is_multiple_of(2) {
                            self.attach(neighbour, inner_len / 2, node);
                        } else {
                            // No middle slot: fork with the middle child.
                            let middle = self.kids(neighbour)[inner_len / 2].id;
                            self.wrap(middle, node, inner.flip(), forward);
                        }
                    }
                    _ if len == 2 => self.kids_mut(group).swap(i, next),
                    _ => {
                        self.detach(node);
                        self.wrap(neighbour, node, axis, forward);
                    }
                }
                break;
            }
            child = group;
        }
        self.flatten();
    }

    /// COSMIC's next_focus: climb to the nearest group running `dir`'s way
    /// with a child on that side, then go down into it: to its near end
    /// through groups running the same way, and to the child nearest the
    /// edge we're leaving through the others.
    pub fn focus(&self, w: W, dir: Direction, area: Rect) -> Option<W> {
        let rects = self.rects(area);
        let start = self.leaf(w)?;
        let from = rects[start]?;
        let mut child = start;
        while let Some(group) = self.node(child).parent {
            let main = self.axis_of(group);
            let kids = self.kids(group);
            let i = self.index_in(group, child);
            let next = if dir.forward() { kids.get(i + 1) } else { i.checked_sub(1).map(|j| &kids[j]) };
            if let (Some(next), true) = (next, main == Some(dir.axis())) {
                let mut id = next.id;
                loop {
                    match &self.node(id).kind {
                        Kind::Leaf(w) => return Some(*w),
                        Kind::Group { axis, kids } if Some(*axis) == main => {
                            id = if dir.forward() { kids[0].id } else { kids[kids.len() - 1].id };
                        }
                        Kind::Group { kids, .. } => {
                            let to = rects.as_slice();
                            id = kids.iter().min_by_key(|c| facing_distance(from, to[c.id].expect("laid out"), dir)).expect("non-empty").id;
                        }
                    }
                }
            }
            child = group;
        }
        None
    }

    /// COSMIC's ToggleOrientation: flips the group `w` is directly in.
    pub fn toggle_orientation(&mut self, w: W) {
        let Some(parent) = self.leaf(w).and_then(|l| self.node(l).parent) else { return };
        if let Kind::Group { axis, .. } = &mut self.node_mut(parent).kind {
            *axis = axis.flip();
        }
    }

    /// COSMIC's Resizing: moves `w`'s edge on the `dir` side, outwards
    /// (growing it) or inwards, trading length with the neighbour across
    /// it. The edge belongs to the nearest group on that axis with a child
    /// on that side.
    pub fn resize(&mut self, w: W, dir: Direction, outwards: bool) {
        let Some((group, i)) = self.fork(w, &[dir]) else { return };
        let (shrink, grow) = if outwards == dir.forward() { (i + 1, i) } else { (i, i + 1) };
        let kids = self.kids_mut(group);
        let floor = MIN_SHARE.min(kids[shrink].share);
        let moved = (kids[shrink].share - floor).clamp(0.0, STEP);
        kids[shrink].share -= moved;
        kids[grow].share += moved;
    }

    /// The group and the index of the child before the edge that
    /// `w`'s `sides` touch, nearest first (COSMIC's resize_request).
    fn fork(&self, w: W, sides: &[Direction]) -> Option<(Id, usize)> {
        let mut child = self.leaf(w)?;
        while let Some(group) = self.node(child).parent {
            let i = self.index_in(group, child);
            let len = self.kids(group).len();
            for &dir in sides.iter().filter(|d| Some(d.axis()) == self.axis_of(group)) {
                match dir.forward() {
                    true if i + 1 < len => return Some((group, i)),
                    false if i > 0 => return Some((group, i - 1)),
                    _ => {}
                }
            }
            child = group;
        }
        None
    }

    /// Where the edge Super+right-drag on `w`'s `corner` grabs is: its
    /// axis and position.
    pub fn edge(&self, w: W, corner: [Direction; 2], area: Rect) -> Option<(Axis, i32)> {
        let (group, i) = self.fork(w, &corner)?;
        let axis = self.axis_of(group)?;
        let rects = self.rects(area);
        Some((axis, axis.span(rects[self.kids(group)[i].id]?).1))
    }

    /// Moves that edge to `at`, as far as both sides keep `MIN_SHARE`.
    pub fn move_edge(&mut self, w: W, corner: [Direction; 2], at: i32, area: Rect) {
        let Some((group, i)) = self.fork(w, &corner) else { return };
        let Some(axis) = self.axis_of(group) else { return };
        let rects = self.rects(area);
        let kids = self.kids(group);
        let (Some(a), Some(b), Some(whole)) = (rects[kids[i].id], rects[kids[i + 1].id], rects[group]) else { return };
        let ((from, _), (_, to)) = (axis.span(a), axis.span(b));
        let (start, end) = axis.span(whole);
        let length = f64::from(end - start);
        let pair = kids[i].share + kids[i + 1].share;
        let min = (MIN_SHARE * length).min(f64::from(to - from) / 2.0);
        let at = f64::from(at).clamp(f64::from(from) + min, f64::from(to) - min);
        let first = (at - f64::from(from)) / length;
        let kids = self.kids_mut(group);
        kids[i].share = first;
        kids[i + 1].share = pair - first;
    }

    /// COSMIC's drop target for a window dragged to `p`: the screen's edges
    /// (a `band` wide) add to the whole layout, the boundaries between a
    /// group's children (`band` wide too) add to that group, and anywhere
    /// else splits the window under `p` on the side nearest `p`. Over the
    /// dragged window itself, it goes back where it was.
    fn zone(&self, w: W, p: Point, area: Rect, band: i32) -> Option<Zone> {
        let own = self.leaf(w)?;
        let root = self.root?;
        if root == own || !area.contains(p) {
            return None;
        }
        if self.axis_of(root).is_some() {
            let edges = [
                (Direction::Left, p.x - area.left),
                (Direction::Right, area.right - 1 - p.x),
                (Direction::Up, p.y - area.top),
                (Direction::Down, area.bottom - 1 - p.y),
            ];
            if let Some(&(dir, _)) = edges.iter().find(|&&(_, d)| d < band) {
                return Some(Zone::Edge(dir));
            }
        }
        let rects = self.rects(area);
        let mut id = root;
        loop {
            match &self.node(id).kind {
                Kind::Leaf(_) => return (id != own).then_some(Zone::Split(id, nearest_side(rects[id]?, p))),
                Kind::Group { axis, kids } => {
                    for pair in kids.windows(2) {
                        let boundary = axis.span(rects[pair[1].id]?).0;
                        if (axis.coord(p) - boundary).abs() < band / 2 {
                            return Some(Zone::Between { group: id, before: pair[0].id, after: pair[1].id });
                        }
                    }
                    id = kids.iter().find(|c| rects[c.id].is_some_and(|r| r.contains(p)))?.id;
                }
            }
        }
    }

    fn drop_into(&mut self, own: Id, zone: Zone) {
        self.detach(own);
        match zone {
            Zone::Split(target, dir) => self.wrap(target, own, dir.axis(), !dir.forward()),
            Zone::Edge(dir) => {
                let root = self.root.expect("other windows remain");
                if self.axis_of(root) == Some(dir.axis()) {
                    let at = if dir.forward() { self.kids(root).len() } else { 0 };
                    self.attach(root, at, own);
                } else {
                    self.wrap(root, own, dir.axis(), !dir.forward());
                }
            }
            Zone::Between { group, before, after } => {
                let at = if after == own { self.index_in(group, before) + 1 } else { self.index_in(group, after) };
                self.attach(group, at, own);
            }
        }
        self.flatten();
    }

    /// Where `w` would end up if dropped at `p`; None if it would go back
    /// where it is.
    pub fn drop_preview(&self, w: W, p: Point, area: Rect, band: i32) -> Option<Rect> {
        let zone = self.zone(w, p, area, band)?;
        let mut after = self.clone();
        after.drop_into(self.leaf(w)?, zone);
        after.layout(area).into_iter().find(|&(x, _)| x == w).map(|(_, r)| r)
    }

    /// Drops dragged `w` at `p`, as `drop_preview` showed.
    pub fn drop(&mut self, w: W, p: Point, area: Rect, band: i32) {
        if let (Some(own), Some(zone)) = (self.leaf(w), self.zone(w, p, area, band)) {
            self.drop_into(own, zone);
        }
    }
}

/// Splits `r` along `axis` by the children's shares. Neighbours share their
/// boundary, so rounding never leaves a sliver or an overlap.
fn divide(r: Rect, axis: Axis, kids: &[Child]) -> Vec<Rect> {
    let (start, end) = axis.span(r);
    let length = f64::from(end - start);
    let mut sum = 0.0;
    let mut from = start;
    kids.iter()
        .enumerate()
        .map(|(i, c)| {
            sum += c.share;
            let to = if i + 1 == kids.len() { end } else { start + (length * sum).round() as i32 };
            let piece = axis.with_span(r, from, to);
            from = to;
            piece
        })
        .collect()
}

/// Squared distance from the middle of `from`'s edge on the `dir` side to
/// the middle of `to`'s facing edge.
fn facing_distance(from: Rect, to: Rect, dir: Direction) -> i64 {
    let (a, b) = match dir {
        Direction::Left => ((from.left, from.centre().y), (to.right, to.centre().y)),
        Direction::Right => ((from.right, from.centre().y), (to.left, to.centre().y)),
        Direction::Up => ((from.centre().x, from.top), (to.centre().x, to.bottom)),
        Direction::Down => ((from.centre().x, from.bottom), (to.centre().x, to.top)),
    };
    let (dx, dy) = (i64::from(a.0 - b.0), i64::from(a.1 - b.1));
    dx * dx + dy * dy
}

/// The side of `r` that `p` is nearest to, relative to `r`'s size.
fn nearest_side(r: Rect, p: Point) -> Direction {
    let fx = f64::from(p.x - r.left) / f64::from(r.width());
    let fy = f64::from(p.y - r.top) / f64::from(r.height());
    let (h, dh) = if fx < 0.5 { (Direction::Left, fx) } else { (Direction::Right, 1.0 - fx) };
    let (v, dv) = if fy < 0.5 { (Direction::Up, fy) } else { (Direction::Down, 1.0 - fy) };
    if dh < dv { h } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 16:10, wider than tall, like a real screen.
    const AREA: Rect = Rect { left: 0, top: 0, right: 1600, bottom: 1000 };
    const BAND: i32 = 32;

    fn r(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    fn p(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    /// Each window opened with the previous one focused.
    fn tree(n: u32) -> Tree<u32> {
        let mut t = Tree::default();
        for w in 1..=n {
            t.insert(w, Some(w - 1), AREA);
        }
        t
    }

    /// 1 | 2 | 3, a third each: 1 | (2 / 3) with 3 dropped on the right
    /// edge of the screen.
    fn thirds() -> Tree<u32> {
        let mut t = tree(3);
        t.drop(3, p(1595, 500), AREA, BAND);
        t
    }

    fn rect(t: &Tree<u32>, w: u32) -> Rect {
        t.layout(AREA).into_iter().find(|&(x, _)| x == w).unwrap().1
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    /// The tiles cover the screen exactly, and no group is left with fewer
    /// than two children.
    fn assert_tiles(t: &Tree<u32>) {
        let l = t.layout(AREA);
        let area: i64 = l.iter().map(|(_, r)| i64::from(r.width()) * i64::from(r.height())).sum();
        assert_eq!(area, 1600 * 1000, "tiles don't cover the screen: {:?}", l.iter().map(|x| x.1).collect::<Vec<_>>());
        for (i, a) in l.iter().enumerate() {
            for b in &l[i + 1..] {
                assert!(!overlaps(a.1, b.1), "{a:?} overlaps {b:?}");
            }
        }
        for id in t.preorder() {
            if let Kind::Group { kids, .. } = &t.node(id).kind {
                assert!(kids.len() >= 2, "a group with {} children", kids.len());
                let sum: f64 = kids.iter().map(|c| c.share).sum();
                assert!((sum - 1.0).abs() < 1e-9, "shares add up to {sum}");
            }
        }
    }

    #[test]
    fn new_windows_split_the_focused_one_along_its_longer_side() {
        let t = tree(3);
        // 1 | 2, then 2 (800x1000, taller than wide) splits top/bottom.
        assert_eq!(rect(&t, 1), r(0, 0, 800, 1000));
        assert_eq!(rect(&t, 2), r(800, 0, 1600, 500));
        assert_eq!(rect(&t, 3), r(800, 500, 1600, 1000));
        assert_eq!(t.windows(), [1, 2, 3]);
        assert_tiles(&t);
    }

    #[test]
    fn without_focus_a_new_window_halves_everything() {
        let mut t = tree(3);
        t.insert(4, None, AREA);
        assert_eq!(rect(&t, 4), r(800, 0, 1600, 1000));
        assert_eq!(rect(&t, 1), r(0, 0, 400, 1000));
        assert_tiles(&t);
    }

    #[test]
    fn a_group_holds_three_in_thirds() {
        let t = thirds();
        assert_eq!(t.windows(), [1, 2, 3]);
        assert_eq!(rect(&t, 1), r(0, 0, 533, 1000));
        assert_eq!(rect(&t, 2), r(533, 0, 1067, 1000));
        assert_eq!(rect(&t, 3), r(1067, 0, 1600, 1000));
        assert_eq!(t.preorder().len(), 4, "one group, not nested pairs");
        assert_tiles(&t);
    }

    #[test]
    fn removal_hands_the_space_and_focus_to_the_sibling() {
        let mut t = tree(3);
        assert_eq!(t.remove(3), Some(2));
        assert_eq!(rect(&t, 2), r(800, 0, 1600, 1000));
        assert_eq!(t.remove(1), Some(2));
        assert_eq!(rect(&t, 2), AREA);
        assert_eq!(t.remove(2), None);
        assert!(t.is_empty());
        assert_eq!(t.remove(2), None);
    }

    #[test]
    fn removal_from_a_group_rebalances_the_rest() {
        let mut t = thirds();
        assert_eq!(t.remove(2), Some(1));
        assert_eq!(rect(&t, 1), r(0, 0, 800, 1000));
        assert_eq!(rect(&t, 3), r(800, 0, 1600, 1000));
        assert_tiles(&t);
        // Uneven shares keep their proportions: 1 at 60%, 2 and 3 at 20%.
        let mut t = thirds();
        for _ in 0..6 {
            t.resize(1, Direction::Right, true);
        }
        t.resize(3, Direction::Left, true);
        let before = (rect(&t, 1).width(), rect(&t, 3).width());
        t.remove(2);
        let after = (rect(&t, 1).width(), rect(&t, 3).width());
        let ratio = |(a, b): (i32, i32)| f64::from(a) / f64::from(b);
        assert!((ratio(after) - ratio(before)).abs() < 0.01, "{before:?} became {after:?}");
        assert_tiles(&t);
    }

    #[test]
    fn slots_are_reused() {
        let mut t = tree(3);
        for _ in 0..20 {
            t.remove(3);
            t.insert(3, Some(2), AREA);
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
        assert_tiles(&t);
    }

    #[test]
    fn move_out_of_a_group_and_into_the_next_one_up_makes_thirds() {
        // 1 | (2 / 3). Moving 3 left wraps its group, 3 on the left...
        let mut t = tree(3);
        t.r#move(3, Direction::Left);
        assert_eq!(t.windows(), [1, 3, 2]);
        assert_eq!(rect(&t, 3), r(800, 0, 1200, 1000));
        // ...and again leaves that group to join the outer one: thirds.
        t.r#move(3, Direction::Left);
        assert_eq!(t.windows(), [1, 3, 2]);
        assert_eq!(rect(&t, 3), r(533, 0, 1067, 1000));
        assert_eq!(t.preorder().len(), 4);
        assert_tiles(&t);
    }

    #[test]
    fn move_past_the_edge_takes_a_whole_side() {
        // 1 | (2 / 3). Moving 3 up swaps it with 2, then no group above it
        // runs vertically, so it becomes the top half of everything.
        let mut t = tree(3);
        t.r#move(3, Direction::Up);
        assert_eq!(t.windows(), [1, 3, 2], "a sibling swap first");
        t.r#move(3, Direction::Up);
        assert_tiles(&t);
        assert_eq!(rect(&t, 3), r(0, 0, 1600, 500));
        assert_eq!(rect(&t, 1), r(0, 500, 800, 1000));
    }

    #[test]
    fn move_within_a_bigger_group_forks_with_the_neighbour() {
        // COSMIC only swaps within pairs: in 1 | 2 | 3, 3 moving left
        // shares 2's place first, and passes it on the next move.
        let mut t = thirds();
        t.r#move(3, Direction::Left);
        assert_eq!(t.windows(), [1, 2, 3]);
        assert_eq!(rect(&t, 1), r(0, 0, 800, 1000));
        assert_eq!(rect(&t, 3), r(1200, 0, 1600, 1000));
        t.r#move(3, Direction::Left);
        assert_eq!(t.windows(), [1, 3, 2]);
        assert_tiles(&t);
    }

    #[test]
    fn move_into_a_neighbouring_group() {
        // 1 | (2 / 3), 1 moving right joins the group in its middle: an
        // even group takes it as a third child.
        let mut t = tree(3);
        t.r#move(1, Direction::Right);
        assert_eq!(t.windows(), [2, 1, 3]);
        assert_eq!(rect(&t, 1), r(0, 333, 1600, 667));
        assert_tiles(&t);
    }

    #[test]
    fn orientation_flips_the_direct_group() {
        let mut t = tree(2);
        t.toggle_orientation(2);
        assert_eq!(rect(&t, 1), r(0, 0, 1600, 500));
        assert_tiles(&t);
        let mut t = thirds();
        t.toggle_orientation(2);
        assert_eq!(rect(&t, 2), r(0, 333, 1600, 667));
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
    fn resize_in_a_group_only_trades_with_that_neighbour() {
        let mut t = thirds();
        t.resize(2, Direction::Right, true);
        assert_eq!(rect(&t, 1), r(0, 0, 533, 1000));
        assert_eq!(rect(&t, 2), r(533, 0, 1147, 1000));
        assert_eq!(rect(&t, 3), r(1147, 0, 1600, 1000));
        assert_tiles(&t);
    }

    #[test]
    fn resize_climbs_to_the_group_on_that_axis() {
        // 1 | (2 / 3): 3's left edge belongs to the outer group.
        let mut t = tree(3);
        t.resize(3, Direction::Left, true);
        assert_eq!(rect(&t, 1).right, 720);
        assert_eq!(rect(&t, 2).left, 720);
        assert_tiles(&t);
    }

    #[test]
    fn dragging_an_edge_follows_the_pointer_and_clamps() {
        let mut t = tree(3);
        // 3's top-left corner: the nearest edge is 2/3's, then 1 | rest.
        let corner = [Direction::Left, Direction::Up];
        assert_eq!(t.edge(3, corner, AREA), Some((Axis::Vertical, 500)));
        t.move_edge(3, corner, 600, AREA);
        assert_eq!(rect(&t, 2).bottom, 600);
        t.move_edge(3, corner, 990, AREA);
        assert_eq!(rect(&t, 3).top, 900);
        // 1's right edge, grabbed from its bottom-right corner.
        let corner = [Direction::Right, Direction::Down];
        assert_eq!(t.edge(1, corner, AREA), Some((Axis::Horizontal, 800)));
        t.move_edge(1, corner, 1000, AREA);
        assert_eq!(rect(&t, 1).right, 1000);
        // Nothing to grab on the screen's own edge.
        assert_eq!(t.edge(1, [Direction::Left, Direction::Up], AREA), None);
        assert_tiles(&t);
    }

    #[test]
    fn focus_walks_the_tree() {
        let t = tree(3);
        assert_eq!(t.focus(1, Direction::Right, AREA), Some(2), "the nearer of 2 and 3, ties to the top");
        assert_eq!(t.focus(3, Direction::Left, AREA), Some(1));
        assert_eq!(t.focus(2, Direction::Down, AREA), Some(3));
        assert_eq!(t.focus(1, Direction::Left, AREA), None);
        assert_eq!(t.focus(2, Direction::Up, AREA), None);
        let t = thirds();
        assert_eq!(t.focus(1, Direction::Right, AREA), Some(2));
        assert_eq!(t.focus(2, Direction::Right, AREA), Some(3));
        assert_eq!(t.focus(3, Direction::Left, AREA), Some(2));
    }

    #[test]
    fn focus_into_a_group_picks_the_child_facing_us() {
        // 1 over (2 | 3).
        let mut t = tree(2);
        t.toggle_orientation(1);
        t.insert(3, Some(2), AREA);
        assert_eq!(rect(&t, 3), r(800, 500, 1600, 1000));
        assert_eq!(t.focus(1, Direction::Down, AREA), Some(2), "ties go to the first");
        assert_eq!(t.focus(3, Direction::Up, AREA), Some(1));
        assert_eq!(t.focus(3, Direction::Left, AREA), Some(2));
    }

    #[test]
    fn dropping_on_the_screen_edge_joins_the_whole_layout() {
        let t = thirds();
        let mut t2 = t.clone();
        // 1 to the right end: 2 | 3 | 1.
        t2.drop(1, p(1590, 300), AREA, BAND);
        assert_eq!(t2.windows(), [2, 3, 1]);
        assert_eq!(rect(&t2, 1), r(1067, 0, 1600, 1000));
        assert_tiles(&t2);
        // The top edge runs the other way: 2 becomes the top half.
        let mut t3 = t.clone();
        assert_eq!(t3.drop_preview(2, p(800, 5), AREA, BAND), Some(r(0, 0, 1600, 500)));
        t3.drop(2, p(800, 5), AREA, BAND);
        assert_eq!(rect(&t3, 2), r(0, 0, 1600, 500));
        assert_eq!(rect(&t3, 1), r(0, 500, 800, 1000));
        assert_tiles(&t3);
    }

    #[test]
    fn dropping_between_two_children_adds_a_third() {
        // (1 | (2 / 3)) | 4, dragging 4 onto the boundary between 2 and 3:
        // that group takes it as a third child, and what's left of the
        // outer group goes away.
        let mut t = tree(3);
        t.insert(4, None, AREA);
        assert_eq!(rect(&t, 2), r(400, 0, 800, 500));
        t.drop(4, p(600, 500), AREA, BAND);
        assert_eq!(t.windows(), [1, 2, 4, 3]);
        assert_eq!(rect(&t, 4), r(800, 333, 1600, 667));
        assert_tiles(&t);
        // Next to its own place it rejoins its group as a newcomer: an
        // equal share, the others keeping their proportions.
        let mut t = thirds();
        t.resize(2, Direction::Right, true);
        t.drop(2, p(533, 500), AREA, BAND);
        assert_eq!(t.windows(), [1, 2, 3]);
        assert_eq!(rect(&t, 2).width(), 533);
        assert_tiles(&t);
    }

    #[test]
    fn dropping_on_a_window_splits_it_on_the_nearer_side() {
        // 1 | (2 / 3), 1 dropped on 3's lower right: 2 / 3 take the screen
        // and 1 takes 3's right half.
        let mut t = tree(3);
        t.drop(1, p(1500, 750), AREA, BAND);
        assert_eq!(t.windows(), [2, 3, 1]);
        assert_eq!(rect(&t, 2), r(0, 0, 1600, 500));
        assert_eq!(rect(&t, 1), r(800, 500, 1600, 1000));
        assert_tiles(&t);
        // Nearer 2's top than its sides: 1 above 2.
        let mut t = tree(3);
        t.drop(1, p(1200, 60), AREA, BAND);
        assert_eq!(rect(&t, 1), r(0, 0, 1600, 250));
        assert_tiles(&t);
    }

    #[test]
    fn dropping_on_itself_or_with_nothing_else_tiled_changes_nothing() {
        let t = tree(3);
        assert_eq!(t.drop_preview(2, p(1200, 250), AREA, BAND), None);
        assert_eq!(tree(1).drop_preview(1, p(5, 5), AREA, BAND), None);
        assert_eq!(t.drop_preview(2, p(2000, 250), AREA, BAND), None, "off screen");
        let mut t2 = t.clone();
        t2.drop(2, p(1200, 250), AREA, BAND);
        assert_eq!(t2.layout(AREA), t.layout(AREA));
    }

    #[test]
    fn mixed_orientations_stay_consistent() {
        // Build something nested and shuffle it around; it must always tile.
        let mut t = tree(5);
        t.drop(5, p(5, 500), AREA, BAND);
        t.toggle_orientation(2);
        t.r#move(3, Direction::Down);
        t.drop(1, p(1590, 900), AREA, BAND);
        t.r#move(4, Direction::Left);
        t.resize(2, Direction::Down, true);
        assert_tiles(&t);
        let mut ws = t.windows();
        ws.sort();
        assert_eq!(ws, [1, 2, 3, 4, 5]);
        for w in 1..=5 {
            t.remove(w);
            if !t.is_empty() {
                assert_tiles(&t);
            }
        }
        assert!(t.is_empty());
    }
}
