use std::hint::spin_loop;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::yield_now;

const INACTIVE: u64 = 0;
const ACTIVE: u64 = 1;
const SHIFT: u32 = 3;
const PHASE_MASK: u64 = (1 << SHIFT) - 1;
const WAIT: u64 = 2;
const READY: u64 = 3;
const CLAIMED: u64 = 4;
const DONE: u64 = 5;
const MAX_TICKET: u64 = u64::MAX >> SHIFT;

static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);

fn next_ticket(counter: &AtomicU64) -> Ticket {
    let mut next = counter.load(Ordering::Relaxed);
    loop {
        let following = next.wrapping_add(1) & MAX_TICKET;
        match counter.compare_exchange_weak(next, following, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ticket(next),
            Err(observed) => next = observed,
        }
    }
}

/// An opaque request identity, unique until the ticket counter wraps.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ticket(u64);

impl Ticket {
    fn state(self, phase: u64) -> u64 {
        (self.0 << SHIFT) | phase
    }
}

/// A zero-initialized per-mailbox trim handshake.
#[derive(Debug)]
#[repr(transparent)]
pub struct TrimMessage {
    state: AtomicU64,
}

impl Default for TrimMessage {
    fn default() -> Self {
        Self::new()
    }
}

impl TrimMessage {
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(INACTIVE),
        }
    }

    /// Begin a trim; callers must serialize trimmers globally.
    pub fn begin(&self) -> bool {
        self.state
            .compare_exchange(INACTIVE, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Check whether a trim is in progress.
    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) != INACTIVE
    }

    /// Register a request; failed attempts still consume a ticket.
    pub fn request(&self) -> Option<Ticket> {
        let ticket = next_ticket(&NEXT_TICKET);
        self.state
            .compare_exchange(
                ACTIVE,
                ticket.state(WAIT),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()?;
        Some(ticket)
    }

    /// Return the current pending request, if any. For the trimmer only.
    pub fn pending(&self) -> Option<Ticket> {
        let state = self.state.load(Ordering::Acquire);
        (state & PHASE_MASK == WAIT).then_some(Ticket(state >> SHIFT))
    }

    /// Signal readiness after publishing the batch.
    pub fn signal_ready(&self, ticket: Ticket) -> bool {
        self.state
            .compare_exchange(
                ticket.state(WAIT),
                ticket.state(READY),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Claim a ready request; the claimant must call `complete`.
    pub fn claim(&self, ticket: Ticket) -> bool {
        self.state
            .compare_exchange(
                ticket.state(READY),
                ticket.state(CLAIMED),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Poll for readiness, cancelling on timeout. Each ticket has one requester.
    pub fn wait_ready(&self, ticket: Ticket, attempts: usize) -> bool {
        for i in 0..attempts {
            let state = self.state.load(Ordering::Acquire);
            if state == ticket.state(READY) {
                if self.claim(ticket) {
                    return true;
                }
            } else if state != ticket.state(WAIT) {
                return false;
            }
            spin_loop();

            if i + 1 == (attempts / 2).max(1) && i + 1 < attempts {
                Self::yield_waiter();
            } else {
                spin_loop();
            }
        }
        self.cancel(ticket);
        false
    }

    #[cold]
    #[inline(never)]
    fn yield_waiter() {
        yield_now();
    }

    /// Cancel a waiting or ready ticket; claimed tickets cannot be cancelled.
    pub fn cancel(&self, ticket: Ticket) -> bool {
        for phase in [WAIT, READY] {
            if self
                .state
                .compare_exchange(
                    ticket.state(phase),
                    ACTIVE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
        false
    }

    /// Publish completion after the claimant has finished using the work.
    pub fn complete(&self, ticket: Ticket) -> bool {
        self.state
            .compare_exchange(
                ticket.state(CLAIMED),
                ticket.state(DONE),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Wait for completion, cancelling unclaimed tickets after `ready_attempts`.
    /// A stalled claimant can block the trimmer indefinitely.
    pub fn wait_done(&self, ticket: Ticket, mut ready_attempts: usize) -> bool {
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state == ticket.state(DONE) {
                return self
                    .state
                    .compare_exchange(state, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok();
            }
            if state == ticket.state(READY) {
                if ready_attempts == 0 {
                    if self
                        .state
                        .compare_exchange(state, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return false;
                    }
                    // A concurrent claim must be observed and waited out.
                    continue;
                }
                ready_attempts -= 1;
            } else if state != ticket.state(CLAIMED) {
                return false;
            }
            spin_loop();
        }
    }

    /// Close the trim, cancelling unclaimed requests and waiting for claimants.
    pub fn finish(&self) -> bool {
        loop {
            let state = self.state.load(Ordering::Acquire);
            match state {
                INACTIVE => return false,
                ACTIVE => {
                    if self
                        .state
                        .compare_exchange(ACTIVE, INACTIVE, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return true;
                    }
                }
                _ => match state & PHASE_MASK {
                    WAIT | READY => {
                        self.cancel(Ticket(state >> SHIFT));
                    }
                    CLAIMED => spin_loop(),
                    DONE => {
                        let _ = self.state.compare_exchange(
                            state,
                            ACTIVE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                    }
                    _ => unreachable!("invalid trim message state"),
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_counter_wraps_after_maximum() {
        let counter = AtomicU64::new(MAX_TICKET - 1);
        assert_eq!(next_ticket(&counter), Ticket(MAX_TICKET - 1));
        assert_eq!(next_ticket(&counter), Ticket(MAX_TICKET));
        assert_eq!(next_ticket(&counter), Ticket(0));
        assert_eq!(next_ticket(&counter), Ticket(1));
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }
}
