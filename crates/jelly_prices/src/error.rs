use std::time::Duration;

use jelly_core::ClockError;
use jelly_net::NetError;

/// No variant carries a response fragment, even through a source error, so a
/// malformed body reports only a position. These errors affect price freshness,
/// never wallet operations.
#[derive(Debug, thiserror::Error)]
pub enum PriceError {
    #[error(transparent)]
    Net(#[from] NetError),
    #[error("the Kraken response is not the documented shape at line {line} column {column}")]
    Malformed { line: usize, column: usize },
    #[error("Kraken refused the request")]
    Refused,
    #[error("the Kraken answer does not name the requested pair")]
    Absent,
    #[error(transparent)]
    Clock(#[from] ClockError),
}

/// Keeps where the body stopped making sense and discards what it said.
impl From<serde_json::Error> for PriceError {
    fn from(cause: serde_json::Error) -> Self {
        Self::Malformed { line: cause.line(), column: cause.column() }
    }
}

impl PriceError {
    /// How long the provider asked to be left alone, if it said so. Only the
    /// transport carries such a request.
    pub(crate) fn retry_after(&self, now_unix_secs: i64) -> Option<Duration> {
        match *self {
            Self::Net(ref net) => net.retry_after().map(|after| after.resolve(now_unix_secs)),
            Self::Malformed { .. } | Self::Refused | Self::Absent | Self::Clock(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::fixtures::planned;
    use crate::ticker::Ticker;

    /// Bodies that must not reach a log through an error.
    const HOSTILE_BODIES: [&str; 6] = [
        "SUPERSECRETMARKER",
        "{\"error\":[\"SUPERSECRETMARKER\"],\"result\":{}}",
        "{\"error\":[],\"result\":\"SUPERSECRETMARKER\"}",
        "{\"error\":[],\"result\":{\"SUPERSECRETMARKER\":{\"c\":[\"1.0\"]}}}",
        "{\"error\":[],\"result\":{\"XXBTZUSD\":\"SUPERSECRETMARKER\"}}",
        "{\"error\":[],\"result\":{\"XXBTZUSD\":{\"c\":[\"SUPERSECRETMARKER\"]}}}",
    ];

    /// Everything an error can be rendered as, including its whole source chain.
    fn rendered(error: &PriceError) -> String {
        let mut parts = vec![format!("{error} {error:?}")];
        let mut current: Option<&dyn Error> = error.source();
        while let Some(cause) = current {
            parts.push(format!("{cause} {cause:?}"));
            current = cause.source();
        }
        parts.join(" ")
    }

    #[test]
    fn no_error_repeats_a_fragment_of_a_response_body() {
        for body in HOSTILE_BODIES {
            match planned(&[Ticker::Btc]).decode(body) {
                Err(error) => {
                    let text = rendered(&error);
                    assert!(!text.contains("SUPERSECRET"), "{text}");
                }
                Ok(quotes) => {
                    let text = format!("{quotes:?}");
                    assert!(!text.contains("SUPERSECRET"), "{text}");
                }
            }
        }
    }

    #[test]
    fn a_malformed_body_is_reported_by_position_only() {
        let error = planned(&[Ticker::Btc])
            .decode("{\"error\":[],\"result\":\"SUPERSECRETMARKER\"}")
            .expect_err("the body is not the documented shape");
        let PriceError::Malformed { line, column } = error else { panic!("{error}") };
        assert_eq!(line, 1);
        assert!(column > 0, "the position is not recorded");
        let message = error.to_string();
        assert!(message.len() < 100, "an error grew a body-sized message: {message}");
    }

    #[test]
    fn a_refusal_names_only_the_provider() {
        let error = planned(&[Ticker::Btc])
            .decode("{\"error\":[\"EQuery:Unknown asset pair SUPERSECRETMARKER\"],\"result\":{}}")
            .expect_err("an unknown pair is a refusal");
        assert!(matches!(error, PriceError::Refused));
        assert_eq!(error.to_string(), "Kraken refused the request");
    }

    #[test]
    fn a_decode_error_never_carries_the_url() {
        for body in HOSTILE_BODIES {
            if let Err(error) = planned(&[Ticker::Btc]).decode(body) {
                let text = rendered(&error);
                assert!(!text.contains("https://"), "{text}");
                assert!(!text.contains("api.kraken.com"), "{text}");
            }
        }
    }
}
