use std::hint::{likely, unlikely};

use crate::internals::oncelock::OnceLock;

pub const SIZE_CLASSES: [usize; 34] = [
    // Tiny (16-128) - 16 Byte steps
    16, 32, 48, 64, 80, 96, 128, // Small (160-512) - 32/64 Byte steps
    160, 192, 256, 320, 384, 512, // Medium (768-3072) - Large steps
    768, 1024, 1280, 1536, 1792, 2048, 2560, 3072, // Large (3840-24KB)
    3840, 4096, 8192, 12288, 16384, 24576, // Very Large (32KB+)
    32768, 65536, 131072, 262144, 524288, 1048576, 2097152,
];

pub const ITERATIONS: [usize; 34] = [
    // TINY (Targets ~32KB / 8 Pages)
    // 16B  -> Block 80B  -> 32768 / 80  = 409 (48B Waste)
    // 32B  -> Block 96B  -> 32768 / 96  = 341 (32B Waste)
    409, 341, 292, 255, 227, 153, 170,
    // --- SMALL (Targets ~8KB / 2 Pages)
    // 160B -> Block 224B -> 8192 / 224 = 36 (128B Waste)
    36, 31, 25, 21, 18, 14,
    // MEDIUM (Targets ~8KB or ~4KB)
    // 768B -> Block 832B -> 8192 / 832 = 9 (704B Waste)
    // We drop to N=1 quickly for sizes > 2KB to prevent VIRT bloat
    9, 7, 6, 2, 4, 3, 1, 2,
    // LARGE (Targets 1 Block)
    // For sizes > 3KB, we want Malloc/Free to be 1:1 with mmap/munmap logic
    // via bulk_fill to allow immediate reclamation by gtrim and ptrim
    1, 1, 1, 1, 1, 1,
    // --- VERY LARGE ---
    //
    // Always 1. Let the OS handle the pages.
    1, 1, 1, 1, 1, 1, 1,
];

pub const SIZE_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let size = (i + 1) * 16;
        let mut class = 0;
        while class < 34 && SIZE_CLASSES[class] < size {
            class += 1;
        }
        lut[i] = class as u8;
        i += 1;
    }
    lut
};

pub const NUM_SIZE_CLASSES: usize = SIZE_CLASSES.len();
static CLASS_4096: OnceLock<usize> = OnceLock::new();

pub fn get_size_4096_class() -> usize {
    *CLASS_4096.get_or_init(|| SIZE_CLASSES.iter().position(|&s| s >= 4096).unwrap())
}

#[must_use]
#[inline(always)]
pub const fn align_to(size: usize, align: usize) -> usize {
    let al = align - 1;
    (size + al) & !al
}

pub const RSEQ_BIG_CLASS_BYTES: usize = 1024 * 96;
pub const RSEQ_MEDIUM_CLASS_BYTES: usize = 1024 * 128;
pub const RSEQ_SMALL_CLASS_BYTES: usize = 1024 * 196;
pub const RSEQ_MAX_BLOCKS: [usize; NUM_SIZE_CLASSES] = {
    let mut arr = [0; NUM_SIZE_CLASSES];
    let mut i = 0;

    while i < NUM_SIZE_CLASSES {
        let payload = SIZE_CLASSES[i];
        let block_size = align_to(payload + 16, 16);
        let mut blocks = if block_size > RSEQ_SMALL_CLASS_BYTES {
            1
        } else {
            if payload < 256 {
                RSEQ_SMALL_CLASS_BYTES / block_size
            } else if payload < 1024 * 16 {
                RSEQ_MEDIUM_CLASS_BYTES / block_size
            } else {
                RSEQ_BIG_CLASS_BYTES / block_size
            }
        };

        if blocks == 0 {
            blocks = 1;
        }

        arr[i] = blocks;
        i += 1;
    }

    arr
};

#[inline(always)]
pub fn match_size_class(size: usize) -> Option<usize> {
    if likely(size <= 4096 && size > 0) {
        let index = (size - 1) >> 4;
        return Some(unsafe { *SIZE_LUT.get_unchecked(index) as usize });
    }

    if unlikely(size == 0 || size > 2097152) {
        return None;
    }

    slow_path_match(size)
}

#[inline(always)]
fn slow_path_match(size: usize) -> Option<usize> {
    for i in 0..NUM_SIZE_CLASSES {
        if size <= SIZE_CLASSES[i] {
            return Some(i);
        }
    }
    None
}
