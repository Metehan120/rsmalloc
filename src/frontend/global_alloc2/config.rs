//! Configuration types for the v2 Rust global allocator.
//!
//! [`Config`] separates ordinary performance and memory-retention tuning from
//! settings that weaken allocator safety. The latter are unavailable unless
//! the `expose-security-critical-settings` Cargo feature is enabled.

#[cfg(not(feature = "preload"))]
use crate::backend::page_allocator::MIN_ARENA_SIZE;
use crate::{backend::bootstrap::BootstrapConfig, core_prim::predictor::DEFAULT_BATCH};

const ARENA_ALIGNMENT: usize = 4096;

const DEFAULT_SEGMENTED_BITMAP_CACHE: usize = 64 * 1024 * 1024;
const DEFAULT_SMALL_TRIM_THRESHOLD: usize = 10 * 1024 * 1024;
const DEFAULT_BIG_TRIM_THRESHOLD: usize = 512 * 1024 * 1024;

/// Transparent huge-page behavior for allocator-managed mappings.
#[derive(Clone, Copy, Debug)]
pub enum THP {
    /// Allow rsmalloc to request transparent huge pages where supported.
    Enabled,
    /// Prevent rsmalloc from requesting transparent huge pages.
    Disabled,
}

impl THP {
    const fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// Transparent huge-page behavior for segmented-bitmap regions.
#[derive(Clone, Copy, Debug)]
pub enum SegmentedBitmapTHP {
    /// Do not explicitly request huge pages for segmented-bitmap regions.
    Disabled,
    /// Request huge pages when global THP support is enabled.
    Force,
}

impl SegmentedBitmapTHP {
    const fn enabled(self) -> bool {
        matches!(self, Self::Force)
    }
}

/// Transparent huge-page settings for allocator mappings and segmented-bitmap regions.
#[derive(Clone, Copy, Debug)]
pub struct THPSettings {
    /// Global transparent huge-page policy.
    pub thp: THP,
    /// Whether segmented-bitmap regions should explicitly request huge pages.
    ///
    /// [`SegmentedBitmapTHP::Force`] has no effect while [`THP::Disabled`] is selected.
    pub segmented_bitmap_use_thp: SegmentedBitmapTHP,
}

impl THPSettings {
    /// Default policy: enable general THP support without forcing it for
    /// segmented-bitmap regions.
    pub const DEFAULT: Self = Self {
        thp: THP::Enabled,
        segmented_bitmap_use_thp: SegmentedBitmapTHP::Disabled,
    };

    /// Creates a transparent huge-page configuration.
    pub const fn new(thp: THP, segmented_bitmap_use_thp: SegmentedBitmapTHP) -> Self {
        Self {
            thp,
            segmented_bitmap_use_thp,
        }
    }
}

/// A configuration size expressed in bytes or binary units.
///
/// Used for trim thresholds, initial segmented-bitmap region sizing, and
/// minimum slab arena sizing. KiB, MiB, and GiB mean powers of 1024, not 1000.
/// Constructors preserve the supplied unit; conversion to bytes happens when
/// the setting is validated or applied.
///
/// The caller must ensure that the resulting byte count fits in `usize`.
/// Unit conversion uses ordinary multiplication and does not check overflow.
/// Individual settings may impose additional constraints, such as
/// [`ArenaSize`]'s 4 KiB granularity.
///
/// ```rust
/// use rsmalloc::v2::config::{ArenaSize, PerCacheLimit, Size};
///
/// let initial_region = PerCacheLimit::Custom(Size::mib(64));
/// let arena = ArenaSize::new(Size::mib(256));
/// assert!(arena.is_some());
/// ```
#[derive(Clone, Copy, Debug)]
pub enum Size {
    /// A byte count.
    Bytes(usize),
    /// A kibibyte count: each KiB is 1024 bytes.
    KiB(usize),
    /// A mebibyte count: each MiB is 1024 KiB.
    MiB(usize),
    /// A gibibyte count: each GiB is 1024 MiB.
    GiB(usize),
}

impl Size {
    /// Default small-allocation threshold for waking the background trimmer: 10 MiB.
    pub const SMALL_TRIM_DEFAULT: Self = Self::Bytes(DEFAULT_SMALL_TRIM_THRESHOLD);
    /// Default big-allocation threshold for waking the background trimmer: 512 MiB.
    pub const BIG_TRIM_DEFAULT: Self = Self::Bytes(DEFAULT_BIG_TRIM_THRESHOLD);

    /// Creates a size expressed in bytes.
    pub const fn bytes(value: usize) -> Self {
        Self::Bytes(value)
    }

    /// Creates a size expressed in KiB (1024 bytes each).
    ///
    /// The caller must ensure that `value * 1024` fits in `usize`.
    pub const fn kib(value: usize) -> Self {
        Self::KiB(value)
    }

    /// Creates a size expressed in MiB (1024 KiB each).
    ///
    /// The caller must ensure that `value * 1024 * 1024` fits in `usize`.
    pub const fn mib(value: usize) -> Self {
        Self::MiB(value)
    }

    /// Creates a size expressed in GiB (1024 MiB each).
    ///
    /// The caller must ensure that `value * 1024 * 1024 * 1024` fits in `usize`.
    pub const fn gib(value: usize) -> Self {
        Self::GiB(value)
    }

    const fn get(self) -> usize {
        match self {
            Self::Bytes(value) => value,
            Self::KiB(value) => value * 1024,
            Self::MiB(value) => value * 1024 * 1024,
            Self::GiB(value) => value * 1024 * 1024 * 1024,
        }
    }
}

/// Initial segmented-bitmap region size.
///
/// The backend rounds this to at least 64 MiB and a power of two. Later
/// growth adds 64 MiB regions.
#[derive(Clone, Copy, Debug)]
pub enum PerCacheLimit {
    /// Requested initial region bytes. The backend normalizes the value.
    Custom(Size),
    /// Use rsmalloc's 64 MiB default.
    Default,
}

impl PerCacheLimit {
    const fn bytes(self) -> usize {
        match self {
            Self::Custom(size) => size.get(),
            Self::Default => DEFAULT_SEGMENTED_BITMAP_CACHE,
        }
    }
}

/// Minimum size requested for a slab page-backend arena.
///
/// Arena sizes must be multiples of 4 KiB. Initialization enforces a 256 KiB
/// minimum. An arena may be larger when an individual backend request exceeds
/// this configured minimum.
#[derive(Clone, Copy, Debug)]
pub struct ArenaSize(Size);

impl ArenaSize {
    /// The default minimum arena size: 256 MiB.
    pub const DEFAULT: Self = Self(Size::MiB(256));

    /// Creates an arena-size setting when `size` is a multiple of 4 KiB.
    ///
    /// Returns [`None`] when the requested size does not satisfy the v2 arena
    /// configuration alignment policy. Zero is accepted; initialization enforces
    /// the 256 KiB minimum.
    #[must_use]
    pub const fn new(size: Size) -> Option<ArenaSize> {
        if !size.get().is_multiple_of(ARENA_ALIGNMENT) {
            return None;
        }

        Some(ArenaSize(size))
    }
}

/// Background trimming-worker state.
#[derive(Clone, Copy, Debug)]
pub enum TrimThread {
    /// Run the background trimming worker.
    Enabled,
    /// Disable background trimming; explicit trim calls remain available.
    Disabled,
}

impl TrimThread {
    const fn disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// Background trimming configuration.
#[derive(Clone, Copy, Debug)]
pub struct TrimSettings {
    /// Whether the background trimming worker runs.
    pub background_worker: TrimThread,
    /// Cached small-allocation bytes required to trigger background trimming.
    pub small_threshold: Size,
    /// Cached big-allocation bytes required to trigger background trimming.
    pub big_threshold: Size,
}

impl TrimSettings {
    /// Default trimming configuration: worker enabled with 10 MiB small and
    /// 512 MiB big-allocation thresholds.
    pub const DEFAULT: Self = Self {
        background_worker: TrimThread::Enabled,
        small_threshold: Size::SMALL_TRIM_DEFAULT,
        big_threshold: Size::BIG_TRIM_DEFAULT,
    };

    /// Creates background trimming settings.
    pub const fn new(
        background_worker: TrimThread,
        small_threshold: Size,
        big_threshold: Size,
    ) -> Self {
        Self {
            background_worker,
            small_threshold,
            big_threshold,
        }
    }
}

/// Segmented-bitmap memory-pressure relief state.
#[derive(Clone, Copy, Debug)]
pub enum ReliefState {
    /// Allow the allocator to disable segmented-bitmap allocation under memory pressure.
    Enabled,
    /// Keep segmented-bitmap allocation enabled regardless of the relief thresholds.
    Disabled,
}

impl ReliefState {
    const fn disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// A percentage clamped to the inclusive range `0..=100`.
#[derive(Clone, Copy, Debug)]
pub struct Percentage(usize);

impl Percentage {
    /// Creates a percentage, clamped to the inclusive `0..=100` range.
    pub const fn new(value: usize) -> Self {
        Self(if value > 100 { 100 } else { value })
    }

    /// Returns the normalized percentage.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Memory-pressure relief settings for the segmented-bitmap backend.
///
/// When enabled, segmented-bitmap allocation is disabled at the disable threshold and is
/// re-enabled after pressure falls to the enable threshold. If the configured
/// enable threshold exceeds the disable threshold, initialization lowers it to
/// the disable threshold.
#[derive(Clone, Copy, Debug)]
pub struct ReliefSettings {
    /// Whether memory-pressure relief is active.
    pub state: ReliefState,
    /// Pressure percentage at which segmented-bitmap allocation is disabled.
    pub segmented_bitmap_disable_percentage: Percentage,
    /// Pressure percentage at or below which segmented-bitmap allocation may be re-enabled.
    pub segmented_bitmap_enable_percentage: Percentage,
}

impl ReliefSettings {
    /// Default thresholds are 85% for disabling and 80% for re-enabling.
    /// Relief itself is disabled by default.
    pub const DEFAULT: Self = Self {
        state: ReliefState::Disabled,
        segmented_bitmap_disable_percentage: Percentage::new(85),
        segmented_bitmap_enable_percentage: Percentage::new(80),
    };

    /// Creates segmented-bitmap memory-pressure relief settings.
    pub const fn new(
        state: ReliefState,
        segmented_bitmap_disable_percentage: Percentage,
        segmented_bitmap_enable_percentage: Percentage,
    ) -> Self {
        Self {
            state,
            segmented_bitmap_disable_percentage,
            segmented_bitmap_enable_percentage,
        }
    }
}

/// Performance and memory-retention tuning.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Transparent huge-page policy.
    pub thp: THPSettings,
    /// Initial batch prediction used when refilling small-allocation caches.
    pub refill_init_batch: u8,
    /// Maximum number of small-cache refill retries.
    pub max_refill_retries: u8,
    /// Initial segmented-bitmap region size; later growth uses 64 MiB regions.
    pub max_per_segmented_bitmap_cache: PerCacheLimit,
    /// Background trimming policy.
    pub trim: TrimSettings,
    /// Segmented-bitmap memory-pressure relief policy.
    pub relief: ReliefSettings,
    /// Minimum slab page-backend arena data size.
    ///
    /// Initialization enforces an absolute minimum of 256 KiB. The default is
    /// 256 MiB.
    pub arena_min_size: ArenaSize,
}

impl Tuning {
    /// Default allocator tuning.
    pub const DEFAULT: Self = Self {
        thp: THPSettings::DEFAULT,
        refill_init_batch: DEFAULT_BATCH as u8,
        max_refill_retries: 3,
        max_per_segmented_bitmap_cache: PerCacheLimit::Default,
        trim: TrimSettings::DEFAULT,
        relief: ReliefSettings::DEFAULT,
        arena_min_size: ArenaSize::DEFAULT,
    };

    /// Replaces the transparent huge-page settings.
    #[must_use]
    pub const fn with_thp(self, thp: THPSettings) -> Self {
        Self { thp, ..self }
    }

    /// Replaces the initial small-cache refill prediction.
    #[must_use]
    pub const fn with_refill_init_batch(self, refill_init_batch: u8) -> Self {
        Self {
            refill_init_batch,
            ..self
        }
    }

    /// Replaces the maximum number of refill retries.
    #[must_use]
    pub const fn with_max_refill_retries(self, max_refill_retries: u8) -> Self {
        Self {
            max_refill_retries,
            ..self
        }
    }

    /// Replaces the initial segmented-bitmap region size.
    #[must_use]
    pub const fn with_max_per_segmented_bitmap_cache(
        self,
        max_per_segmented_bitmap_cache: PerCacheLimit,
    ) -> Self {
        Self {
            max_per_segmented_bitmap_cache,
            ..self
        }
    }

    /// Replaces the background trimming settings.
    #[must_use]
    pub const fn with_trim(self, trim: TrimSettings) -> Self {
        Self { trim, ..self }
    }

    /// Replaces the segmented-bitmap memory-pressure relief settings.
    #[must_use]
    pub const fn with_relief(self, relief: ReliefSettings) -> Self {
        Self { relief, ..self }
    }

    /// Replaces the minimum slab arena size.
    #[must_use]
    pub const fn with_arena_min_size(self, arena_min_size: ArenaSize) -> Self {
        Self {
            arena_min_size,
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SecurityState {
    randomize_magic: bool,
    abort_on_foreign_pointer: bool,
}

impl SecurityState {
    const DEFAULT: Self = Self {
        randomize_magic: true,
        abort_on_foreign_pointer: true,
    };
}

#[cfg(any(feature = "expose-security-critical-settings", doc))]
/// Magic-value randomization policy.
///
/// This API is available only with `expose-security-critical-settings`.
#[derive(Clone, Copy, Debug)]
pub enum MagicSafety {
    /// Randomize allocator magic values during initialization.
    Randomized,
    /// Keep the built-in magic values after acknowledging the safety tradeoff.
    Fixed(MagicSafetyDisable),
}

#[cfg(any(feature = "expose-security-critical-settings", doc))]
/// Proof that the caller explicitly accepted the risk of fixed magic values.
#[derive(Clone, Copy, Debug)]
pub struct MagicSafetyDisable {
    _private: (),
}

#[cfg(any(feature = "expose-security-critical-settings", doc))]
impl MagicSafetyDisable {
    /// Acknowledges that predictable magic values weaken corruption detection.
    ///
    /// # Safety
    /// Only use fixed magic values in controlled debugging or test environments.
    pub const unsafe fn acknowledge_safety_risk() -> Self {
        Self { _private: () }
    }
}

#[cfg(any(feature = "expose-security-critical-settings", doc))]
/// Policy for pointers detected as not owned by rsmalloc.
///
/// Free reads metadata first unless `validate-foreign-first-on-free` is enabled.
/// Without that feature, this policy is best-effort and may not be reached before
/// a fault or misclassification. Coarse validation does not permit invalid frees.
#[derive(Clone, Copy, Debug)]
pub enum ForeignPointerPolicy {
    /// Abort instead of silently accepting an invalid deallocation request.
    Abort,
    /// Ignore the pointer and return without freeing it.
    ///
    /// This can hide allocator mismatches and invalid frees.
    Ignore,
}

/// Security-sensitive settings, available only with
/// `expose-security-critical-settings`.
#[cfg(any(feature = "expose-security-critical-settings", doc))]
#[derive(Clone, Copy, Debug)]
pub struct SecurityCritical {
    /// Magic-value randomization policy.
    pub magic: MagicSafety,
    /// Handling policy for pointers not owned by rsmalloc.
    pub foreign_pointer: ForeignPointerPolicy,
}

#[cfg(feature = "expose-security-critical-settings")]
impl SecurityCritical {
    /// Secure defaults: randomized magic values and abort on foreign pointers.
    pub const DEFAULT: Self = Self {
        magic: MagicSafety::Randomized,
        foreign_pointer: ForeignPointerPolicy::Abort,
    };

    /// Creates security-sensitive settings.
    pub const fn new(magic: MagicSafety, foreign_pointer: ForeignPointerPolicy) -> Self {
        Self {
            magic,
            foreign_pointer,
        }
    }

    const fn state(self) -> SecurityState {
        SecurityState {
            randomize_magic: matches!(self.magic, MagicSafety::Randomized),
            abort_on_foreign_pointer: matches!(self.foreign_pointer, ForeignPointerPolicy::Abort),
        }
    }
}

/// Complete configuration for the v2 Rust global allocator.
///
/// Start from [`Config::DEFAULT`] or pass a customized [`Tuning`] to
/// [`Config::new`]. Configuration is consumed once by the first v2 allocator
/// instance that initializes the process-wide allocator state.
///
/// # Example
///
/// ```rust
/// use rsmalloc::v2::{
///     alloc::RSMalloc,
///     config::{Config, SegmentedBitmapTHP, THP, THPSettings, Tuning},
/// };
///
/// const CONFIG: Config = Config::new(
///     Tuning::DEFAULT
///         .with_thp(THPSettings::new(THP::Enabled, SegmentedBitmapTHP::Force))
///         .with_max_refill_retries(4),
/// );
///
/// #[global_allocator]
/// static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Performance and memory-retention tuning.
    pub tuning: Tuning,
    security: SecurityState,
}

impl Config {
    /// Default v2 configuration.
    pub const DEFAULT: Self = Self {
        tuning: Tuning::DEFAULT,
        security: SecurityState::DEFAULT,
    };

    /// Creates a configuration with custom tuning and secure defaults.
    pub const fn new(tuning: Tuning) -> Self {
        Self {
            tuning,
            security: SecurityState::DEFAULT,
        }
    }

    /// Implementation of the temporary `legacy_config_to_v2!` compatibility macro.
    /// Security-weakening legacy settings require explicit migration to the v2 API.
    #[cfg(not(feature = "preload"))]
    #[allow(deprecated)]
    #[doc(hidden)]
    pub const fn from_legacy_compat(legacy: crate::RSMallocConfig) -> Self {
        use crate::frontend::global_alloc as old;

        if !matches!(legacy.magic_safety, old::MagicSafety::MagicRandomization) {
            panic!("fixed legacy magic requires explicit v2 security configuration");
        }
        if !matches!(
            legacy.foreign_pointer.global_alloc,
            old::ForeignPointerPolicy::Abort
        ) {
            panic!("ignoring foreign pointers requires explicit v2 security configuration");
        }

        let arena_size = if legacy.arena_min_size.0 < MIN_ARENA_SIZE {
            MIN_ARENA_SIZE
        } else {
            legacy.arena_min_size.0
        };
        if arena_size % ARENA_ALIGNMENT != 0 {
            panic!("legacy arena size must be a multiple of 4 KiB for v2");
        }

        let thp = if matches!(legacy.thp_settings.thp, old::THP::Enabled) {
            THP::Enabled
        } else {
            THP::Disabled
        };
        let segmented_thp = if matches!(
            legacy.thp_settings.segmented_bitmap_use_thp,
            old::SegmentedBitmapTHP::Force
        ) {
            SegmentedBitmapTHP::Force
        } else {
            SegmentedBitmapTHP::Disabled
        };
        let cache = match legacy.max_per_segmented_bitmap_cache {
            old::PerCacheLimit::Bytes(bytes) => PerCacheLimit::Custom(Size::bytes(bytes)),
            old::PerCacheLimit::Default => PerCacheLimit::Default,
        };
        let trim_worker = if matches!(
            legacy.trim_thread.background_worker,
            old::TrimThread::Enabled
        ) {
            TrimThread::Enabled
        } else {
            TrimThread::Disabled
        };
        let relief_state = if matches!(legacy.relief.state, old::ReliefState::Enabled) {
            ReliefState::Enabled
        } else {
            ReliefState::Disabled
        };

        Self::new(Tuning {
            thp: THPSettings::new(thp, segmented_thp),
            refill_init_batch: legacy.predictor_settings.init_batch,
            max_refill_retries: legacy.max_refill_retries,
            max_per_segmented_bitmap_cache: cache,
            trim: TrimSettings::new(
                trim_worker,
                Size::Bytes(legacy.trim_thread.threshold.0),
                Size::BIG_TRIM_DEFAULT,
            ),
            relief: ReliefSettings::new(
                relief_state,
                Percentage::new(legacy.relief.segmented_bitmap_disable_percentage.0),
                Percentage::new(legacy.relief.segmented_bitmap_enable_percentage.0),
            ),
            arena_min_size: ArenaSize(Size::bytes(arena_size)),
        })
    }

    /// Replaces the ordinary tuning while preserving security settings.
    #[must_use]
    pub const fn with_tuning(self, tuning: Tuning) -> Self {
        Self { tuning, ..self }
    }

    #[cfg(feature = "expose-security-critical-settings")]
    /// Replaces the security-sensitive settings.
    ///
    /// # Safety
    ///
    /// The caller must accept that the supplied settings can weaken invalid
    /// free and corruption detection. Prefer [`Config::DEFAULT`] unless the
    /// consequences are understood and required.
    #[must_use]
    pub const unsafe fn with_security_critical(self, settings: SecurityCritical) -> Self {
        Self {
            security: settings.state(),
            ..self
        }
    }

    pub(crate) const fn bootstrap(self) -> BootstrapConfig {
        let disable_percentage = self.tuning.relief.segmented_bitmap_disable_percentage.get();
        let requested_enable_percentage =
            self.tuning.relief.segmented_bitmap_enable_percentage.get();
        let enable_percentage = if requested_enable_percentage > disable_percentage {
            disable_percentage
        } else {
            requested_enable_percentage
        };
        let arena_size = self.tuning.arena_min_size.0;

        BootstrapConfig::new(
            arena_size.get(),
            self.tuning.max_refill_retries as usize,
            self.tuning.refill_init_batch as usize,
            self.tuning.max_per_segmented_bitmap_cache.bytes(),
            self.tuning.thp.segmented_bitmap_use_thp.enabled(),
            self.tuning.trim.background_worker.disabled(),
            self.tuning.trim.small_threshold.get(),
            self.tuning.trim.big_threshold.get(),
            self.tuning.relief.state.disabled(),
            disable_percentage,
            enable_percentage,
            !self.tuning.thp.thp.enabled(),
            self.security.randomize_magic,
            self.security.abort_on_foreign_pointer,
        )
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Default for Tuning {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Temporarily converts a legacy `RSMallocConfig` into a v2 `Config`.
///
/// This works in const contexts, including a `#[global_allocator]` static.
/// Legacy arena sizes below 256 KiB are raised to that minimum. Fixed magic,
/// ignored foreign pointers, and larger arena sizes not divisible by 4 KiB
/// are rejected instead of silently changing their behavior. Migrate these
/// settings explicitly using the v2 API when needed.
///
/// ```ignore
/// use rsmalloc::{legacy_config_to_v2, RSMallocConfig};
/// use rsmalloc::v2::alloc::RSMalloc;
///
/// #[global_allocator]
/// static GLOBAL: RSMalloc = RSMalloc::new(legacy_config_to_v2!(RSMallocConfig::DEFAULT));
/// ```
#[cfg(not(feature = "preload"))]
#[macro_export]
macro_rules! legacy_config_to_v2 {
    ($config:expr) => {
        $crate::v2::config::Config::from_legacy_compat($config)
    };
}
