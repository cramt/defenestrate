//! The window switcher's selection, as cosmic-launcher's alt-tab view
//! moves it: the list opens on its first entry (the focused window) and the
//! Tab that opened it takes the first step, so a quick Alt+Tab lands on the
//! window used before. Both ways wrap. Pure, so it's tested here.

use crate::wm::Step;

pub struct Cycle<T> {
    items: Vec<T>,
    at: usize,
}

impl<T> Cycle<T> {
    /// None for an empty list: COSMIC's switcher closes instead of showing
    /// nothing.
    pub fn open(items: Vec<T>, first: Step) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let mut c = Cycle { items, at: 0 };
        c.step(first);
        Some(c)
    }

    pub fn step(&mut self, step: Step) {
        let n = self.items.len();
        self.at = match step {
            Step::Next => (self.at + 1) % n,
            Step::Previous => (self.at + n - 1) % n,
        };
    }

    pub fn select(&mut self, i: usize) {
        if i < self.items.len() {
            self.at = i;
        }
    }

    pub fn at(&self) -> usize {
        self.at
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn into_selected(mut self) -> T {
        self.items.swap_remove(self.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected<T: Copy>(c: &Cycle<T>) -> T {
        c.items()[c.at()]
    }

    #[test]
    fn opens_on_the_window_used_before() {
        let c = Cycle::open(vec!['a', 'b', 'c'], Step::Next).unwrap();
        assert_eq!(selected(&c), 'b');
        let c = Cycle::open(vec!['a', 'b', 'c'], Step::Previous).unwrap();
        assert_eq!(selected(&c), 'c', "Shift+Tab starts from the least recent");
    }

    #[test]
    fn wraps_both_ways() {
        let mut c = Cycle::open(vec!['a', 'b', 'c'], Step::Next).unwrap();
        c.step(Step::Next);
        c.step(Step::Next);
        assert_eq!(selected(&c), 'a');
        c.step(Step::Previous);
        assert_eq!(c.into_selected(), 'c');
    }

    #[test]
    fn one_window_stays_selected_and_none_doesnt_open() {
        let c = Cycle::open(vec!['a'], Step::Next).unwrap();
        assert_eq!(selected(&c), 'a');
        assert!(Cycle::<char>::open(Vec::new(), Step::Next).is_none());
    }

    #[test]
    fn clicking_selects_within_the_list() {
        let mut c = Cycle::open(vec!['a', 'b'], Step::Next).unwrap();
        c.select(0);
        c.select(5);
        assert_eq!((c.at(), c.items().len()), (0, 2));
    }
}
