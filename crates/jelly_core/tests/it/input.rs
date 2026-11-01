mod payment_uris {
    use jelly_core::{PaymentUri, payment_uri};

    #[test]
    fn a_uri_splits_into_its_target_and_query() {
        let uri = payment_uri("bitcoin", " BITCOIN:bc1q?amount=1&label=x ").expect("a uri");
        assert_eq!(uri.target, "bc1q");
        assert_eq!(uri.query, "amount=1&label=x");
        assert_eq!(uri.param("label"), Some("x"));
        assert_eq!(uri.param("memo"), None);
        assert_eq!(uri.params().count(), 2);
    }

    #[test]
    fn a_double_slash_and_a_missing_query_are_both_fine() {
        let uri = payment_uri("ethereum", "ethereum://0xabc").expect("a uri");
        assert_eq!(uri, PaymentUri { target: "0xabc", query: "" });
        assert_eq!(uri.params().count(), 0);
    }

    #[test]
    fn another_scheme_or_no_scheme_is_not_a_uri() {
        assert_eq!(payment_uri("bitcoin", "litecoin:ltc1q"), None);
        assert_eq!(payment_uri("bitcoin", "bc1q"), None);
    }

    #[test]
    fn parameter_names_are_percent_decoded_and_values_are_not() {
        let uri = payment_uri("solana", "solana:abc?%6Demo=hi%20there").expect("a uri");
        assert_eq!(uri.param("memo"), Some("hi%20there"));
    }
}

mod phrases {
    use jelly_core::phrase_words;

    #[test]
    fn list_numbers_and_separators_are_not_words() {
        let words: Vec<&str> = phrase_words("1. abandon, 2)ability;3: able\n4-about").collect();
        assert_eq!(words, ["abandon", "ability", "able", "about"]);
        assert_eq!(phrase_words("   ").count(), 0);
    }
}
