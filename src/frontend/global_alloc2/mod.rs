pub mod alloc;
#[cfg(any(feature = "native-allocation-api", doc))]
pub mod allocation_api;
pub mod config;
mod debug;
pub mod helpers;
mod raw;

/// Common imports for configuring the v2 global allocator and using its core API.
///
/// Re-exports [`RSMalloc`](crate::v2::alloc::RSMalloc),
/// [`RSMallocCoreAPI`](crate::v2::alloc::RSMallocCoreAPI),
/// [`SimpleTrimSize`](crate::v2::alloc::SimpleTrimSize), and all public
/// configuration types. Importing the core trait makes manual initialization,
/// usable-size queries, and safe trimming methods available on `RSMalloc`.
///
/// The optional native allocation API, raw interface, and general-purpose
/// helpers are not included; import those explicitly from their respective modules.
///
/// ```rust
/// use rsmalloc::v2::prelude::*;
///
/// const CONFIG: Config = Config::DEFAULT;
///
/// #[global_allocator]
/// static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);
///
/// // Optional: initialize before the first allocation.
/// GLOBAL.manual_init();
/// ```
pub mod prelude {
    pub use crate::v2::alloc::RSMalloc;
    pub use crate::v2::alloc::RSMallocCoreAPI;
    pub use crate::v2::alloc::SimpleTrimSize;
    pub use crate::v2::config::*;
}
