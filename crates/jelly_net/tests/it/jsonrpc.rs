use jelly_core::EndpointLabel;
use jelly_net::{
    JsonRpcBatch, JsonRpcRequest, JsonRpcResponse, NetError, Outcome, json_rpc_decode,
    json_rpc_outcome,
};
use serde_json::value::RawValue;

fn label() -> EndpointLabel {
    EndpointLabel::new("https", "example.com", None)
}

#[test]
fn a_request_and_a_batch_read_back_as_json_rpc_text() {
    let single = r#"{"jsonrpc":"2.0","id":7,"method":"eth_chainId","params":[]}"#;
    let request = JsonRpcRequest::new(7, "eth_chainId", &[(); 0]).expect("a request");
    assert_eq!(request.as_str(), single);

    let batch = JsonRpcBatch::new(&[request.clone(), request]);
    assert_eq!(batch.as_str(), format!("[{single},{single}]"));
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct Tip {
    height: u64,
}

#[test]
fn a_result_member_decodes_into_the_callers_own_type() {
    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"height":42}}"#;
    let tip: Tip = json_rpc_decode(&label(), body).expect("a decoded result");
    assert_eq!(tip.height, 42);
}

#[test]
fn an_error_object_is_an_rpc_failure_with_its_code() {
    let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"bad params"}}"#;
    let error = json_rpc_decode::<u64>(&label(), body).expect_err("an error object");
    assert!(matches!(error, NetError::Rpc { code: -32602, .. }));
}

#[test]
fn an_unavailable_transaction_remains_an_rpc_failure() {
    let body =
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32020,"message":"Transaction not found"}}"#;
    let error = json_rpc_decode::<u64>(&label(), body).expect_err("a missing transaction");
    assert!(matches!(error, NetError::Rpc { code: -32020, .. }), "{error}");
}

#[test]
fn an_error_sent_as_a_bare_string_is_still_an_rpc_failure() {
    let body = r#"{"jsonrpc":"2.0","id":1,"error":"over capacity"}"#;
    let error = json_rpc_decode::<u64>(&label(), body).expect_err("an error member");
    assert!(matches!(error, NetError::Rpc { code: 0, .. }), "{error}");
}

#[test]
fn both_error_shapes_read_as_a_code_and_a_message() {
    for (body, code) in [
        (r#"{"id":1,"error":{"code":-26,"message":"dust"}}"#, -26),
        (r#"{"id":1,"error":"dust"}"#, 0),
    ] {
        let response: JsonRpcResponse<Box<RawValue>> =
            serde_json::from_str(body).expect("a response");
        let error = response.error.expect("an error member");
        assert_eq!(error.message(), "dust", "{body}");
        assert_eq!(error.code(), code, "{body}");
    }
}

#[test]
fn a_response_with_neither_result_nor_error_is_a_protocol_failure() {
    let error = json_rpc_decode::<u64>(&label(), r#"{"jsonrpc":"2.0","id":1}"#)
        .expect_err("nothing to read");
    assert!(matches!(error, NetError::Protocol { .. }));
}

#[test]
fn a_request_every_endpoint_will_refuse_stops_the_walk() {
    for code in [-32700, -32600, -32601, -32602] {
        assert_eq!(json_rpc_outcome(code), Outcome::Fail, "{code}");
    }
    for code in [-32000, -32603, 429, 1, 3] {
        assert_eq!(json_rpc_outcome(code), Outcome::Rotate, "{code}");
    }
}
