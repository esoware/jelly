use std::time::Duration;

use jelly_net::RetryAfter;

#[test]
fn a_delta_and_a_date_parse() {
    assert_eq!(RetryAfter::parse("120"), Some(RetryAfter::After(Duration::from_secs(120))));
    assert_eq!(
        RetryAfter::parse("Thu, 01 Jan 2026 00:01:00 GMT"),
        Some(RetryAfter::AtUnixSecs(1_767_225_660))
    );
    assert_eq!(
        RetryAfter::parse("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(RetryAfter::AtUnixSecs(784_111_777))
    );
}

#[test]
fn a_malformed_header_is_no_header() {
    for header in [
        "",
        "   ",
        "soon",
        "Mon, 32 Xxx 2026 00:00:00 GMT",
        "Thu, 01 Jan 2026 00:00:00 UTC",
        "Thu, 01 Jan 2026 25:00:00 GMT",
        "Thu 01 Jan 2026 00:00:00 GMT",
    ] {
        assert_eq!(RetryAfter::parse(header), None, "{header}");
    }
}
