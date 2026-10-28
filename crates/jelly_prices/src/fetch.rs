use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use jelly_net::{HttpRequest, Network};

use crate::candle::CandleRequest;
use crate::error::PriceError;
use crate::kraken;
use crate::quote::QuoteRequest;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Sends live price and candle requests.
///
/// Tests swap in a fake, so a whole polling pass runs without a socket. The real
/// requests go through the [`Network`]'s shared HTTP client, so prices use the same trust
/// configuration and proxy as chain traffic.
#[async_trait]
pub trait Fetch: Send + Sync + fmt::Debug {
    async fn fetch_quotes(&self, request: &QuoteRequest) -> Result<String, PriceError>;

    async fn fetch_candles(&self, request: &CandleRequest) -> Result<String, PriceError>;
}

#[async_trait]
impl Fetch for Network {
    async fn fetch_quotes(&self, request: &QuoteRequest) -> Result<String, PriceError> {
        get(self, request.url(), QuoteRequest::MAX_BYTES).await
    }

    async fn fetch_candles(&self, request: &CandleRequest) -> Result<String, PriceError> {
        get(self, request.url(), CandleRequest::MAX_BYTES).await
    }
}

async fn get(network: &Network, url: &str, max_bytes: usize) -> Result<String, PriceError> {
    let request =
        HttpRequest::get(REQUEST_TIMEOUT).with_headers(kraken::HEADERS).with_max_bytes(max_bytes);
    Ok(network.http().send_to(&kraken::label(), url, request).await?.into_body()?)
}
