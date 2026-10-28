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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_numbers_and_separators_are_not_words() {
        let words: Vec<&str> = phrase_words("1. abandon, 2)ability;3: able\n4-about").collect();
        assert_eq!(words, ["abandon", "ability", "able", "about"]);
        assert_eq!(phrase_words("   ").count(), 0);
    }
}
