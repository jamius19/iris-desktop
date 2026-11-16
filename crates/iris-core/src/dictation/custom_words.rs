/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Custom word correction (Android `CustomWords.kt`, a stricter port of Handy's `apply_custom_words` in
//! `audio_toolkit/text.rs`).

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

/// Longest run of words (n-gram) compared with a custom word ("deep gram" -> "Deepgram").
const MAX_NGRAM: usize = 3;

/// A candidate is replaced when its score (0 = same key) is below this (Handy's default).
const MATCH_THRESHOLD: f64 = 0.18;

/// Score factor when the Soundex codes match (Handy: 0.3, which turns "Son of" into "Shinobi").
const PHONETIC_FACTOR: f64 = 0.5;

/// Shorter keys only get the exact fix (casing; joined words only with a digit or symbol, "GPT-5"): short words are
/// too often one edit apart, and short plain words too often two words ("is a", not "Isa").
const MIN_FUZZY_KEY_LENGTH: usize = 5;

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").unwrap());

/// Lowercase ASCII letters and digits only.
fn key(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect()
}

/// `text` with words that match one of `words` (the custom words) replaced by the custom word as typed. Port of
/// Handy's `apply_custom_words`, stricter: phonetic factor 0.5, fuzzy matching only for keys of 5+ characters, and
/// only ASCII words. Runs of 1-3 words joined by spaces or tabs only (never across punctuation or a line break) are
/// compared by key (lowercase letters and digits): an equal key always matches (casing, split words); otherwise the
/// normalised Levenshtein distance, halved when the Soundex codes match, must stay below 0.18. A plural (the key plus
/// "s" or "es") is never a match. A letters-only custom word shorter than 5 matches one word only ("is a" stays, not
/// "Isa"; "gpt 5" still becomes "GPT-5"), and if it is all uppercase (an acronym: "US", "IT") it leaves an
/// all-lowercase word as is ("let us know"). The closest candidate wins, ties go to the longer one. Unlike Handy, a
/// fuzzy span gives way to a closer match starting inside it ("a deepgram" is not one fuzzy "Deepgram": "deepgram"
/// alone matches exactly). Everything outside a replaced span is kept as is.
pub fn correct_custom_words(text: &str, words: &[String]) -> String {
    let custom: Vec<(&str, String)> = words
        .iter()
        .filter(|word| word.is_ascii())
        .map(|word| (word.as_str(), key(word)))
        .filter(|(_, key)| !key.is_empty())
        .collect();
    if custom.is_empty() {
        return text.to_string();
    }
    let tokens: Vec<Range<usize>> = TOKEN.find_iter(text).map(|m| m.range()).collect();
    let mut result = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i < tokens.len() {
        let Some(best) = best_match(text, &tokens, i, &custom) else {
            i += 1;
            continue;
        };
        let beaten = (i + 1..i + best.n).any(|j| best_match(text, &tokens, j, &custom).is_some_and(|m| m.beats(&best)));
        if beaten {
            i += 1;
            continue;
        }
        result.push_str(&text[copied..tokens[i].start]);
        result.push_str(best.word);
        copied = tokens[i + best.n - 1].end;
        i += best.n;
    }
    if copied == 0 {
        return text.to_string();
    }
    result.push_str(&text[copied..]);
    result
}

/// A custom `word` matching `n` words with `score` (0 = same key).
struct Match<'a> {
    word: &'a str,
    score: f64,
    n: usize,
}

impl Match<'_> {
    fn beats(&self, other: &Match) -> bool {
        self.score < other.score
    }
}

/// The closest custom word to 1-3 words starting at token `i`, or None if none is below the threshold.
fn best_match<'a>(text: &str, tokens: &[Range<usize>], i: usize, custom: &'a [(&'a str, String)]) -> Option<Match<'a>> {
    let mut best: Option<Match> = None;
    let mut candidate = String::new();
    for n in 1..=MAX_NGRAM {
        let last = i + n - 1;
        if last >= tokens.len() {
            break;
        }
        let token = &text[tokens[last].clone()];
        if !token.chars().all(|c| c.is_ascii_alphanumeric()) {
            break;
        }
        if n > 1 && !text[tokens[last - 1].end..tokens[last].start].chars().all(|c| c == ' ' || c == '\t') {
            break;
        }
        candidate.push_str(&key(token));
        for (word, word_key) in custom {
            if word_key.len() < MIN_FUZZY_KEY_LENGTH && word.chars().all(|c| c.is_ascii_alphabetic()) {
                // Short plain words: no joins ("in a" is not "Ina"), and an acronym leaves a lowercase word as is.
                let acronym = word.chars().all(|c| c.is_ascii_uppercase());
                if n > 1 || (acronym && token.chars().all(|c| c.is_ascii_lowercase())) {
                    continue;
                }
            }
            let Some(score) = score(&candidate, word_key) else {
                continue;
            };
            let better = match &best {
                None => score < MATCH_THRESHOLD,
                Some(current) => score < current.score || (score == current.score && n > current.n),
            };
            if better {
                best = Some(Match { word, score, n });
            }
        }
    }
    best
}

/// 0 for equal keys, else the fuzzy score, or None when the pair isn't compared (a plural of `b`, short, or too
/// different in length). `a` is the transcript's key, `b` the custom word's.
fn score(a: &str, b: &str) -> Option<f64> {
    if a == b {
        return Some(0.0);
    }
    if a.strip_prefix(b).is_some_and(|rest| rest == "s" || rest == "es") {
        return None;
    }
    if a.len() < MIN_FUZZY_KEY_LENGTH || b.len() < MIN_FUZZY_KEY_LENGTH {
        return None;
    }
    let max_length = a.len().max(b.len());
    if a.len().abs_diff(b.len()) as f64 > f64::max(2.0, max_length as f64 / 4.0) {
        return None;
    }
    let ratio = strsim::levenshtein(a, b) as f64 / max_length as f64;
    let letters = |s: &str| s.chars().all(|c| c.is_ascii_lowercase());
    let phonetic = letters(a) && letters(b) && soundex(a) == soundex(b);
    Some(if phonetic { ratio * PHONETIC_FACTOR } else { ratio })
}

/// American Soundex of a lowercase a-z `word`: h and w don't separate equal codes, vowels and y do.
fn soundex(word: &str) -> String {
    let mut chars = word.chars();
    let first = chars.next().unwrap();
    let mut code = String::from(first);
    let mut last = soundex_digit(first);
    for c in chars {
        if code.len() == 4 {
            break;
        }
        let digit = soundex_digit(c);
        if digit != '0' && digit != last {
            code.push(digit);
        }
        if c != 'h' && c != 'w' {
            last = digit;
        }
    }
    while code.len() < 4 {
        code.push('0');
    }
    code
}

fn soundex_digit(c: char) -> char {
    match c {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        _ => '0',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn correct(text: &str, words: &[&str]) -> String {
        correct_custom_words(text, &words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn exact_key_fixes_the_casing() {
        assert_eq!("I run Kubernetes daily", correct("I run kubernetes daily", &["Kubernetes"]));
    }

    #[test]
    fn correct_text_is_unchanged() {
        assert_eq!("I use Deepgram.", correct("I use Deepgram.", &["Deepgram"]));
    }

    #[test]
    fn split_words_are_joined() {
        assert_eq!("try Deepgram now", correct("try deep gram now", &["Deepgram"]));
    }

    #[test]
    fn digits_count() {
        assert_eq!("use GPT-5 here", correct("use gpt 5 here", &["GPT-5"]));
    }

    #[test]
    fn similar_sounding_words_are_fixed() {
        assert_eq!("I use Soniox daily", correct("I use sonics daily", &["Soniox"]));
        assert_eq!("Kubernetes", correct("Kubernetis", &["Kubernetes"]));
    }

    #[test]
    fn multi_word_custom_words() {
        assert_eq!("hi Jamius Siam", correct("hi jamius siam", &["Jamius Siam"]));
    }

    #[test]
    fn a_fuzzy_span_gives_way_to_a_closer_match_inside_it() {
        assert_eq!("a Deepgram key", correct("a deepgram key", &["Deepgram"]));
        // "a stackoverflw" alone is a fuzzy match too, but "stackoverflw" is closer.
        assert_eq!("a Stackoverflow answer", correct("a stackoverflw answer", &["Stackoverflow"]));
    }

    #[test]
    fn short_plain_words_are_not_joined() {
        assert_eq!("This is a test, I live in a house", correct("This is a test, I live in a house", &["Isa", "Ina"]));
        assert_eq!("a lot of stuff", correct("a lot of stuff", &["Alot"]));
        assert_eq!("GPT-5", correct("gpt 5", &["GPT-5"]));
    }

    #[test]
    fn plurals_are_not_matched() {
        assert_eq!("two Teslas and three iPhones", correct("two Teslas and three iPhones", &["Tesla", "iPhone"]));
        assert_eq!("Deepgrams are cool", correct("Deepgrams are cool", &["Deepgram"]));
    }

    #[test]
    fn acronyms_leave_lowercase_words() {
        assert_eq!("let us know about it", correct("let us know about it", &["US", "IT"]));
        assert_eq!("the US market", correct("the Us market", &["US"]));
        assert_eq!("the US market", correct("the US market", &["US"]));
        assert_eq!("Go Ami", correct("go ami", &["Go", "Ami"]));
    }

    #[test]
    fn son_of_is_not_shinobi() {
        assert_eq!("the Son of man", correct("the Son of man", &["Shinobi"]));
    }

    #[test]
    fn short_keys_get_no_fuzzy_match() {
        assert_eq!("Sam said", correct("Sam said", &["Siam"]));
    }

    #[test]
    fn punctuation_and_line_breaks_end_an_ngram() {
        let words = ["Deepgram", "Jamius Siam"];
        assert_eq!("deep. Gram", correct("deep. Gram", &words));
        assert_eq!("Jamius, Siam", correct("Jamius, Siam", &words));
        assert_eq!("deep\ngram", correct("deep\ngram", &words));
    }

    #[test]
    fn punctuation_around_a_match_is_kept() {
        assert_eq!("(Deepgram, ok)", correct("(deepgram, ok)", &["Deepgram"]));
    }

    #[test]
    fn possessives() {
        assert_eq!("Deepgram's model", correct("Deepgram's model", &["Deepgram"]));
        assert_eq!("Deepgram's model", correct("deepgram's model", &["Deepgram"]));
    }

    #[test]
    fn non_ascii_tokens_and_words_are_skipped() {
        assert_eq!("un café", correct("un café", &["Cafe"]));
        assert_eq!("привет мир", correct("привет мир", &["Privet"]));
        assert_eq!("a cafe", correct("a cafe", &["Café"]));
    }

    #[test]
    fn ties_prefer_the_longer_ngram() {
        assert_eq!("Deepgram", correct("deep gram", &["Deep", "Deepgram"]));
    }

    #[test]
    fn no_words_is_identity() {
        assert_eq!("deep gram", correct("deep gram", &[]));
    }
}
