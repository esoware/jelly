//! The fee service over a loopback host.

use std::sync::Arc;
use std::time::Duration;

use jelly_core::{Clock, FeeRate, Transport, test_util::FakeClock};
use jelly_net::{Endpoint, HttpClient, HttpConnector, NetRuntime, NoIdentity, PacerPolicy};
use jelly_utxo::{Auction, FeeApi as _, MempoolFeeApi, UtxoParams};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::common::{BITCOIN, Fallible, auction};

const WHOLE_NUMBER_ROUTE: &str = "/v1/fees/recommended";

const RECOMMENDED: &str =
    r#"{"fastestFee":12,"halfHourFee":8,"hourFee":4,"economyFee":2,"minimumFee":1}"#;

/// Answers the way a deployment that predates the precise route does, which is 404
/// for every route but the whole-number one.
async fn answer(mut stream: TcpStream) -> Fallible<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(stream.read_u8().await?);
    }
    let head = String::from_utf8(bytes)?;
    let path = head.split(' ').nth(1).ok_or("the request has no path")?.to_owned();
    let (status, body) =
        if path == WHOLE_NUMBER_ROUTE { ("200 OK", RECOMMENDED) } else { ("404 Not Found", "") };
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    Ok(path)
}

async fn serve(listener: TcpListener, requests: usize) -> Fallible<Vec<String>> {
    timeout(Duration::from_secs(20), async {
        let mut paths = Vec::with_capacity(requests);
        for _ in 0..requests {
            let (stream, _) = listener.accept().await?;
            paths.push(timeout(Duration::from_secs(10), answer(stream)).await??);
        }
        Ok(paths)
    })
    .await?
}

fn fee_api(params: &UtxoParams, auction: &Auction, url: &str) -> Fallible<MempoolFeeApi> {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let runtime = NetRuntime::with_pacing(clock, PacerPolicy::UNTHROTTLED);
    let connector = HttpConnector::new(HttpClient::new(None)?, Arc::new(NoIdentity));
    let endpoint = Endpoint::parse(params.chain, Transport::FeeApi, url)?;
    let pool = runtime.pool(params.chain, Transport::FeeApi, Arc::new(connector), vec![endpoint]);
    Ok(MempoolFeeApi::new(Arc::new(pool), auction))
}

#[tokio::test]
async fn a_host_without_the_precise_route_is_read_through_the_whole_number_one() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let market = auction(&BITCOIN).unwrap();
    let api = fee_api(&BITCOIN, &market, &url).unwrap();
    let requesting = async { Ok(api.recommended().await?) };
    let (fees, paths) = tokio::try_join!(requesting, serve(listener, 2)).unwrap();

    assert_eq!(fees.half_hour, FeeRate::from_sat_per_kwu(2_000));
    assert_eq!(paths, [market.api_route, WHOLE_NUMBER_ROUTE]);
}

#[tokio::test]
async fn without_a_fallback_a_missing_route_fails_after_one_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let market = Auction { api_fallback_route: None, ..auction(&BITCOIN).unwrap() };
    let api = fee_api(&BITCOIN, &market, &url).unwrap();
    let requesting = async {
        Ok(api.recommended().await.expect_err("a host without the route has no rate to give"))
    };
    let (_, paths) = tokio::try_join!(requesting, serve(listener, 1)).unwrap();
    assert_eq!(paths, [market.api_route]);
}
