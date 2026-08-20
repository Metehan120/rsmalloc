use std::hint::unlikely;

use crate::{Header, internals::oncelock::OnceLock};

pub const SIZE_CLASSES: [usize; 34] = [
    // Tiny (16-128) - 16 Byte steps
    16, 32, 48, 64, 80, 96, 128, // Small (160-512) - 32/64 Byte steps
    160, 192, 256, 320, 384, 512, // Medium (768-3072) - Large steps
    768, 1024, 1280, 1536, 1792, 2048, 2560, 3072, // Large (3840-24KB)
    3840, 4096, 8192, 12288, 16384, 24576, // Very Large (32KB+)
    32768, 65536, 131072, 262144, 524288, 1048576, 2097152,
];

pub const NUM_SIZE_CLASSES: usize = SIZE_CLASSES.len();

const REFILL_TINY_BYTES: usize = 32 * 1024;
const REFILL_SMALL_BYTES: usize = 16 * 1024;
const REFILL_MEDIUM_BYTES: usize = 16 * 1024;
const REFILL_LARGE_BYTES: usize = 16 * 1024;

#[inline(always)]
const fn refill_target_bytes(payload: usize) -> usize {
    if payload <= 128 {
        REFILL_TINY_BYTES
    } else if payload <= 512 {
        REFILL_SMALL_BYTES
    } else if payload <= 1536 {
        REFILL_MEDIUM_BYTES
    } else {
        let byte = REFILL_LARGE_BYTES;
        if byte == 0 { 1 } else { byte }
    }
}

const fn refill_iterations_for_payload(payload: usize) -> usize {
    let target = refill_target_bytes(payload);
    if target == 0 {
        return 1;
    }

    let block_size = align_to(payload + Header::SIZE, 16);
    let blocks = target / block_size;
    if blocks == 0 { 1 } else { blocks }
}

pub const ITERATIONS: [usize; NUM_SIZE_CLASSES] = {
    let mut arr = [1; NUM_SIZE_CLASSES];
    let mut i = 0;

    while i < NUM_SIZE_CLASSES {
        arr[i] = refill_iterations_for_payload(SIZE_CLASSES[i]);
        i += 1;
    }

    arr
};

const fn refill_total_bytes_for_payload(payload: usize) -> usize {
    let block_size = align_to(payload + Header::SIZE, 16);
    let num_blocks = refill_iterations_for_payload(payload);
    let meta_size = core::mem::size_of::<crate::MetaData>();
    let total = meta_size + block_size * num_blocks;

    let pages = (total + 4095) / 4096;
    let available_bytes = pages * 4096 - meta_size;
    let max_blocks_in_pages = available_bytes / block_size;

    if max_blocks_in_pages > num_blocks {
        meta_size + block_size * max_blocks_in_pages
    } else {
        total
    }
}

pub const MIN_REFILL_BYTES: usize = {
    let mut min = usize::MAX;
    let mut i = 0;

    while i < NUM_SIZE_CLASSES {
        let total = refill_total_bytes_for_payload(SIZE_CLASSES[i]);
        if total < min {
            min = total;
        }
        i += 1;
    }

    align_to(min, 4096)
};

pub const SIZE_LUT: [u8; 256] = {
    let mut lut = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let size = (i + 1) * 16;
        let mut class = 0;
        while class < NUM_SIZE_CLASSES && SIZE_CLASSES[class] < size {
            class += 1;
        }
        lut[i] = class as u8;
        i += 1;
    }
    lut
};

// Classes between 4 KiB and 32 KiB are irregular, but fit in eight 4 KiB
// buckets. Classes above 32 KiB are powers of two and are matched arithmetically.
const LARGE_SIZE_LUT: [u8; 8] = [0, 23, 24, 25, 26, 26, 27, 27];

pub const BIG_CLASS_BYTES: usize = 1024 * 96;
pub const MEDIUM_CLASS_BYTES: usize = 1024 * 96;
pub const SMALL_CLASS_BYTES: usize = 1024 * 128;
pub const CACHE_HIGH_BLOCKS: [usize; NUM_SIZE_CLASSES] = {
    let mut arr = [0; NUM_SIZE_CLASSES];
    let mut i = 0;

    while i < NUM_SIZE_CLASSES {
        let payload = SIZE_CLASSES[i];
        let block_size = align_to(payload + Header::SIZE, 16);
        let mut blocks = if block_size > SMALL_CLASS_BYTES {
            1
        } else {
            if payload < 256 {
                SMALL_CLASS_BYTES / block_size
            } else if payload < 1024 * 16 {
                MEDIUM_CLASS_BYTES / block_size
            } else {
                BIG_CLASS_BYTES / block_size
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

#[allow(dead_code)]
pub const CACHE_LOW_BLOCKS: [usize; NUM_SIZE_CLASSES] = {
    let mut arr = [0; NUM_SIZE_CLASSES];
    let mut i = 0;

    while i < NUM_SIZE_CLASSES {
        let low = CACHE_HIGH_BLOCKS[i] / 2;
        arr[i] = if low == 0 { 1 } else { low };
        i += 1;
    }

    arr
};

pub static CLASS_4096_OFFSET: OnceLock<usize> = OnceLock::new();

pub fn get_size_4096_class() -> usize {
    *CLASS_4096_OFFSET.get_or_init(|| SIZE_CLASSES.iter().position(|&s| s >= 4096).unwrap_or(22))
}

#[inline(always)]
pub unsafe fn match_size_class(size: usize) -> Option<usize> {
    if size == 0 {
        return Some(0);
    } else if size <= 4096 {
        let index = (size - 1) >> 4;
        return Some(*SIZE_LUT.get_unchecked(index) as usize);
    }

    if size > 2097152 {
        return None;
    }

    if size <= 32768 {
        let index = (size - 1) >> 12;
        return Some(*LARGE_SIZE_LUT.get_unchecked(index) as usize);
    }

    let exponent = usize::BITS as usize - (size - 1).leading_zeros() as usize;
    Some(exponent + 12)
}

#[cfg(test)]
mod tests {
    use super::{NUM_SIZE_CLASSES, SIZE_CLASSES, match_size_class};

    fn reference_match(size: usize) -> Option<usize> {
        if size == 0 {
            return Some(0);
        }
        SIZE_CLASSES
            .iter()
            .position(|&class_size| size <= class_size)
    }

    #[test]
    fn fast_size_matching_matches_reference_for_every_slab_size() {
        for size in 0..=SIZE_CLASSES[NUM_SIZE_CLASSES - 1] + 1 {
            assert_eq!(unsafe { match_size_class(size) }, reference_match(size));
        }
    }
}

#[must_use]
#[inline(always)]
const fn align_to(size: usize, align: usize) -> usize {
    let al = align - 1;
    (size + al) & !al
}

pub trait Alignment<T> {
    fn align_to(self, align: T) -> T;
    fn checked_align_to(self, align: T) -> Option<T>;
    #[allow(dead_code)]
    fn checked_align_of_page(self, align: T) -> Option<T>;
}

macro_rules! impl_align {
    ($($u:ty),*) => {
        $(impl Alignment<$u> for $u {
            #[inline(always)]
            fn align_to(self, align: $u) -> $u {
                let al = align - 1;
                (self + al) & !al
            }

            #[inline(always)]
            fn checked_align_to(self, align: $u) -> Option<$u> {
                let al = align - 1;
                let aligned = self.checked_add(al)? & !al;
                if unlikely(aligned < self) {
                    return None;
                }
                Some(aligned)
            }

            #[inline(always)]
            fn checked_align_of_page(self, align: $u) -> Option<$u> {
                if !align.is_multiple_of(4096){
                    return None;
                }
                self.checked_align_to(align)
            }
        })*
    };
}

impl_align!(usize, u64, u32, u16);
