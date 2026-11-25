//! One move in flight per window. Moves go out async, so a hung app can't
//! hang the shell, but then each waits in the app's own queue: a Super+drag
//! resize sends one per pointer move, and a console, slow to resize (it
//! re-wraps its text), kept working through them after the drag ended
//! (saturn, 2026-11-20). A window still busy gets only the newest place,
//! sent once it has handled the last, or after `IN_FLIGHT_FOR` anyway.
//!
//! Pure; desktop.rs sends the moves and runs the flush timer.

use std::time::{Duration, Instant};

/// How long a window gets to handle a move before the next goes anyway:
/// one that can't move (at its minimum size, say) mustn't hold the newest
/// place back for good.
pub const IN_FLIGHT_FOR: Duration = Duration::from_millis(100);

/// A window's move that was sent, from `from` at `sent`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Move<R> {
    Sent { from: R, sent: Instant },
    /// Still on the one sent, with the newest place waiting.
    Holding { from: R, sent: Instant, to: R },
}

impl<R: Copy> Move<R> {
    fn sent(&self) -> (R, Instant) {
        match *self {
            Move::Sent { from, sent } | Move::Holding { from, sent, .. } => (from, sent),
        }
    }
}

/// What to do with a move asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Send it now.
    Send,
    /// Held until the flush; the flush timer should be (re)started.
    Hold,
}

/// Every window with a move out, in the order their places were last held,
/// which is the order a flush sends them in.
pub struct Moves<W, R> {
    moves: Vec<(W, Move<R>)>,
}

impl<W: Copy + PartialEq, R: Copy + PartialEq> Moves<W, R> {
    pub const fn new() -> Self {
        Moves { moves: Vec::new() }
    }

    /// `w` is to go to `to`. `rect` says where it is now (None if it's
    /// gone); it's asked only when the answer matters.
    pub fn ask(&mut self, w: W, to: R, now: Instant, rect: impl Fn() -> Option<R>) -> Ask {
        if let Some(i) = self.moves.iter().position(|&(x, _)| x == w) {
            let (_, m) = self.moves.remove(i);
            let (from, sent) = m.sent();
            // Not handled yet: still where it was when the move went out.
            if now.saturating_duration_since(sent) < IN_FLIGHT_FOR && rect() == Some(from) {
                self.moves.push((w, Move::Holding { from, sent, to }));
                return Ask::Hold;
            }
        }
        if let Some(from) = rect().filter(|&from| from != to) {
            self.moves.push((w, Move::Sent { from, sent: now }));
        }
        Ask::Send
    }

    /// The held places, oldest first, each to be asked for again; they're
    /// held no longer meanwhile.
    pub fn take_held(&mut self) -> Vec<(W, R)> {
        let mut held = Vec::new();
        for (w, m) in &mut self.moves {
            if let Move::Holding { from, sent, to } = *m {
                *m = Move::Sent { from, sent };
                held.push((*w, to));
            }
        }
        held
    }

    pub fn holding(&self) -> bool {
        self.moves.iter().any(|(_, m)| matches!(m, Move::Holding { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = (i32, i32);

    #[test]
    fn a_window_with_nothing_out_gets_its_move_at_once() {
        let mut m: Moves<u8, R> = Moves::new();
        assert_eq!(m.ask(1, (10, 0), Instant::now(), || Some((0, 0))), Ask::Send);
        assert!(!m.holding());
    }

    /// The Super+drag resize: every pointer move asks, and while the window
    /// hasn't handled the first, only the newest place waits.
    #[test]
    fn one_move_in_flight_and_only_the_newest_held() {
        let t = Instant::now();
        let mut m: Moves<u8, R> = Moves::new();
        let at = || Some((0, 0));
        assert_eq!(m.ask(1, (10, 0), t, at), Ask::Send);
        assert_eq!(m.ask(1, (20, 0), t + Duration::from_millis(5), at), Ask::Hold);
        assert_eq!(m.ask(1, (30, 0), t + Duration::from_millis(10), at), Ask::Hold);
        assert_eq!(m.take_held(), vec![(1, (30, 0))]);
        assert!(!m.holding());
        assert_eq!(m.take_held(), vec![]);
    }

    #[test]
    fn once_it_has_moved_the_held_place_goes() {
        let t = Instant::now();
        let mut m: Moves<u8, R> = Moves::new();
        assert_eq!(m.ask(1, (10, 0), t, || Some((0, 0))), Ask::Send);
        assert_eq!(m.ask(1, (20, 0), t, || Some((0, 0))), Ask::Hold);
        let held = m.take_held();
        assert_eq!(held, vec![(1, (20, 0))]);
        // The flush asks again, and the window is at (10, 0) now.
        assert_eq!(m.ask(1, (20, 0), t + Duration::from_millis(8), || Some((10, 0))), Ask::Send);
    }

    /// One that can't move, at its minimum size say, mustn't hold the
    /// newest place back for good.
    #[test]
    fn after_100_ms_the_next_goes_anyway() {
        let t = Instant::now();
        let mut m: Moves<u8, R> = Moves::new();
        let stuck = || Some((0, 0));
        assert_eq!(m.ask(1, (10, 0), t, stuck), Ask::Send);
        assert_eq!(m.ask(1, (20, 0), t + Duration::from_millis(99), stuck), Ask::Hold);
        m.take_held();
        assert_eq!(m.ask(1, (20, 0), t + IN_FLIGHT_FOR, stuck), Ask::Send);
        // That one is in flight now, from when it went.
        assert_eq!(m.ask(1, (30, 0), t + IN_FLIGHT_FOR + Duration::from_millis(1), stuck), Ask::Hold);
    }

    #[test]
    fn a_window_already_there_or_gone_has_nothing_in_flight() {
        let t = Instant::now();
        let mut m: Moves<u8, R> = Moves::new();
        assert_eq!(m.ask(1, (0, 0), t, || Some((0, 0))), Ask::Send);
        assert_eq!(m.ask(1, (10, 0), t, || Some((0, 0))), Ask::Send);
        let mut gone: Moves<u8, R> = Moves::new();
        assert_eq!(gone.ask(2, (10, 0), t, || None), Ask::Send);
        assert_eq!(gone.ask(2, (20, 0), t, || None), Ask::Send);
    }

    #[test]
    fn windows_are_throttled_apart_and_flushed_in_the_order_held() {
        let t = Instant::now();
        let mut m: Moves<u8, R> = Moves::new();
        let at = || Some((0, 0));
        m.ask(1, (10, 0), t, at);
        m.ask(2, (10, 0), t, at);
        assert_eq!(m.ask(3, (10, 0), t, at), Ask::Send, "another window's move doesn't wait");
        assert_eq!(m.ask(2, (20, 0), t, at), Ask::Hold);
        assert_eq!(m.ask(1, (20, 0), t, at), Ask::Hold);
        assert_eq!(m.ask(2, (30, 0), t, at), Ask::Hold);
        assert_eq!(m.take_held(), vec![(1, (20, 0)), (2, (30, 0))]);
    }
}
