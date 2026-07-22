use std::alloc::{GlobalAlloc, Layout};
use std::hint::likely;
use std::ptr::{null_mut, write_bytes};

use rustix::rand::{GetRandomFlags, getrandom};

use crate::backend::page_allocator::ARENA_SIZE;
use crate::big_allocations::buddy::BUDDY_BACKEND;
use crate::core_prim::predictor::{DEFAULT_BATCH, PREDICTOR_INIT_BATCH};
use crate::core_prim::wrappers::UnsafePointer;
use crate::inner::align::memalign_inner;
use crate::inner::alloc::{MAX_REFILL_RETRIES, rs_alloc, usable_size};
use crate::inner::calloc::rs_calloc;
use crate::inner::free::rs_free;
use crate::inner::realloc::rs_realloc;
use crate::internals::l3_main_radix::{RADIX, RadixTree};
use crate::internals::once::Once;
use crate::rseq_core::rseq_offsets::__rseq_offset;
use crate::rseq_core::rseq_offsets::__rseq_size;
use crate::rseq_core::slab_cache::SLAB_CACHE;
use crate::trim::{BUDDY_DISABLE_PERCENTAGE, BUDDY_ENABLE_PERCENTAGE, DISABLE_RELIEF, trim_small};
use crate::{
    ALIGN_TAG, BIG_MAGIC, BUDDY_ATTEMPT_HUGE, BUDDY_MAX_CACHE, DISABLE_TRIM_THREAD,
    FOREIGN_POINTER_ABORT, FREED_MAGIC, Header, MAGIC, MAGIC_DISABLE, RS_DISABLE_THP,
    RSMallocError, TRIM_THRESHOLD,
};

#[inline(never)]
unsafe fn init_magic() {
    #[cfg(not(feature = "extended-header"))]
    {
        let mut main = 0u16.to_le_bytes();

        match getrandom(&mut main, GetRandomFlags::empty()) {
            Ok(_) => {
                let small = u16::from_le_bytes(main);
                let medium = small.wrapping_sub(1);
                let large = medium.wrapping_sub(1);

                MAGIC = small;
                FREED_MAGIC = medium;
                BIG_MAGIC = large;
            }
            Err(err) => RSMallocError::SecurityViolation.log_and_abort(
                null_mut(),
                "calling getrandom failed, cannot initialize magic",
                Some(err.raw_os_error()),
            ),
        };
    }

    #[cfg(feature = "extended-header")]
    {
        let mut main = 0u64.to_le_bytes();
        match getrandom(&mut main, GetRandomFlags::empty()) {
            Ok(_) => {
                let small = u64::from_le_bytes(main);
                let medium = small.wrapping_sub(1);
                let large = medium.wrapping_sub(1);

                MAGIC = small;
                FREED_MAGIC = medium;
                BIG_MAGIC = large;
            }
            Err(err) => RSMallocError::SecurityViolation.log_and_abort(
                null_mut(),
                "calling getrandom failed, cannot initialize magic",
                Some(err.raw_os_error()),
            ),
        };
    }
}

unsafe fn init_align() {
    let mut main = 0usize.to_le_bytes();
    match getrandom(&mut main, GetRandomFlags::empty()) {
        Ok(_) => {
            ALIGN_TAG = usize::from_le_bytes(main);
        }
        Err(err) => RSMallocError::SecurityViolation.log_and_abort(
            null_mut(),
            "calling getrandom failed, cannot initialize align tag",
            Some(err.raw_os_error()),
        ),
    };
}

// ------------------

/// Result of querying the usable size of an allocation.
///
/// `Size::RS(bytes)` means the pointer appears to belong to rsmalloc and the
/// allocator could determine its usable payload size. `Size::NotRS` means the
/// pointer was not recognized as an rsmalloc allocation in this build/mode.
#[must_use]
pub enum Size {
    /// Usable payload size in bytes.
    RS(usize),
    /// Pointer was not recognized as an rsmalloc allocation.
    NotRS,
}

/// Experimental features for the allocator.
#[derive(Clone, Copy, Debug)]
pub struct ExperimentalFeatures {}

impl ExperimentalFeatures {
    pub const DEFAULT: Self = Self {};
}

/// Configuration for the small-allocation refill predictor.
#[derive(Clone, Copy, Debug)]
pub struct RefillPredictorSettings {
    /// Initial predicted refill batch size.
    pub init_batch: u8,
}

impl RefillPredictorSettings {
    pub const DEFAULT: Self = Self {
        init_batch: DEFAULT_BATCH as u8,
    };

    pub const fn new(init_batch: u8) -> Self {
        Self { init_batch }
    }
}

/// Global transparent huge page behavior for allocator-managed mappings.
#[derive(Clone, Copy, Debug)]
pub enum THP {
    /// Allow rsmalloc to request transparent huge pages where supported.
    Enabled,
    /// Disable transparent huge page requests.
    Disabled,
}

impl THP {
    const fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Transparent huge page behavior for buddy allocator regions.
#[derive(Clone, Copy, Debug)]
pub enum BuddyTHP {
    /// Do not force huge-page requests for buddy regions.
    Disabled,
    /// Request huge pages for buddy regions when general THP is enabled.
    Force,
}

impl BuddyTHP {
    const fn enabled(self) -> bool {
        matches!(self, Self::Force)
    }
}

/// Transparent huge page configuration.
#[derive(Clone, Copy, Debug)]
pub struct THPSettings {
    pub thp: THP,
    pub buddy_use_thp: BuddyTHP,
}

impl THPSettings {
    pub const DEFAULT: Self = Self {
        thp: THP::Enabled,
        buddy_use_thp: BuddyTHP::Disabled,
    };

    pub const fn new(thp: THP, buddy_use_thp: BuddyTHP) -> Self {
        Self { thp, buddy_use_thp }
    }
}

/// Magic-value safety mode.
///
/// Magic values are used as lightweight allocation-state checks for detecting
/// corruption, double frees, and invalid frees. The default mode randomizes
/// these values during allocator initialization.
#[derive(Clone, Copy, Debug, Default)]
pub enum MagicSafety {
    /// Randomize magic values at initialization.
    #[default]
    MagicRandomization,

    /// Keep fixed built-in magic values.
    ///
    /// This requires an explicit unsafe acknowledgement because predictable
    /// magic values weaken corruption detection against crafted overwrites.
    FixedMagic(MagicSafetyDisable),

    /// Disable magic checks.
    ///
    /// This requires an explicit unsafe acknowledgement because it weakens
    /// double-free and corruption detection.
    Disabled(DisableMagic),
}

impl MagicSafety {
    const fn is_fixed(&self) -> bool {
        matches!(self, MagicSafety::FixedMagic(_))
    }

    const fn is_disabled(&self) -> bool {
        matches!(self, MagicSafety::Disabled(_))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MagicSafetyDisable {
    _private: (),
}

impl MagicSafetyDisable {
    /// Disables randomized magic values.
    ///
    /// When this token is used, rsmalloc keeps its fixed built-in magic
    /// values instead of randomizing them during allocator initialization.
    ///
    /// # Safety
    /// Predictable magic values weaken allocator corruption detection against
    /// crafted overwrites. Use only for debugging, reproducible tests, or
    /// environments where this tradeoff is explicitly acceptable.
    pub const unsafe fn acknowledge_safety_risk() -> Self {
        Self { _private: () }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DisableMagic {
    _private: (),
}

impl DisableMagic {
    /// Disables magic checks and magic randomization.
    ///
    /// When this token is used, rsmalloc skips magic-value validation during
    /// corruption detection. If a block appears corrupted, or if a double free
    /// is detected through non-magic metadata, the allocator may intentionally
    /// leak that block and continue instead of aborting.
    ///
    /// # Safety
    /// Disabling magic checks removes one of the allocator's normal
    /// double-free and corruption guards. This mode is intended only for
    /// security research, allocator experiments, or tightly controlled
    /// debugging where leak-on-suspicion behavior and weaker corruption
    /// detection are explicitly acceptable.
    pub const unsafe fn acknowledge_safety_risk() -> Self {
        Self { _private: () }
    }
}

/// Behavior for pointers not recognized as rsmalloc-owned
#[derive(Clone, Copy, Debug)]
pub struct ForeignPointerSettings {
    pub global_alloc: ForeignPointerPolicy,
}

impl ForeignPointerSettings {
    pub const fn new(global_alloc: ForeignPointerPolicy) -> Self {
        Self { global_alloc }
    }
}

/// Policy for handling foreign pointers in Rust global-allocator mode.
#[derive(Clone, Copy, Debug)]
pub enum ForeignPointerPolicy {
    /// Ignore unknown pointers and return without freeing them.
    ///
    /// This can be useful during experiments, but silently ignoring invalid
    /// frees can hide bugs.
    Ignore,

    /// Abort when a pointer is not recognized as rsmalloc-owned.
    Abort,
}

impl ForeignPointerPolicy {
    const fn is_abort(&self) -> bool {
        matches!(self, ForeignPointerPolicy::Abort)
    }
}

impl ForeignPointerSettings {
    pub const DEFAULT: Self = Self {
        global_alloc: ForeignPointerPolicy::Abort,
    };

    pub const IGNORE: Self = Self {
        global_alloc: ForeignPointerPolicy::Ignore,
    };
}

/// Buddy allocator region sizing policy.
///
/// This controls the target size of each cached buddy region.
///
/// Smaller regions can reduce lock contention by spreading large allocations
/// across more regions, but may increase memory overhead, stranded space, and
/// worsen some `realloc` growth cases.
///
/// Larger regions can improve packing and make some `realloc` growth cases more
/// likely to stay within the buddy backend, but may increase lock contention
/// under concurrent large-allocation workloads.
#[derive(Clone, Copy, Debug)]
pub enum PerCacheLimit {
    /// Request an explicit cache size in bytes.
    ///
    /// The value is rounded up to a power of two during initialization.
    Bytes(usize),

    /// Use rsmalloc's default cache size.
    Default,
}

impl PerCacheLimit {
    const fn get_size(&self) -> usize {
        match self {
            Self::Bytes(some) => *some,
            Self::Default => 1024 * 1024 * 256,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TrimThread {
    Enabled,
    Disabled,
}

impl TrimThread {
    const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Bytes(pub usize);

impl Bytes {
    pub const TRIM_DEFAULT: Bytes = Bytes(1024 * 1024 * 10);
    pub const ARENA_DEFAULT: Bytes = Bytes(1024 * 1024 * 256);
}

#[derive(Clone, Copy, Debug)]
pub struct TrimThreadSettings {
    pub background_worker: TrimThread,
    pub threshold: Bytes,
}

impl TrimThreadSettings {
    pub const DEFAULT: TrimThreadSettings = TrimThreadSettings {
        background_worker: TrimThread::Enabled,
        threshold: Bytes::TRIM_DEFAULT,
    };
}

#[derive(Clone, Copy, Debug)]
pub enum ReliefState {
    Enabled,
    Disabled,
}

impl ReliefState {
    const fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Percentage(pub usize);

impl Percentage {
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    pub const fn get(self) -> usize {
        self.0
    }
}

/// Applies memory-pressure relief policy.
///
/// When system-wide memory pressure reaches `BUDDY_DISABLE_PERCENTAGE`, the
/// buddy backend is disabled and its cached blocks are trimmed. While disabled,
/// future eligible big allocations bypass the buddy cache and use direct
/// mapping instead.
///
/// The buddy backend is re-enabled only after memory pressure stays at or below
/// `BUDDY_ENABLE_PERCENTAGE` for `ENABLE_AFTER` consecutive samples.
///
/// This path favors avoiding retained-memory OOM behavior over allocation
/// throughput.
#[derive(Clone, Copy, Debug)]
pub struct ReliefSettings {
    pub state: ReliefState,
    pub buddy_disable_percentage: Percentage,
    pub buddy_enable_percentage: Percentage,
}

impl ReliefSettings {
    pub const DEFAULT: ReliefSettings = ReliefSettings {
        state: ReliefState::Disabled,
        buddy_disable_percentage: Percentage::new(85),
        buddy_enable_percentage: Percentage::new(80),
    };

    #[must_use]
    pub const fn new(
        state: ReliefState,
        buddy_disable_percentage: Percentage,
        buddy_enable_percentage: Percentage,
    ) -> ReliefSettings {
        ReliefSettings {
            state,
            buddy_disable_percentage,
            buddy_enable_percentage,
        }
    }
}

// ------------------

/// Runtime configuration for `RSMalloc`.
///
/// `RSMallocConfig` controls allocator behavior for the Rust `GlobalAlloc`
/// integration path. It includes transparent huge page behavior, refill
/// predictor tuning, magic-value safety behavior, foreign-pointer policy,
/// buddy cache sizing, refill retry limits, and experimental feature gates.
///
/// Configuration is applied once during allocator initialization. Changing or
/// constructing additional `RSMalloc` values after initialization does not
/// reconfigure already-initialized global allocator state.
#[must_use]
pub struct RSMallocConfig {
    pub thp_settings: THPSettings,
    pub predictor_settings: RefillPredictorSettings,
    pub magic_safety: MagicSafety,
    pub foreign_pointer: ForeignPointerSettings,
    pub max_refill_retries: u8,
    pub max_per_buddy_cache: PerCacheLimit,
    pub experimental_features: ExperimentalFeatures,
    pub trim_thread: TrimThreadSettings,
    pub relief: ReliefSettings,
    /// Minimum slab page-backend arena data size. The default is 256 MiB;
    /// arena creation page-aligns it and may exceed it for a larger refill.
    pub arena_min_size: Bytes,
}

impl RSMallocConfig {
    /// Useful when you need different foreign pointer settings than the default and want to avoid modifying the default
    pub const fn with_foreign_pointer_policy(policy: ForeignPointerSettings) -> Self {
        Self::DEFAULT.with_foreign_pointer(policy)
    }

    pub const DEFAULT: Self = Self {
        thp_settings: THPSettings::DEFAULT,
        max_refill_retries: 3,
        predictor_settings: RefillPredictorSettings::DEFAULT,
        foreign_pointer: ForeignPointerSettings::DEFAULT,
        trim_thread: TrimThreadSettings {
            background_worker: TrimThread::Disabled,
            threshold: Bytes::TRIM_DEFAULT,
        },
        relief: ReliefSettings::DEFAULT,
        max_per_buddy_cache: PerCacheLimit::Default,
        magic_safety: MagicSafety::MagicRandomization,
        experimental_features: ExperimentalFeatures::DEFAULT,
        arena_min_size: Bytes::ARENA_DEFAULT,
    };

    #[must_use]
    pub const fn with_experimental_features(&self, features: ExperimentalFeatures) -> Self {
        Self {
            experimental_features: features,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_relief_settings(&self, relief: ReliefSettings) -> Self {
        Self { relief, ..*self }
    }

    #[must_use]
    pub const fn with_thp_settings(&self, settings: THPSettings) -> Self {
        Self {
            thp_settings: settings,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_refill_predictor_settings(&self, settings: RefillPredictorSettings) -> Self {
        Self {
            predictor_settings: settings,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_magic_safety(&self, safety: MagicSafety) -> Self {
        Self {
            magic_safety: safety,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_max_refill_retries(&self, retries: u8) -> Self {
        Self {
            max_refill_retries: retries,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_max_per_buddy_cache(&self, cache: PerCacheLimit) -> Self {
        Self {
            max_per_buddy_cache: cache,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_foreign_pointer(&self, policy: ForeignPointerSettings) -> Self {
        Self {
            foreign_pointer: policy,
            ..*self
        }
    }

    #[must_use]
    pub const fn with_trim_thread(&self, thread: TrimThreadSettings) -> Self {
        Self {
            trim_thread: thread,
            ..*self
        }
    }
}

static ONCE: Once = Once::new();

/// rsmalloc Rust global allocator.
///
/// Use this type with Rust's `#[global_allocator]` attribute:
///
/// ```rust
/// use rsmalloc::RSMalloc;
///
/// #[global_allocator]
/// static GLOBAL: RSMalloc = RSMalloc::new_default();
/// ```
///
/// The allocator initializes lazily on first allocation, or eagerly when
/// `manual_init` is called.
pub struct RSMalloc {
    config: RSMallocConfig,
}

#[inline(never)]
unsafe fn init(rs: &RSMalloc) {
    if __rseq_size == 0 || __rseq_offset == 0 {
        RSMallocError::RSEQRegFailed.log_and_abort(
            null_mut(),
            "RSEQ register failed, cannot initialize rseq cache.",
            None,
        );
    }

    #[cfg(feature = "debug")]
    {
        use crate::START_TIME;
        use std::time::Instant;

        START_TIME = Some(Instant::now());
    }

    ARENA_SIZE = rs.config.arena_min_size.0;
    MAX_REFILL_RETRIES = rs.config.max_refill_retries as usize;
    RS_DISABLE_THP = !rs.config.thp_settings.thp.enabled();
    // Buddy initialization performs checked normalization after applying its
    // minimum region size. Keep the raw configuration value here so an
    // overflowing next-power-of-two request cannot wrap before validation.
    BUDDY_MAX_CACHE = rs.config.max_per_buddy_cache.get_size();

    PREDICTOR_INIT_BATCH = rs.config.predictor_settings.init_batch as usize;
    BUDDY_ATTEMPT_HUGE = rs.config.thp_settings.buddy_use_thp.enabled();

    RADIX = RadixTree::new();
    SLAB_CACHE.ensure_cache();
    BUDDY_BACKEND.init(BUDDY_MAX_CACHE, BUDDY_ATTEMPT_HUGE && !RS_DISABLE_THP);

    DISABLE_TRIM_THREAD = rs.config.trim_thread.background_worker.is_disabled();
    TRIM_THRESHOLD = rs.config.trim_thread.threshold.0;
    DISABLE_RELIEF = rs.config.relief.state.is_disabled();
    BUDDY_DISABLE_PERCENTAGE = rs.config.relief.buddy_disable_percentage.0.min(100);
    BUDDY_ENABLE_PERCENTAGE = rs
        .config
        .relief
        .buddy_enable_percentage
        .0
        .min(BUDDY_DISABLE_PERCENTAGE);

    if !rs.config.magic_safety.is_fixed() && !rs.config.magic_safety.is_disabled() {
        init_magic();
        init_align();

        if ALIGN_TAG == 0 {
            while ALIGN_TAG == 0 {
                init_align();
            }
        }
    };

    if rs.config.magic_safety.is_disabled() {
        MAGIC_DISABLE = true;
    }

    if rs.config.foreign_pointer.global_alloc.is_abort() {
        FOREIGN_POINTER_ABORT = true;
    }
}

impl RSMalloc {
    /// Creates a new `RSMalloc` instance with the given configuration.
    pub const fn new_with_config(config: RSMallocConfig) -> Self {
        Self { config }
    }

    /// Creates a new `RSMalloc` instance with the default configuration.
    pub const fn new_default() -> Self {
        Self::new_with_config(RSMallocConfig::DEFAULT)
    }

    #[inline(always)]
    unsafe fn init(&self) {
        ONCE.call_once(|| {
            init(self);
        });

        #[cfg(feature = "debug-printer-thread")]
        crate::debug_printer_thread::start();
    }
}

unsafe impl GlobalAlloc for RSMalloc {
    /// Allocates memory.
    ///
    /// This path is designed to behave like the POSIX-style rsmalloc allocation
    /// path where that does not conflict with Rust's `GlobalAlloc` contract.
    ///
    /// # Safety
    ///
    /// The caller must uphold Rust's `GlobalAlloc::alloc` safety contract for
    /// `layout`.
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.init();
        if likely(layout.align() <= 16) {
            rs_alloc(layout.size(), false).cast_as_ptr()
        } else {
            memalign_inner(layout.align(), layout.size()).cast_as_ptr()
        }
    }

    /// Deallocates memory.
    ///
    /// This path is designed to match the allocator's POSIX-style free behavior
    /// while still being used through Rust's `GlobalAlloc` interface.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated by this allocator with a compatible `layout`.
    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _: Layout) {
        rs_free(UnsafePointer::new(ptr as *mut Header));
    }

    /// Reallocates memory.
    ///
    /// This path is designed to follow POSIX-style realloc behavior where that does
    /// not conflict with Rust's `GlobalAlloc` contract.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated by this allocator with `layout`, and the
    /// caller must uphold Rust's `GlobalAlloc::realloc` safety contract.
    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, _: Layout, new_size: usize) -> *mut u8 {
        self.init();

        rs_realloc(UnsafePointer::new(ptr as *mut Header), new_size).cast_as_ptr()
    }

    /// Allocates zeroed memory.
    ///
    /// This path is designed to match POSIX-style calloc behavior where possible.
    ///
    /// # Safety
    ///
    /// The caller must uphold Rust's `GlobalAlloc::alloc_zeroed` safety contract
    /// for `layout`.
    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.init();
        if likely(layout.align() <= 16) {
            rs_calloc(1, layout.size()).cast_as_ptr()
        } else {
            let ptr = self.alloc(layout);
            if !ptr.is_null() {
                write_bytes(ptr, 0, layout.size());
            }
            ptr
        }
    }
}

// --------------------------

#[derive(Clone, Copy, Debug)]
pub enum RSMallocTrim {
    All,
    Request(usize),
}

impl RSMallocTrim {
    #[inline(always)]
    const fn get_request_size(&self) -> usize {
        match self {
            RSMallocTrim::All => 0,
            RSMallocTrim::Request(size) => *size,
        }
    }
}

#[must_use]
pub enum RSTrimStatus {
    Disabled,
    Trimmed(usize),
    NothingToTrim,
}

impl RSTrimStatus {
    #[inline(always)]
    pub const fn get_trim_size(&self) -> usize {
        match self {
            RSTrimStatus::Disabled => 0,
            RSTrimStatus::NothingToTrim => 0,
            RSTrimStatus::Trimmed(size) => *size,
        }
    }

    pub const fn succeeded(&self) -> bool {
        matches!(self, RSTrimStatus::Trimmed(_))
    }

    pub const fn disabled(&self) -> bool {
        matches!(self, RSTrimStatus::Disabled)
    }
}

pub const RSMALLOC_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Snapshot of allocator capability/configuration information.
#[derive(Clone, Copy, Debug)]
pub struct RSMallocCapabilities {
    /// Whether general THP support is enabled in this allocator's config.
    pub thp_enabled: bool,
    /// Whether NUMA-aware allocator behavior is currently available.
    ///
    /// This is currently always `Partial`.
    pub numa_support: NumaSupport,
    /// rsmalloc crate version.
    pub version: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub enum NumaSupport {
    Unsupported,
    Partial,
    Supported,
}

impl NumaSupport {
    pub fn is_unsupported(&self) -> bool {
        matches!(self, NumaSupport::Unsupported)
    }

    pub fn is_partial(&self) -> bool {
        matches!(self, NumaSupport::Partial)
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, NumaSupport::Supported)
    }
}

impl RSMalloc {
    pub fn get_capabilities(&self) -> RSMallocCapabilities {
        RSMallocCapabilities {
            thp_enabled: self.config.thp_settings.thp.enabled(),
            numa_support: NumaSupport::Partial,
            version: RSMALLOC_VERSION,
        }
    }

    /// Allocates memory through rsmalloc's Rust-facing malloc-style API.
    ///
    /// This is a low-level helper for tests, allocator experiments, and code that
    /// intentionally wants rsmalloc's raw allocation semantics without going
    /// through C symbol interposition.
    ///
    /// # Safety
    ///
    /// The returned pointer must be freed with [`RSMalloc::rs_free`] or
    /// reallocated with [`RSMalloc::rs_realloc`]. Mixing this API with another
    /// allocator is undefined allocator behavior and may abort.
    #[inline]
    pub unsafe fn rs_malloc(&self, size: usize) -> *mut u8 {
        self.init();
        rs_alloc(size, false).cast_as_ptr()
    }

    /// Allocates zeroed memory through rsmalloc's Rust-facing calloc-style API.
    ///
    /// The argument order matches C `calloc(nmemb, size)`.
    ///
    /// # Safety
    ///
    /// The returned pointer must be freed with [`RSMalloc::rs_free`] or
    /// reallocated with [`RSMalloc::rs_realloc`]. Mixing this API with another
    /// allocator is undefined allocator behavior and may abort.
    #[inline]
    pub unsafe fn rs_calloc(&self, nmemb: usize, size: usize) -> *mut u8 {
        self.init();
        rs_calloc(size, nmemb).cast_as_ptr()
    }

    /// Allocates aligned memory through rsmalloc's Rust-facing memalign-style API.
    ///
    /// `alignment` must be a power of two. Alignments smaller than 8 are rounded
    /// up to 8 by the allocator.
    ///
    /// # Safety
    ///
    /// The returned pointer must be freed with [`RSMalloc::rs_free`] or
    /// reallocated with [`RSMalloc::rs_realloc`]. Mixing this API with another
    /// allocator is undefined allocator behavior and may abort.
    #[inline]
    pub unsafe fn rs_memalign(&self, alignment: usize, size: usize) -> *mut u8 {
        self.init();
        memalign_inner(alignment, size).cast_as_ptr()
    }

    /// Reallocates memory through rsmalloc's Rust-facing realloc-style API.
    ///
    /// # Safety
    ///
    /// `ptr` must be null or a pointer returned by `RSMalloc` that has not already
    /// been freed. On failure, this returns null and leaves the original pointer
    /// allocated, matching realloc-style semantics.
    #[inline]
    pub unsafe fn rs_realloc(&self, ptr: *mut u8, new_size: usize) -> *mut u8 {
        self.init();
        rs_realloc(UnsafePointer::new(ptr as *mut Header), new_size).cast_as_ptr()
    }

    /// Frees memory allocated by rsmalloc's Rust-facing malloc-style API.
    ///
    /// # Safety
    ///
    /// `ptr` must be null or a pointer returned by `RSMalloc` that has not already
    /// been freed. Passing foreign pointers, interior pointers, or double-freeing
    /// may abort or corrupt allocator state depending on configuration.
    #[inline]
    pub unsafe fn rs_free(&self, ptr: *mut u8) {
        rs_free(UnsafePointer::new(ptr as *mut Header));
    }

    /// Advises the kernel that cold free pages may be reclaimed.
    ///
    /// Trimming is performed in two stages:
    /// - the buddy allocator trims eligible free large blocks,
    /// - small-allocation caches are scanned for aged free blocks and their
    ///   releasable pages are advised back to the kernel.
    ///
    /// `RSMallocTrim::Request(bytes)` attempts to reclaim approximately the
    /// requested number of bytes. `RSMallocTrim::All` scans all eligible free
    /// memory.
    ///
    /// Depending on enabled features, physical pages are released with either
    /// `MADV_FREE` (default) or `MADV_DONTNEED` (`force-physical-trim`).
    ///
    /// Returns the number of bytes successfully advised to the kernel.
    #[inline]
    pub unsafe fn rs_trim(&self, trim: RSMallocTrim) -> RSTrimStatus {
        self.init();

        let requested = trim.get_request_size();
        let size = BUDDY_BACKEND.trim(requested);
        if size < requested && requested != 0 {
            let small = trim_small(requested.saturating_sub(size));
            if small > 0 {
                return RSTrimStatus::Trimmed(size + small);
            }
        }

        RSTrimStatus::Trimmed(size)
    }

    /// Returns the usable payload size for an rsmalloc allocation.
    ///
    /// Returns `Size::NotRS` if the pointer is not recognized as rsmalloc-owned.
    ///
    /// # Safety
    ///
    /// `ptr` must be either null or a pointer previously returned by this allocator.
    /// Passing arbitrary pointers, interior pointers, or pointers from another
    /// allocator may cause incorrect results or allocator aborts depending on the
    /// configured foreign-pointer behavior.
    #[inline]
    pub unsafe fn rs_usable_size(&self, ptr: *mut u8) -> Size {
        let size = usable_size(UnsafePointer::new(ptr as *mut Header));
        if size == 0 {
            Size::NotRS
        } else {
            Size::RS(size)
        }
    }

    /// Initializes the allocator manually.
    ///
    /// You can ignore this, rsmalloc will automatically initialize itself on first use.
    ///
    /// # Safety
    /// This is safe to call multiple times, but only the first call will have any effect.
    pub unsafe fn manual_init(&self) {
        self.init();
    }

    #[cfg(feature = "debug")]
    pub fn get_stats(&self) -> RSMallocStats {
        use crate::{
            ABORTS, BUDDY_AVERAGE_BLOCK_TIMES, HIGH_WATER_BUDDY_CACHED_VA,
            HIGH_WATER_SLAB_CACHED_VA, HIGH_WATER_TOTAL_CACHED_VA, NCPU, REFILL_OVER_PREDICTS,
            REFILL_UNDER_PREDICTS, REFILLS_BY_CLASS, TOTAL_CACHED_VA, TOTAL_REFILL_CALLS,
            big_allocations::buddy::{BIG_BUDDY_MIN_ORDER, BUDDY_BACKEND, BUDDY_TOTAL_CACHED_VA},
            internals::l3_main_radix::{CHUNK_SIZE, RADIX},
            rseq_core::slab_cache::SLAB_CACHE,
            trim::{DISABLE_BUDDY, TOTAL_TRIM_CALLS, TOTAL_TRIMMED_VA},
            utility::{NUM_SIZE_CLASSES, SIZE_CLASSES},
        };
        use std::sync::atomic::Ordering::{self, Relaxed};

        unsafe { self.init() };

        let under = REFILL_UNDER_PREDICTS.load(Ordering::Relaxed);
        let over = REFILL_OVER_PREDICTS.load(Ordering::Relaxed);
        let misses = under.saturating_add(over);
        let total = TOTAL_REFILL_CALLS.load(Ordering::Relaxed);
        let percentage = if total == 0 {
            0.0
        } else {
            (misses as f64 / total as f64) * 100.0
        };
        let success_rates = 100.0 - percentage;
        let aborts = ABORTS.load(Ordering::Relaxed);

        let slab_cached_va = TOTAL_CACHED_VA.load(Relaxed);
        let buddy_cached_va = BUDDY_TOTAL_CACHED_VA.load(Relaxed);
        let total_cached_va = slab_cached_va.saturating_add(buddy_cached_va);

        let mut rseq_cpu_total_cached_bytes = 0usize;
        let mut rseq_cpu_min_cached_bytes = usize::MAX;
        let mut rseq_cpu_max_cached_bytes = 0usize;
        let mut rseq_cpu_nonempty = 0usize;
        let cpu_limit = unsafe { NCPU };
        let rseq_cpu_buffer = unsafe { alloc_usize_array(cpu_limit) };
        let rseq_cpu_cached_bytes = rseq_cpu_buffer.cast_as_ptr() as *mut usize;
        for cpu in 0..cpu_limit {
            let bytes = unsafe { SLAB_CACHE.get_rseq_cpu_usage_bytes(cpu) };
            if !rseq_cpu_cached_bytes.is_null() {
                unsafe { *rseq_cpu_cached_bytes.add(cpu) = bytes };
            }
            rseq_cpu_total_cached_bytes = rseq_cpu_total_cached_bytes.saturating_add(bytes);
            rseq_cpu_min_cached_bytes = rseq_cpu_min_cached_bytes.min(bytes);
            rseq_cpu_max_cached_bytes = rseq_cpu_max_cached_bytes.max(bytes);
            if bytes != 0 {
                rseq_cpu_nonempty += 1;
            }
        }
        if cpu_limit == 0 {
            rseq_cpu_min_cached_bytes = 0;
        }

        let mut refills_by_class = [0usize; NUM_SIZE_CLASSES];
        let mut class_cached_bytes = [0usize; NUM_SIZE_CLASSES];
        let mut class_active_cpus = [0usize; NUM_SIZE_CLASSES];
        let mut class_min_cached_bytes = [0usize; NUM_SIZE_CLASSES];
        let mut class_max_cached_bytes = [0usize; NUM_SIZE_CLASSES];
        let mut class_avg_cached_bytes = [0usize; NUM_SIZE_CLASSES];

        for class in 0..NUM_SIZE_CLASSES {
            refills_by_class[class] = REFILLS_BY_CLASS[class].load(Relaxed);
            let mut min = usize::MAX;
            let mut max = 0usize;
            let mut active = 0usize;
            let mut total_cached = 0usize;

            for cpu in 0..cpu_limit {
                let bytes = unsafe { SLAB_CACHE.get_rseq_cpu_class_usage_bytes(cpu, class) };
                total_cached = total_cached.saturating_add(bytes);
                if bytes != 0 {
                    active += 1;
                    min = min.min(bytes);
                    max = max.max(bytes);
                }
            }

            class_cached_bytes[class] = total_cached;
            class_active_cpus[class] = active;
            class_min_cached_bytes[class] = if active == 0 { 0 } else { min };
            class_max_cached_bytes[class] = max;
            class_avg_cached_bytes[class] = if active == 0 {
                0
            } else {
                total_cached / active
            };
        }

        let (numa, inner) = unsafe { SLAB_CACHE.get_numa_and_inner() };
        let buddy = unsafe { BUDDY_BACKEND.report() };
        let radix = unsafe { RADIX.report() };
        let buddy_used_bytes = buddy.total_region_bytes.saturating_sub(buddy.free_bytes);
        let buddy_free_blocks = buddy.free_blocks.iter().sum();
        let buddy_never_allocated_bytes = buddy
            .never_allocated_by_order
            .iter()
            .enumerate()
            .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
            .sum();
        let buddy_reused_bytes = buddy
            .reused_by_order
            .iter()
            .enumerate()
            .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
            .sum();
        let buddy_trimmed_bytes = buddy
            .trimmed_by_order
            .iter()
            .enumerate()
            .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
            .sum();
        let radix_owned_bytes = radix.owned_chunks.saturating_mul(CHUNK_SIZE);
        let radix_metadata_per_chunk = if radix.owned_chunks == 0 {
            0.0
        } else {
            radix.metadata_bytes as f64 / radix.owned_chunks as f64
        };

        RSMallocStats {
            total_refills: total,
            refill_under_predicts: under,
            refill_over_predicts: over,
            total_misses: misses,
            miss_percentage: percentage,
            success_rates,
            rseq_aborts: aborts,
            total_cached_va,
            slab_cached_va,
            buddy_cached_va,
            high_water_slab_cached_va: HIGH_WATER_SLAB_CACHED_VA.load(Relaxed),
            high_water_buddy_cached_va: HIGH_WATER_BUDDY_CACHED_VA.load(Relaxed),
            high_water_total_cached_va: HIGH_WATER_TOTAL_CACHED_VA.load(Relaxed),
            numa_enabled: inner.is_numa,
            numa_cpus: numa.ncpu,
            numa_nodes: numa.nnodes,
            numa_ranges: numa.nranges,
            rseq_cpu_count: cpu_limit,
            rseq_cpu_cached_bytes,
            rseq_cpu_buffer,
            rseq_cpu_total_cached_bytes,
            rseq_cpu_min_cached_bytes,
            rseq_cpu_max_cached_bytes,
            rseq_cpu_nonempty,
            size_classes: SIZE_CLASSES,
            refills_by_class,
            class_cached_bytes,
            class_active_cpus,
            class_min_cached_bytes,
            class_max_cached_bytes,
            class_avg_cached_bytes,
            trim_calls: TOTAL_TRIM_CALLS.load(Relaxed),
            trimmed_va: TOTAL_TRIMMED_VA.load(Relaxed),
            avg_small_life_ms: crate::AVERAGE_BLOCK_TIMES.load(Relaxed),
            avg_buddy_life_ms: BUDDY_AVERAGE_BLOCK_TIMES.load(Relaxed),
            buddy_disabled: DISABLE_BUDDY.load(Relaxed),
            buddy_regions: buddy.regions,
            buddy_total_region_bytes: buddy.total_region_bytes,
            buddy_used_bytes,
            buddy_free_bytes: buddy.free_bytes,
            buddy_free_blocks,
            buddy_never_allocated_blocks: buddy.never_allocated_blocks,
            buddy_reused_blocks: buddy.reused_blocks,
            buddy_trimmed_blocks: buddy.trimmed_blocks,
            buddy_never_allocated_bytes,
            buddy_reused_bytes,
            buddy_trimmed_bytes,
            buddy_free_blocks_by_order: buddy.free_blocks,
            buddy_never_allocated_by_order: buddy.never_allocated_by_order,
            buddy_reused_by_order: buddy.reused_by_order,
            buddy_trimmed_by_order: buddy.trimmed_by_order,
            buddy_grow_order: buddy.grow_order,
            buddy_thp: buddy.thp,
            radix_l1_nodes: radix.l1_nodes,
            radix_l2_nodes: radix.l2_nodes,
            radix_leaves: radix.leaves,
            radix_owned_chunks: radix.owned_chunks,
            radix_chunk_size: CHUNK_SIZE,
            radix_owned_bytes,
            radix_metadata_bytes: radix.metadata_bytes,
            radix_metadata_per_chunk,
        }
    }

    #[cfg(feature = "debug-exact")]
    pub fn get_exact_stats(&self) -> RSMallocExactStats {
        #[cfg(feature = "transfer-debug-exact")]
        use crate::{
            DRY_TRANSFER_STEALS, TOTAL_TRANSFER_POP_CALLS, TOTAL_TRANSFER_PUSH_CALLS,
            TOTAL_TRANSFER_RETRIES, TOTAL_TRANSFER_STEALS,
        };
        use crate::{
            GLOBAL_LOCK_RETRIES, GLOBAL_LOCKS, GLOBAL_SPIN_WAITS, GLOBAL_TRY_LOCK_MISSES,
            GLOBAL_TRY_LOCKS,
        };
        use std::sync::atomic::Ordering::Relaxed;

        let stats = self.get_stats();
        let total_locks = GLOBAL_LOCKS.load(Relaxed);
        let total_lock_retries = GLOBAL_LOCK_RETRIES.load(Relaxed);
        let aborts_vs_locks = if total_locks == 0 {
            0.0
        } else {
            stats.rseq_aborts as f64 / total_locks as f64
        };

        RSMallocExactStats {
            under_vs_over: stats
                .refill_under_predicts
                .saturating_sub(stats.refill_over_predicts),
            over_vs_under: stats
                .refill_over_predicts
                .saturating_sub(stats.refill_under_predicts),
            total_locks,
            total_lock_retries,
            total_try_locks: GLOBAL_TRY_LOCKS.load(Relaxed),
            total_try_lock_misses: GLOBAL_TRY_LOCK_MISSES.load(Relaxed),
            total_spin_waits: GLOBAL_SPIN_WAITS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_pop_calls: TOTAL_TRANSFER_POP_CALLS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_push_calls: TOTAL_TRANSFER_PUSH_CALLS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_steals: TOTAL_TRANSFER_STEALS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_retries: TOTAL_TRANSFER_RETRIES.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            dry_transfer_steals: DRY_TRANSFER_STEALS.load(Relaxed),
            aborts_vs_locks,
            capabilities: self.get_capabilities(),
            stats,
        }
    }
}

pub const RSMALLOC_BUDDY_NUM_ORDERS: usize = 5;

#[cfg(any(feature = "debug", doc))]
unsafe fn alloc_usize_array(count: usize) -> UnsafePointer<Header> {
    let Some(bytes) = count.checked_mul(core::mem::size_of::<usize>()) else {
        return UnsafePointer::NULL;
    };

    rs_alloc(bytes, false)
}

#[cfg(any(feature = "debug", doc))]
/// rsmalloc structured debug stats.
pub struct RSMallocStats {
    pub total_refills: usize,
    pub refill_under_predicts: usize,
    pub refill_over_predicts: usize,
    pub total_misses: usize,
    pub success_rates: f64,
    pub miss_percentage: f64,
    pub rseq_aborts: usize,

    pub total_cached_va: usize,
    pub slab_cached_va: usize,
    pub buddy_cached_va: usize,
    pub high_water_slab_cached_va: usize,
    pub high_water_buddy_cached_va: usize,
    pub high_water_total_cached_va: usize,

    pub numa_enabled: bool,
    pub numa_cpus: usize,
    pub numa_nodes: usize,
    pub numa_ranges: usize,

    pub rseq_cpu_count: usize,
    pub rseq_cpu_cached_bytes: *mut usize,
    rseq_cpu_buffer: UnsafePointer<Header>,
    pub rseq_cpu_total_cached_bytes: usize,
    pub rseq_cpu_min_cached_bytes: usize,
    pub rseq_cpu_max_cached_bytes: usize,
    pub rseq_cpu_nonempty: usize,

    pub size_classes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub refills_by_class: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_active_cpus: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_min_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_max_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_avg_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],

    pub trim_calls: usize,
    pub trimmed_va: usize,
    pub avg_small_life_ms: u32,
    pub avg_buddy_life_ms: u32,
    pub buddy_disabled: bool,

    pub buddy_regions: usize,
    pub buddy_total_region_bytes: usize,
    pub buddy_used_bytes: usize,
    pub buddy_free_bytes: usize,
    pub buddy_free_blocks: usize,
    pub buddy_never_allocated_blocks: usize,
    pub buddy_reused_blocks: usize,
    pub buddy_trimmed_blocks: usize,
    pub buddy_never_allocated_bytes: usize,
    pub buddy_reused_bytes: usize,
    pub buddy_trimmed_bytes: usize,
    pub buddy_free_blocks_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_never_allocated_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_reused_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_trimmed_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_grow_order: usize,
    pub buddy_thp: bool,

    pub radix_l1_nodes: usize,
    pub radix_l2_nodes: usize,
    pub radix_leaves: usize,
    pub radix_owned_chunks: usize,
    pub radix_chunk_size: usize,
    pub radix_owned_bytes: usize,
    pub radix_metadata_bytes: usize,
    pub radix_metadata_per_chunk: f64,
}

#[cfg(any(feature = "debug", doc))]
impl Drop for RSMallocStats {
    fn drop(&mut self) {
        if self.rseq_cpu_cached_bytes.is_null() || self.rseq_cpu_count == 0 {
            return;
        }

        unsafe { rs_free(UnsafePointer::new(self.rseq_cpu_buffer.as_ptr())) };
    }
}

#[cfg(any(feature = "debug-exact", doc))]
/// rsmalloc structured exact debug stats.
pub struct RSMallocExactStats {
    pub stats: RSMallocStats,
    pub total_locks: usize,
    pub total_lock_retries: usize,
    pub total_try_locks: usize,
    pub total_try_lock_misses: usize,
    pub total_spin_waits: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_pop_calls: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_push_calls: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_steals: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_retries: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub dry_transfer_steals: usize,
    pub aborts_vs_locks: f64,
    pub under_vs_over: usize,
    pub over_vs_under: usize,
    pub capabilities: RSMallocCapabilities,
}
