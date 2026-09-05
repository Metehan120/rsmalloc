use super::{
    BIG_BUDDY_MIN_ORDER, BUDDY_NUM_ORDERS, Flags,
    atomics::{AtomicU32, AtomicU64, Ordering},
};

pub(super) const SLOTS: usize = 16;
pub(super) const SLOT_BYTES: usize = 1 << BIG_BUDDY_MIN_ORDER;
pub(super) const SEGMENT_BYTES: usize = SLOTS * SLOT_BYTES;
const DIRTY_SHIFT: u32 = 16;
const USED_SHIFT: u32 = 32;

#[repr(C, align(64))]
struct Hot {
    word: AtomicU64,
    base: usize,
}

#[repr(C, align(64))]
pub(super) struct Segment {
    hot: Hot,
    freed_at: [AtomicU32; SLOTS],
}

#[inline(always)]
pub(super) fn candidates(busy: u16, order: usize) -> u16 {
    let mut free = !busy;
    if order >= 1 {
        free &= free >> 1;
    }
    if order >= 2 {
        free &= free >> 2;
    }
    if order >= 3 {
        free &= free >> 4;
    }
    if order >= 4 {
        free &= free >> 8;
    }
    free & [0xffff, 0x5555, 0x1111, 0x0101, 0x0001][order]
}

#[inline(always)]
pub(super) fn slot_mask(start: usize, order: usize) -> u16 {
    (((1u32 << (1 << order)) - 1) << start) as u16
}

#[inline(always)]
pub(super) fn classify(word: u64, mask: u16) -> Flags {
    if (word >> DIRTY_SHIFT) as u16 & mask != 0 {
        Flags::Allocated
    } else if (word >> USED_SHIFT) as u16 & mask != 0 {
        Flags::Trimmed
    } else {
        Flags::NotAllocated
    }
}

impl Segment {
    pub(super) fn new(base: usize) -> Self {
        Self {
            hot: Hot {
                word: AtomicU64::new(0),
                base,
            },
            freed_at: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    #[inline(always)]
    pub(super) fn base(&self) -> usize {
        self.hot.base
    }

    #[inline(always)]
    pub(super) fn snapshot(&self) -> u64 {
        self.hot.word.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub(super) fn dirty_free(word: u64) -> u16 {
        !(word as u16) & (word >> DIRTY_SHIFT) as u16
    }

    #[inline(always)]
    pub(super) fn age(&self, slot: usize, now: u32) -> u32 {
        now.saturating_sub(self.freed_at[slot].load(Ordering::Relaxed))
    }

    #[inline(always)]
    pub(super) fn alloc(&self, order: usize) -> Option<(usize, Flags)> {
        debug_assert!(order < BUDDY_NUM_ORDERS);
        #[cfg(all(test, feature = "debug-exact"))]
        super::atomics::probe();
        let mut word = self.snapshot();
        loop {
            let available = candidates(word as u16, order);
            if available == 0 {
                return None;
            }
            let start = available.trailing_zeros() as usize;
            let mask = slot_mask(start, order) as u64;
            let next = word | mask | (mask << DIRTY_SHIFT) | (mask << USED_SHIFT);
            match self.hot.word.compare_exchange_weak(
                word,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some((
                        self.base() + start * SLOT_BYTES,
                        classify(word, mask as u16),
                    ));
                }
                Err(observed) => {
                    #[cfg(all(test, feature = "debug-exact"))]
                    super::atomics::retry();
                    word = observed;
                }
            }
        }
    }

    #[inline(always)]
    fn allocation_mask(&self, addr: usize, order: usize) -> Option<u16> {
        if order >= BUDDY_NUM_ORDERS {
            return None;
        }
        let offset = addr.checked_sub(self.base())?;
        if offset >= SEGMENT_BYTES || offset & ((SLOT_BYTES << order) - 1) != 0 {
            return None;
        }
        Some(slot_mask(offset / SLOT_BYTES, order))
    }

    #[inline(always)]
    pub(super) unsafe fn free(&self, addr: usize, order: usize, stamp: u32) {
        let Some(mask) = self.allocation_mask(addr, order) else {
            return;
        };
        let start = (addr - self.base()) / SLOT_BYTES;
        for slot in start..start + (1 << order) {
            self.freed_at[slot].store(stamp, Ordering::Relaxed);
        }
        let previous = self.hot.word.fetch_and(!(mask as u64), Ordering::Release);
        debug_assert_eq!(previous as u16 & mask, mask);
    }

    #[inline(always)]
    pub(super) unsafe fn grow(&self, addr: usize, order: usize) -> bool {
        if order >= BUDDY_NUM_ORDERS - 1 {
            return false;
        }
        let Some(whole) = self.allocation_mask(addr, order + 1) else {
            return false;
        };
        let left = self.allocation_mask(addr, order).unwrap();
        let right = (whole ^ left) as u64;
        let mut word = self.snapshot();
        loop {
            if word as u16 & whole != left {
                return false;
            }
            let next = word | right | (right << DIRTY_SHIFT) | (right << USED_SHIFT);
            match self.hot.word.compare_exchange_weak(
                word,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => {
                    #[cfg(all(test, feature = "debug-exact"))]
                    super::atomics::retry();
                    word = observed;
                }
            }
        }
    }

    pub(super) fn claim_trim(&self, mask: u16) -> bool {
        let mut word = self.snapshot();
        loop {
            if mask == 0 || Self::dirty_free(word) & mask != mask {
                return false;
            }
            match self.hot.word.compare_exchange_weak(
                word,
                word | mask as u64,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => {
                    #[cfg(all(test, feature = "debug-exact"))]
                    super::atomics::retry();
                    word = observed;
                }
            }
        }
    }

    pub(super) fn finish_trim(&self, mask: u16, success: bool, stamp: Option<u32>) {
        if mask == 0 {
            return;
        }
        if let Some(stamp) = stamp {
            let mut slots = mask;
            while slots != 0 {
                let slot = slots.trailing_zeros() as usize;
                slots &= slots - 1;
                self.freed_at[slot].store(stamp, Ordering::Relaxed);
            }
        }
        let clear = mask as u64
            | if success {
                (mask as u64) << DIRTY_SHIFT
            } else {
                0
            };
        self.hot.word.fetch_and(!clear, Ordering::Release);
    }
}
