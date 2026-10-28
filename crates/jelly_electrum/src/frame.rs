use jelly_net::JsonRpcResponse;
use serde_json::value::RawValue;

/// Electrum frames one JSON-RPC document per line and uses no other framing.
const TERMINATOR: u8 = b'\n';

/// One JSON-RPC message off the wire. A server push is a message with no `id`.
///
/// A `result` that is present but null stays `Some(null)`. Folding it into `None` would
/// make the line look malformed and retire the socket with every request in flight.
pub(crate) type Message = JsonRpcResponse<Box<RawValue>>;

/// A whole line. A batch is answered with an array and everything else with an
/// object, so the first non-space byte decides without a trial parse.
#[derive(Debug)]
pub(crate) enum Frame {
    Single(Message),
    Batch(Vec<Message>),
}

pub(crate) fn parse_frame(line: &[u8]) -> Result<Frame, serde_json::Error> {
    let is_batch = line.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'[');
    if is_batch {
        return serde_json::from_slice(line).map(Frame::Batch);
    }
    serde_json::from_slice(line).map(Frame::Single)
}

/// Splits a byte stream into newline-delimited frames.
///
/// The limit applies only to the unterminated tail, so many complete lines in one read
/// are fine but an endless line isn't.
#[derive(Debug)]
pub(crate) struct FrameReader {
    buffer: Vec<u8>,
    /// How much of `buffer` is known to hold no newline, so a line arriving over many
    /// reads is scanned only once.
    scanned_bytes: usize,
    limit_bytes: usize,
}

impl FrameReader {
    pub(crate) const fn new(limit_bytes: usize) -> Self {
        Self { buffer: Vec::new(), scanned_bytes: 0, limit_bytes }
    }

    pub(crate) fn extend(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// The next complete line, without its newline or a carriage return before it.
    pub(crate) fn take_line(&mut self) -> Option<Vec<u8>> {
        let unscanned = self.buffer.get(self.scanned_bytes..).unwrap_or_default();
        let Some(offset) = unscanned.iter().position(|byte| *byte == TERMINATOR) else {
            self.scanned_bytes = self.buffer.len();
            return None;
        };
        let position = self.scanned_bytes.saturating_add(offset);
        self.scanned_bytes = 0;
        let mut line: Vec<u8> = self.buffer.drain(..=position).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(line)
    }

    /// True once the unterminated tail is past the limit.
    pub(crate) const fn is_oversized(&self) -> bool {
        self.buffer.len() > self.limit_bytes
    }

    #[cfg(test)]
    pub(crate) const fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }
}

pub(crate) fn encode_line(body: &str) -> Vec<u8> {
    let mut line = Vec::with_capacity(body.len().saturating_add(1));
    line.extend_from_slice(body.as_bytes());
    line.push(TERMINATOR);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_arriving_in_pieces_is_reassembled() {
        let mut reader = FrameReader::new(64);
        reader.extend(b"{\"id\":1,");
        assert!(reader.take_line().is_none());
        reader.extend(b"\"result\":7}\n{\"id\":2,\"result\":8}\n");
        assert_eq!(reader.take_line().as_deref(), Some(&b"{\"id\":1,\"result\":7}"[..]));
        assert_eq!(reader.take_line().as_deref(), Some(&b"{\"id\":2,\"result\":8}"[..]));
        assert_eq!(reader.take_line(), None);
        assert_eq!(reader.pending_bytes(), 0);
    }

    #[test]
    fn a_carriage_return_before_the_newline_is_dropped() {
        let mut reader = FrameReader::new(64);
        reader.extend(b"{\"id\":1}\r\n");
        assert_eq!(reader.take_line().as_deref(), Some(&b"{\"id\":1}"[..]));
    }

    #[test]
    fn several_complete_lines_at_once_are_not_oversized_but_an_endless_one_is() {
        let mut reader = FrameReader::new(8);
        reader.extend(b"aaaa\nbbbb\ncccc\n");
        assert!(reader.take_line().is_some());
        while reader.take_line().is_some() {}
        assert!(!reader.is_oversized());

        reader.extend(b"123456789");
        assert!(reader.is_oversized());
    }

    #[test]
    fn a_batch_line_and_a_single_line_parse_differently() {
        let single = parse_frame(b"{\"id\":1,\"result\":7}").expect("an object");
        assert!(matches!(single, Frame::Single(_)));
        match parse_frame(b" [{\"id\":1,\"result\":7}]").expect("an array") {
            Frame::Batch(items) => assert_eq!(items.len(), 1),
            Frame::Single(_) => panic!("an array is a batch"),
        }
        parse_frame(b"not json").unwrap_err();
    }

    #[test]
    fn a_line_split_across_many_reads_is_found_once_its_newline_arrives() {
        let mut reader = FrameReader::new(64);
        for piece in [&b"{\"id\""[..], b":1,", b"\"result\"", b":7}"] {
            reader.extend(piece);
            assert!(reader.take_line().is_none());
        }
        reader.extend(b"\n{\"id\":2}");
        assert_eq!(reader.take_line().as_deref(), Some(&b"{\"id\":1,\"result\":7}"[..]));
        assert!(reader.take_line().is_none());
        reader.extend(b"\n");
        assert_eq!(reader.take_line().as_deref(), Some(&b"{\"id\":2}"[..]));
        assert_eq!(reader.pending_bytes(), 0);
    }

    #[test]
    fn an_encoded_line_ends_with_exactly_one_newline() {
        assert_eq!(encode_line("{}"), b"{}\n".to_vec());
    }

    /// Reading a null result as absent would retire the socket over an answer.
    #[test]
    fn a_null_result_is_present_and_an_absent_one_is_not() {
        let with_null = parse_frame(b"{\"id\":1,\"result\":null}").expect("json");
        let without = parse_frame(b"{\"id\":1}").expect("json");
        match (with_null, without) {
            (Frame::Single(null), Frame::Single(absent)) => {
                assert_eq!(null.result.as_ref().map(|raw| raw.get()), Some("null"));
                assert!(absent.result.is_none());
            }
            _ => panic!("an object is not a batch"),
        }
    }
}
