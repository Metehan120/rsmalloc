#[cfg(feature = "preload")]
pub mod abi;
#[cfg(not(feature = "preload"))]
pub mod global_alloc;
