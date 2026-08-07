pub mod bulk_fill;
pub mod pending_queue;
pub mod rseq_asm;
pub mod rseq_offsets;
pub mod slab_cache;

pub mod bitmap {
    use std::sync::atomic::{AtomicU64, Ordering};

    #[inline(always)]
    fn cpu_word_bit(cpu_id: usize) -> (usize, u64) {
        let word = cpu_id >> 6;
        let bit = 1u64 << (cpu_id & 63);
        (word, bit)
    }

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
