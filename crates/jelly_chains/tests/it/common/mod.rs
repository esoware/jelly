use std::error::Error;
use std::sync::Arc;

use jelly_net::{EndpointRegistry, NetError, Network, test_util};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// A registry over every chain's shipped defaults.
pub(crate) fn registry() -> EndpointRegistry {
    EndpointRegistry::new(jelly_chains::chains().entries())
}

/// A direct network serving every chain, on a fake clock with no pins stored.
pub(crate) fn direct_network() -> Result<Arc<Network>, NetError> {
    test_util::direct_network(jelly_chains::chains().entries())
}

/// A loopback HTTP server that answers one request per connection with each of
/// `replies` in turn, as `(status, body)`, and hands back every request it read.
pub(crate) async fn serve(
    replies: Vec<(&'static str, &'static str)>,
) -> Fallible<(String, JoinHandle<Fallible<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut requests = Vec::with_capacity(replies.len());
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().await?;
            let mut head: Vec<u8> = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await?;
                head.extend_from_slice(&byte);
            }
            let head = String::from_utf8(head)?;
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            let mut payload = vec![0u8; length];
            stream.read_exact(&mut payload).await?;
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await?;
            requests.push(format!("{head}{}", String::from_utf8(payload)?));
        }
        Ok(requests)
    });
    Ok((url, server))
}
