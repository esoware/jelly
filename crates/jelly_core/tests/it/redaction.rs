use jelly_core::{
    ChainError, ChainId, CursorBytes, CursorScope, EndpointAttempt, EndpointHealth,
    EndpointHealthRecord, EndpointLabel, EndpointOverride, FailureKind, HeldReservation,
    ImportedKey, KeyKind, KeyMaterial, ProxySetting, ReservationSlot, ScanAdvance, ScanStream,
    ScriptKind, SecretText,
};
use zeroize::{Zeroize as _, Zeroizing};

const MARKER: &str = "correcthorsebatterystaple";

fn leaks(rendering: &str) -> bool {
    rendering.contains(MARKER)
}

#[test]
fn key_material_never_prints_its_bytes() {
    let key = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(MARKER.as_bytes().to_vec()));
    let debug = format!("{key:?}");
    assert!(!leaks(&debug), "{debug}");
    assert!(debug.contains("redacted"), "{debug}");
    assert!(debug.contains("Secp256k1"), "{debug}");
    assert_eq!(key.len(), MARKER.len());
    assert!(!key.is_empty());
    key.with_bytes(|bytes| assert_eq!(bytes, MARKER.as_bytes()));
}

#[test]
fn an_imported_key_never_prints_its_bytes() {
    let imported = ImportedKey {
        material: KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(MARKER.as_bytes().to_vec())),
        named_script: Some(ScriptKind::P2wpkh),
    };
    let debug = format!("{imported:?}");
    assert!(!leaks(&debug), "{debug}");
    assert!(debug.contains("P2wpkh"), "{debug}");
}

#[test]
fn secret_text_never_prints_its_value() {
    let secret = SecretText::new(String::from(MARKER));
    let debug = format!("{secret:?}");
    assert!(!leaks(&debug), "{debug}");
    assert_eq!(debug, "SecretText(<redacted>)");
    secret.with_str(|text| assert_eq!(text, MARKER));
}

#[test]
fn a_cursor_payload_is_read_through_its_closure_and_wiped_on_demand() {
    let mut cursor = CursorBytes::copied(MARKER.as_bytes());
    assert_eq!(cursor.len(), MARKER.len());
    assert!(!cursor.is_empty());
    assert!(cursor.with_bytes(|bytes| bytes == MARKER.as_bytes()));

    cursor.zeroize();
    assert!(cursor.with_bytes(<[u8]>::is_empty), "a wipe empties the buffer");
    assert!(CursorBytes::default().is_empty());
}

/// Monero cursor payloads contain secrets.
#[test]
fn a_cursor_payload_never_prints_itself() {
    let cursor = CursorBytes::copied(MARKER.as_bytes());
    let shown = format!("{cursor:?}");
    assert!(!leaks(&shown), "{shown}");
    assert_eq!(shown, format!("CursorBytes(<redacted {} bytes>)", MARKER.len()));

    let advance = ScanAdvance {
        scope: CursorScope::Wallet,
        stream: ScanStream::MoneroScan,
        cursor: cursor.clone(),
    };
    assert!(!leaks(&format!("{advance:?}")));
    let reservation = HeldReservation {
        scope: CursorScope::Wallet,
        slot: ReservationSlot::MoneroSends,
        payload: cursor,
    };
    assert!(!leaks(&format!("{reservation:?}")));
}

#[test]
fn an_endpoint_label_drops_userinfo_path_and_query() {
    let label = EndpointLabel::new(
        "HTTPS",
        "user:correcthorsebatterystaple@Node.Example.COM/rpc?api_key=SECRET",
        Some(443),
    );
    assert_eq!(label.host(), "node.example.com");
    assert_eq!(label.to_string(), "https://node.example.com:443");
    let rendered = format!("{label:?} {label}");
    assert!(!leaks(&rendered), "{rendered}");
    assert!(!rendered.contains("SECRET"), "{rendered}");
    assert!(!rendered.contains("rpc"), "{rendered}");
}

#[test]
fn an_endpoint_label_without_a_port_names_scheme_and_host_only() {
    let label = EndpointLabel::new("ssl", "electrum.example.org", None);
    assert_eq!(label.to_string(), "ssl://electrum.example.org");
}

#[test]
fn an_endpoint_label_survives_input_with_no_userinfo_or_path() {
    let label = EndpointLabel::new("http", "127.0.0.1", Some(8332));
    assert_eq!(label.host(), "127.0.0.1");
    assert_eq!(label.to_string(), "http://127.0.0.1:8332");
}

#[test]
fn an_at_sign_in_the_query_is_not_read_as_userinfo() {
    let label =
        EndpointLabel::new("https", "node.example.com/v1?key=abc@correcthorsebatterystaple", None);
    assert_eq!(label.host(), "node.example.com");
    let rendered = format!("{label} {label:?}");
    assert!(!leaks(&rendered), "{rendered}");
    assert!(!rendered.contains("abc"), "{rendered}");
}

#[test]
fn a_proxy_prints_its_host_and_never_its_credentials() {
    for proxy in [
        ProxySetting::Socks5h { url: format!("socks5h://user:{MARKER}@proxy.test:1080") },
        ProxySetting::Http { url: format!("http://user:{MARKER}@proxy.test:8080") },
    ] {
        let debug = format!("{proxy:?}");
        assert!(debug.contains("proxy.test"), "{debug}");
        assert!(!leaks(&debug), "{debug}");
        assert!(!debug.contains("user"), "{debug}");
    }
    assert_eq!(format!("{:?}", ProxySetting::System), "System");
}

#[test]
fn an_endpoint_override_prints_neither_its_url_nor_what_was_typed() {
    let url = format!("https://user:{MARKER}@node.example:8443/v1/path?apikey={MARKER}");
    let added = EndpointOverride {
        display: format!("my node {MARKER}"),
        is_pinned: true,
        ..EndpointOverride::new(url)
    };
    let debug = format!("{added:?}");
    assert!(!leaks(&debug), "{debug}");
    for fragment in ["user", "node.example", "/v1", "apikey", "my node"] {
        assert!(!debug.contains(fragment), "{fragment} reached {debug}");
    }
    assert!(debug.contains("is_pinned: true"), "{debug}");
}

#[test]
fn an_endpoint_health_record_prints_its_url_only_as_its_label() {
    // A URL with credentials in each of the places one can hide.
    let url = "https://user:secret-key@node.example:8443/v1/secret-path?apikey=secret-query";
    let health = EndpointHealthRecord {
        chain: ChainId::Bitcoin,
        url: url.to_owned(),
        health: EndpointHealth { latency_ms: Some(20), ..EndpointHealth::default() },
    };
    let rendered = format!("{health:?}");
    let label = format!("{:?}", EndpointLabel::from_url(url));
    assert!(rendered.contains(&label), "{rendered}");
    for fragment in ["user", "secret", "/v1", "apikey"] {
        assert!(!rendered.contains(fragment), "{fragment} reached {rendered}");
    }
}

#[test]
fn an_endpoints_down_error_names_hosts_and_no_urls() {
    let attempt = EndpointAttempt::new(
        EndpointLabel::new("https", "node.example.com/v1?key=correcthorsebatterystaple", None),
        FailureKind::Timeout,
        "no answer in 15s",
    );
    let error = ChainError::endpoints_down(ChainId::Bitcoin, "sync", vec![attempt]);
    let rendered = format!("{error} {error:?}");
    assert!(!leaks(&rendered), "{rendered}");
    assert!(rendered.contains("node.example.com"), "{rendered}");
}
