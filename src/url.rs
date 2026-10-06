//! Shared HTTP URL syntax validation, separate from routing and retrieval.
use crate::Result;
use reqwest::Url;

/// Retrieval URLs may contain a query; routes and protocol origins may not.
pub fn http_url(value: &str, allow_query: bool) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| "invalid HTTP(S) URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || (!allow_query && url.query().is_some())
        || url.fragment().is_some()
    {
        return Err("URL must be HTTP(S), without credentials or fragment (query allowed only for retrieval)".into());
    }
    Ok(url)
}
