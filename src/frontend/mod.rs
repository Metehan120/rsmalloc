#[cfg(feature = "preload")]
pub mod abi;
#[deprecated = "please use new v2 API"]
#[cfg(not(feature = "preload"))]
pub mod global_alloc;
#[cfg(not(feature = "preload"))]
pub mod global_alloc2;
