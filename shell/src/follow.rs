//! focus_follows_cursor, as cosmic-comp's pointer motion handling does it
//! (src/input/mod.rs, `PointerFocusState`): when the window under the
//! pointer changes to another one, focusing it is scheduled after
//! `focus_follows_cursor_delay`; going back to where the pointer started
//! cancels that, and going on to a third window reschedules it. Leaving
//! every window (the desktop, a dialog) changes nothing. Pure, so the
//! desktop layer only feeds it the window under the pointer and runs the
//! timer it asks for.

#[derive(Debug, PartialEq, Eq)]
pub enum Timer {
    /// Start the delay over; when it runs out, call `fire`.
    Restart,
    Cancel,
    Leave,
}

#[derive(Debug, Default)]
pub struct Follow<W> {
    /// The window the pointer was last over, if any.
    under: Option<W>,
    pending: Option<Pending<W>>,
}

#[derive(Debug)]
struct Pending<W> {
    /// Under the pointer when the first change was scheduled.
    original: Option<W>,
    scheduled: W,
}

impl<W: Copy + Eq> Follow<W> {
    pub fn new() -> Self {
        Follow { under: None, pending: None }
    }

    /// The pointer moved; `now` is the window under it.
    pub fn moved(&mut self, now: Option<W>) -> Timer {
        let old = std::mem::replace(&mut self.under, now);
        let Some(new) = now.filter(|_| old != now) else { return Timer::Leave };
        match &self.pending {
            None => {
                self.pending = Some(Pending { original: old, scheduled: new });
                Timer::Restart
            }
            Some(p) if p.original == Some(new) => {
                self.pending = None;
                Timer::Cancel
            }
            Some(p) if p.scheduled != new => {
                let original = p.original;
                self.pending = Some(Pending { original, scheduled: new });
                Timer::Restart
            }
            Some(_) => Timer::Leave,
        }
    }

    /// The delay ran out: the window to focus.
    pub fn fire(&mut self) -> Option<W> {
        self.pending.take().map(|p| p.scheduled)
    }

    /// The shell put the pointer on `w` itself (cursor_follows_focus):
    /// that's not the user pointing somewhere new.
    pub fn warped(&mut self, w: Option<W>) {
        self.under = w;
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_cancels_and_reschedules_like_cosmic() {
        let mut f = Follow::new();
        assert_eq!(f.moved(Some(1)), Timer::Restart, "the first window counts as a change from none");
        f.fire();
        assert_eq!(f.moved(Some(1)), Timer::Leave, "still over it");
        assert_eq!(f.moved(Some(2)), Timer::Restart);
        assert_eq!(f.moved(Some(1)), Timer::Cancel, "back where it started");
        assert_eq!(f.fire(), None);
        assert_eq!(f.moved(Some(2)), Timer::Restart);
        assert_eq!(f.moved(Some(3)), Timer::Restart, "on to a third: rescheduled");
        assert_eq!(f.moved(None), Timer::Leave, "the desktop changes nothing");
        assert_eq!(f.moved(Some(3)), Timer::Leave, "the same window it's scheduled for");
        assert_eq!(f.fire(), Some(3));
        f.warped(Some(1));
        assert_eq!(f.moved(Some(1)), Timer::Leave, "a warp isn't a move");
    }
}
