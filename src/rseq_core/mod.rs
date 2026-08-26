pub mod bulk_fill;
pub mod pending_queue;
pub mod rseq_asm;
pub mod rseq_offsets;
pub mod slab_cache;

pub mod bitmap {
    use std::sync::atomic::{AtomicU64, Ordering};

    #[inline(always)]
    pub fn cpu_word_bit(cpu_id: usize) -> (usize, u64) {
        let word = cpu_id >> 6;
        let bit = 1u64 << (cpu_id & 63);
        (word, bit)
    }

    #[doc(hidden)]
    #[macro_export]
    macro_rules! bitmap_word {
        ($map:expr, $class:expr, $word:expr, $words:expr) => {
            $map.add($class * $words + $word)
        };
    }

    // # Safety:
    // Trust me bro
    pub unsafe fn cpu_bit_set(map: *mut AtomicU64, class: usize, cpu_id: usize, words: usize) {
        let (word, bit) = cpu_word_bit(cpu_id);
        let ptr = bitmap_word!(map, class, word, words);
        if (*ptr).load(Ordering::Relaxed) & bit == 0 {
            (*ptr).fetch_or(bit, Ordering::Relaxed);
        }
    }

    pub unsafe fn cpu_bit_clear(map: *mut AtomicU64, class: usize, cpu_id: usize, words: usize) {
        let (word, bit) = cpu_word_bit(cpu_id);
        let ptr = bitmap_word!(map, class, word, words);
        if (*ptr).load(Ordering::Relaxed) & bit != 0 {
            (*ptr).fetch_and(!bit, Ordering::Relaxed);
        }
    }

    pub unsafe fn cpu_is_empty(
        map: *mut AtomicU64,
        class: usize,
        cpu_id: usize,
        words: usize,
    ) -> bool {
        let (word, bit) = cpu_word_bit(cpu_id);
        let ptr = bitmap_word!(map, class, word, words);
        (*ptr).load(Ordering::Relaxed) & bit == 0
    }

    pub unsafe fn cpu_try_marking(
        map: *mut AtomicU64,
        class: usize,
        cpu_id: usize,
        words: usize,
    ) -> bool {
        let (word, bit) = cpu_word_bit(cpu_id);
        let ptr = bitmap_word!(map, class, word, words);
        if (*ptr).load(Ordering::Relaxed) & bit != 0 {
            return false;
        }
        (*ptr).fetch_or(bit, Ordering::Relaxed);
        true
    }
}

pub mod aba {
    use crate::Header;

    pub struct PackedTag {
        pub current_header: *mut Header,
        pub old_packed: u128,
    }

    pub const TAG_SHIFT: u32 = 64;
    const PTR_MASK: u128 = u64::MAX as u128;

    pub struct Tagging;

    impl Tagging {
        #[inline(always)]
        pub fn tag_ptr(&self, ptr: *mut Header, old_tag: u128) -> u128 {
            // this shift should be eliminated at compile time
            let tag = (old_tag >> TAG_SHIFT) as u64;
            let tag = tag.wrapping_add(1);

            ((ptr as usize as u128) & PTR_MASK) | ((tag as u128) << TAG_SHIFT)
        }

        #[inline(always)]
        pub fn untag_ptr(&self, word: u128) -> PackedTag {
            PackedTag {
                current_header: (word & PTR_MASK) as usize as *mut Header,
                old_packed: word,
            }
        }
    }
}
