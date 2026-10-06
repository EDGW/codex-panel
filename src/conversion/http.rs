//! Independent retrieval. No extraction, billing or authentication knowledge.
use crate::dest::Result;
use reqwest::Url;
use reqwest::blocking::Client;
use std::time::Duration;

pub struct HttpGet {
    client: Client,
    url: Url,
}

impl HttpGet {
    pub fn new(url: Url, timeout: Duration) -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| "cannot initialize independent HTTP client")?;
        Ok(Self { client, url })
    }

    pub fn get(&self) -> Result<String> {
        let response = self
            .client
            .get(self.url.clone())
            .send()
            .map_err(|_| "source GET failed (network error or timeout)")?;
        if !response.status().is_success() {
            return Err(format!("source GET HTTP {}", response.status().as_u16()));
        }
        response
            .text()
            .map_err(|_| "cannot read source response".into())
    }
}
