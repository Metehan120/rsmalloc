//! Configuration types for the v2 Rust global allocator.
//!
//! [`Config`] separates ordinary performance and memory-retention tuning from
//! settings that weaken allocator safety. The latter are unavailable unless
//! the `expose-security-critical-settings` Cargo feature is enabled.

use crate::{backend::bootstrap::BootstrapConfig, core_prim::predictor::DEFAULT_BATCH};

const DEFAULT_BUDDY_CACHE: usize = 64 * 1024 * 1024;
const DEFAULT_TRIM_THRESHOLD: usize = 10 * 1024 * 1024;
const DEFAULT_ARENA_SIZE: usize = 256 * 1024 * 1024;

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

/// Transparent huge-page behavior for buddy allocator regions.
#[derive(Clone, Copy, Debug)]
pub enum BuddyTHP {
    /// Do not explicitly request huge pages for buddy regions.
    Disabled,
    /// Request huge pages when global THP support is enabled.
    Force,
}

impl BuddyTHP {
    const fn enabled(self) -> bool {
        matches!(self, Self::Force)
    }
}

/// Transparent huge-page settings for allocator mappings and buddy regions.
#[derive(Clone, Copy, Debug)]
pub struct THPSettings {
    /// Global transparent huge-page policy.
    pub thp: THP,
    /// Whether buddy regions should explicitly request huge pages.
    ///
    /// [`BuddyTHP::Force`] has no effect while [`THP::Disabled`] is selected.
    pub buddy_use_thp: BuddyTHP,
}

impl THPSettings {
    /// Default policy: enable general THP support without forcing it for buddy regions.
    pub const DEFAULT: Self = Self {
        thp: THP::Enabled,
        buddy_use_thp: BuddyTHP::Disabled,
    };

    /// Creates a transparent huge-page configuration.
    pub const fn new(thp: THP, buddy_use_thp: BuddyTHP) -> Self {
        Self { thp, buddy_use_thp }
    }
}

/// Maximum target size of an individual buddy cache region.
#[derive(Clone, Copy, Debug)]
pub enum PerCacheLimit {
    /// Requested bytes per buddy cache. The backend normalizes the value.
    Bytes(usize),
    /// Use rsmalloc's 64 MiB default.
    Default,
}

impl PerCacheLimit {
    const fn bytes(self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Default => DEFAULT_BUDDY_CACHE,
        }
    }
}

/// A byte count used by configuration fields.
#[derive(Clone, Copy, Debug)]
pub struct Bytes(pub usize);

impl Bytes {
    /// Default threshold for waking the background trimmer: 10 MiB.
    pub const TRIM_DEFAULT: Self = Self(DEFAULT_TRIM_THRESHOLD);
    /// Default minimum slab arena size: 256 MiB.
    pub const ARENA_DEFAULT: Self = Self(DEFAULT_ARENA_SIZE);
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
    /// Cached-byte threshold that triggers background trimming.
    pub threshold: Bytes,
}

impl TrimSettings {
    /// Default trimming configuration: worker enabled with a 10 MiB threshold.
    pub const DEFAULT: Self = Self {
        background_worker: TrimThread::Enabled,
        threshold: Bytes::TRIM_DEFAULT,
    };

    /// Creates background trimming settings.
    pub const fn new(background_worker: TrimThread, threshold: Bytes) -> Self {
        Self {
            background_worker,
            threshold,
        }
    }
}

/// Buddy memory-pressure relief state.
#[derive(Clone, Copy, Debug)]
pub enum ReliefState {
    /// Allow the allocator to disable buddy caching under memory pressure.
    Enabled,
    /// Keep buddy caching enabled regardless of the relief thresholds.
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

/// Memory-pressure relief settings for the buddy backend.
///
/// When enabled, buddy caching is disabled at the disable threshold and is
/// re-enabled after pressure falls to the enable threshold. If the configured
/// enable threshold exceeds the disable threshold, initialization lowers it to
/// the disable threshold.
#[derive(Clone, Copy, Debug)]
pub struct ReliefSettings {
    /// Whether memory-pressure relief is active.
    pub state: ReliefState,
    /// Pressure percentage at which buddy caching is disabled.
    pub buddy_disable_percentage: Percentage,
    /// Pressure percentage at or below which buddy caching may be re-enabled.
    pub buddy_enable_percentage: Percentage,
}

impl ReliefSettings {
    /// Default thresholds are 85% for disabling and 80% for re-enabling.
    /// Relief itself is disabled by default.
    pub const DEFAULT: Self = Self {
        state: ReliefState::Disabled,
        buddy_disable_percentage: Percentage::new(85),
        buddy_enable_percentage: Percentage::new(80),
    };

    /// Creates buddy memory-pressure relief settings.
    pub const fn new(
        state: ReliefState,
        buddy_disable_percentage: Percentage,
        buddy_enable_percentage: Percentage,
    ) -> Self {
        Self {
            state,
            buddy_disable_percentage,
            buddy_enable_percentage,
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
    /// Target maximum size of each buddy cache region.
    pub max_per_buddy_cache: PerCacheLimit,
    /// Background trimming policy.
    pub trim: TrimSettings,
    /// Buddy memory-pressure relief policy.
    pub relief: ReliefSettings,
    /// Minimum slab page-backend arena data size.
    ///
    /// Initialization enforces an absolute minimum of 256 KiB. The default is
    /// 256 MiB.
    pub arena_min_size: Bytes,
}

impl Tuning {
    /// Default allocator tuning.
    pub const DEFAULT: Self = Self {
        thp: THPSettings::DEFAULT,
        refill_init_batch: DEFAULT_BATCH as u8,
        max_refill_retries: 3,
        max_per_buddy_cache: PerCacheLimit::Default,
        trim: TrimSettings::DEFAULT,
        relief: ReliefSettings::DEFAULT,
        arena_min_size: Bytes::ARENA_DEFAULT,
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

    /// Replaces the buddy per-cache size limit.
    #[must_use]
    pub const fn with_max_per_buddy_cache(self, max_per_buddy_cache: PerCacheLimit) -> Self {
        Self {
            max_per_buddy_cache,
            ..self
        }
    }

    /// Replaces the background trimming settings.
    #[must_use]
    pub const fn with_trim(self, trim: TrimSettings) -> Self {
        Self { trim, ..self }
    }

    /// Replaces the buddy memory-pressure relief settings.
    #[must_use]
    pub const fn with_relief(self, relief: ReliefSettings) -> Self {
        Self { relief, ..self }
    }

    /// Replaces the minimum slab arena size.
    #[must_use]
    pub const fn with_arena_min_size(self, arena_min_size: Bytes) -> Self {
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
/// Policy for pointers that are not owned by rsmalloc.
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
///     config::{BuddyTHP, Config, THP, THPSettings, Tuning},
/// };
///
/// const CONFIG: Config = Config::new(
///     Tuning::DEFAULT
///         .with_thp(THPSettings::new(THP::Enabled, BuddyTHP::Force))
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
        let disable_percentage = self.tuning.relief.buddy_disable_percentage.get();
        let requested_enable_percentage = self.tuning.relief.buddy_enable_percentage.get();
        let enable_percentage = if requested_enable_percentage > disable_percentage {
            disable_percentage
        } else {
            requested_enable_percentage
        };
        let requested_arena_size = self.tuning.arena_min_size.0;
        let arena_size = if requested_arena_size < 256 * 1024 {
            256 * 1024
        } else {
            requested_arena_size
        };

        BootstrapConfig::new(
            arena_size,
            self.tuning.max_refill_retries as usize,
            self.tuning.refill_init_batch as usize,
            self.tuning.max_per_buddy_cache.bytes(),
            self.tuning.thp.buddy_use_thp.enabled(),
            self.tuning.trim.background_worker.disabled(),
            self.tuning.trim.threshold.0,
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
