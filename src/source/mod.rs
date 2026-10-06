//! Independent retrieval, extraction and caching shared by billing and conversion sources.
pub(crate) mod cache;
pub mod extraction;
pub(crate) mod http;
pub(crate) mod numeric;

/// Retrieval returns text; extraction and price protocols interpret it separately.
pub(crate) trait Retrieval: Send + Sync {
    fn get(&self) -> crate::Result<String>;
}
