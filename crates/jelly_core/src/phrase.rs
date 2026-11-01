use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// How many words a generated BIP-39 phrase has.
    pub enum PhraseLength {
        Twelve => "twelve",
        TwentyFour => "twenty_four",
    }
}

/// The words of a pasted BIP-39, Monero, or TON phrase.
///
/// Commas and semicolons separate words like whitespace, and a list number before a word,
/// as in `1. abandon` or `12)abandon`, is dropped, so a phrase copied from a numbered
/// backup still parses. No word in any of the lists contains a digit or any of this
/// punctuation.
pub fn phrase_words(phrase: &str) -> impl Iterator<Item = &str> {
    phrase
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(|token| {
            token.trim_start_matches(|c: char| {
                c.is_ascii_digit() || matches!(c, '.' | ')' | ':' | '-')
            })
        })
        .filter(|word| !word.is_empty())
}
