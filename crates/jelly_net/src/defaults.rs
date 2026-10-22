use jelly_core::{ChainId, Transport};
use serde::Deserialize;

const TARGET: &str = "jelly::net::defaults";

/// One curated default, as it appears in a chain's `endpoints.json`.
#[derive(Debug, Deserialize)]
pub struct DefaultEndpoint {
    pub url: String,
    pub transport: Transport,
    pub note: Option<String>,
}

/// One curated defaults file, compiled into its chain's crate.
#[derive(Debug, Deserialize)]
pub struct DefaultEndpoints {
    pub chain: ChainId,
    pub endpoints: Vec<DefaultEndpoint>,
}

impl DefaultEndpoints {
    pub(crate) fn parse(chain: ChainId, raw: &str) -> Result<Self, &'static str> {
        let file: Self = serde_json::from_str(raw).map_err(|source| {
            tracing::error!(
                target: TARGET,
                chain = chain.as_str(),
                %source,
                "a compiled-in endpoint file does not parse"
            );
            "the file does not parse"
        })?;
        if file.chain != chain {
            return Err("the file declares another chain");
        }
        if file.endpoints.is_empty() {
            return Err("the file lists no endpoint");
        }
        Ok(file)
    }
}
