use crate::Header;

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
        let block_size = align_to(payload + Header::SIZE, 16);
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
    if size <= 4096 && size > 0 {
        let index = (size - 1) >> 4;
        return Some(unsafe { *SIZE_LUT.get_unchecked(index) as usize });
    }

    if size == 0 || size > 2097152 {
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
