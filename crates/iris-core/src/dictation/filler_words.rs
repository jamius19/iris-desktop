/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Filler word removal (Android `FillerWords.kt`, a port of Handy's built-in removal in `audio_toolkit/text.rs`).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::settings::Settings;
use crate::stt::local::models::{by_id, whisper_language};

/// Never real words in a language Iris's models output: removed whatever the language. (Handy's list.)
pub const UNIVERSAL_FILLER_WORDS: [&str; 13] =
    ["uh", "uhm", "umm", "uhh", "uhhh", "ehh", "ehm", "ahm", "hmm", "hm", "mmm", "хм", "ммм"];

/// Real words elsewhere (Portuguese "um" = "a", German "um" = "at"): removed only with the language known.
pub const GATED_FILLER_WORDS: [(&str, &[&str]); 3] =
    [("en", &["um", "ah", "eh"]), ("de", &["äh", "ähm"]), ("fr", &["euh"])];

/// Each filler (group 1) with an optional `...`, `…` or one of `, . ! ?` (group 2). Whole words only (not joined by
/// `-` or an apostrophe: "Uh-huh", "Um's") is checked by [`fillers`]: Rust's regex has no lookarounds.
static PATTERNS: LazyLock<HashMap<&str, Regex>> = LazyLock::new(|| {
    let gated = GATED_FILLER_WORDS.iter().flat_map(|(_, words)| words.iter());
    UNIVERSAL_FILLER_WORDS
        .iter()
        .chain(gated)
        .map(|word| (*word, Regex::new(&format!(r"(?i)({})(?:\.\.\.|…|([,.!?]))?", regex::escape(word))).unwrap()))
        .collect()
});

static WORD_CHAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}_'’-]").unwrap());

/// Kotlin's `Char.isLetterOrDigit`.
static LETTER_OR_DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{Nd}]").unwrap());

static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());

/// Spaces or tabs next to a line break, e.g. left by a filler that opened the line.
static LINE_EDGE_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]*\n[ \t]*").unwrap());

const SENTENCE_ENDS: &str = ".!?…";

fn is_word_char(c: char) -> bool {
    WORD_CHAR.is_match(c.encode_utf8(&mut [0; 4]))
}

fn is_letter_or_digit(c: char) -> bool {
    LETTER_OR_DIGIT.is_match(c.encode_utf8(&mut [0; 4]))
}

/// The base language of an ISO code ("pt-BR", "PT_br" -> "pt"); blank = None.
pub fn base_language(code: &str) -> Option<String> {
    let base = code.trim().to_lowercase().split(['-', '_']).next().unwrap_or_default().to_string();
    (!base.is_empty()).then_some(base)
}

/// The base language code of the transcript as the settings give it, or None if they don't.
pub fn output_language(settings: &Settings) -> Option<String> {
    let code = if settings.stt_on_device() {
        let model = by_id(&settings.on_device_stt_model);
        // The shared language reaches only Whisper on device; the other multilingual models never see it.
        match model.output_language {
            Some(language) => language.to_string(),
            None if model.id == "whisper-small" => whisper_language(&settings.stt_language),
            None => String::new(),
        }
    } else if settings.stt_soniox() {
        // Android's `sonioxLanguageHints(...).singleOrNull()`.
        let mut hints: Vec<String> = settings
            .soniox_languages
            .split(',')
            .map(|hint| hint.trim().to_lowercase())
            .filter(|hint| !hint.is_empty())
            .collect();
        hints.sort();
        hints.dedup();
        if hints.len() == 1 { hints.remove(0) } else { String::new() }
    } else {
        settings.stt_language.clone()
    };
    base_language(&code)
}

/// Whether `text` holds a filler that is removed only with the language known (else detection can't matter).
pub fn has_gated_filler(text: &str) -> bool {
    GATED_FILLER_WORDS.iter().flat_map(|(_, words)| words.iter()).any(|word| !fillers(text, &PATTERNS[word]).is_empty())
}

/// `text` without filler words: the universal ones always, the gated ones only for `language` (an ISO code, None =
/// unknown). Port of Handy's built-in removal (`remove_filler_matches`): a filler eats one trailing `...`, `…`,
/// comma, period, `!` or `?`, a capitalized filler that opened a sentence hands its capital on, and space runs
/// collapse (line breaks kept). Unlike Handy, a `.`, `!` or `?` the filler ate still ends the sentence before it. Not
/// ported: stutter collapse, a custom list, Chinese script handling. A changed text left with no letter or digit
/// becomes "".
pub fn remove_filler_words(text: &str, language: Option<&str>) -> String {
    let base = language.and_then(base_language);
    let gated = GATED_FILLER_WORDS.iter().find(|(code, _)| Some(*code) == base.as_deref()).map_or(&[][..], |g| g.1);
    let mut filtered = text.to_string();
    for word in UNIVERSAL_FILLER_WORDS.iter().chain(gated) {
        filtered = remove_matches(&filtered, &PATTERNS[word]);
    }
    let filtered = LINE_EDGE_SPACES.replace_all(&MULTI_SPACE.replace_all(&filtered, " "), "\n").trim().to_string();
    if filtered != text && !filtered.chars().any(is_letter_or_digit) { String::new() } else { filtered }
}

/// The whole-word matches of `pattern` in `text`: start, end and the eaten mark, if one of `, . ! ?`.
fn fillers(text: &str, pattern: &Regex) -> Vec<(usize, usize, Option<char>)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(caps) = pattern.captures_at(text, at) {
        let word = caps.get(1).unwrap();
        let before = text[..word.start()].chars().next_back();
        let after = text[word.end()..].chars().next();
        if before.is_some_and(is_word_char) || after.is_some_and(is_word_char) {
            // Not a whole word here: search again from the next character.
            at = word.start() + text[word.start()..].chars().next().unwrap().len_utf8();
            continue;
        }
        let end = caps.get(0).unwrap().end();
        found.push((word.start(), end, caps.get(2).and_then(|mark| mark.as_str().chars().next())));
        at = end;
    }
    found
}

fn remove_matches(text: &str, pattern: &Regex) -> String {
    let mut kept = String::new();
    let mut capital_owed = false;
    let mut resume = 0;
    for (start, end, mark) in fillers(text, pattern) {
        push(&mut kept, &text[resume..start], &mut capital_owed);
        capital_owed = capital_owed || (text[start..].chars().next().unwrap().is_uppercase() && opens_sentence(&kept));
        if let Some(mark) = mark.filter(|mark| *mark != ',') {
            end_sentence(&mut kept, mark);
        }
        resume = end;
    }
    push(&mut kept, &text[resume..], &mut capital_owed);
    kept
}

/// Appends `segment`, capitalizing its first letter or digit if a capital is owed.
fn push(kept: &mut String, segment: &str, capital_owed: &mut bool) {
    match segment.char_indices().find(|(_, c)| is_letter_or_digit(*c)) {
        Some((i, c)) if *capital_owed => {
            kept.push_str(&segment[..i]);
            kept.extend(c.to_uppercase());
            kept.push_str(&segment[i + c.len_utf8()..]);
            *capital_owed = false;
        }
        _ => kept.push_str(segment),
    }
}

fn opens_sentence(kept: &str) -> bool {
    kept.trim_end().chars().next_back().is_none_or(|c| SENTENCE_ENDS.contains(c))
}

/// The `mark` a removed filler ate ends the kept text's sentence ("Let me check, um." -> "Let me check.").
fn end_sentence(kept: &mut String, mark: char) {
    let end = kept.trim_end();
    let end = end.strip_suffix(',').unwrap_or(end).trim_end();
    if end.trim().is_empty() || end.ends_with(|c| SENTENCE_ENDS.contains(c)) {
        return;
    }
    kept.truncate(end.len());
    kept.push(mark);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::settings::{
        STT_PROVIDER_DEEPGRAM, STT_PROVIDER_ON_DEVICE, STT_PROVIDER_OPENAI, STT_PROVIDER_SONIOX,
        STT_PROVIDER_VERCEL_GATEWAY, Values, to_settings,
    };

    fn remove(text: &str, language: Option<&str>) -> String {
        remove_filler_words(text, language)
    }

    fn defaults() -> Settings {
        let settings = to_settings(&Values::new(), &HashSet::new()).unwrap();
        Settings { stt_provider_id: STT_PROVIDER_OPENAI.to_string(), ..settings }
    }

    #[test]
    fn universal_fillers_go_with_an_unknown_language() {
        assert_eq!("I think this works", remove("uh I think uhm this works", None));
    }

    #[test]
    fn gated_fillers_go_only_with_their_language() {
        assert_eq!("I think this is good", remove("um I think um this is good", Some("en")));
        assert_eq!("um I think um this is good", remove("um I think um this is good", None));
        assert_eq!("um I think um this is good", remove("um I think um this is good", Some("xx")));
    }

    #[test]
    fn matching_ignores_case_and_carries_the_capital() {
        assert_eq!("This is a test", remove("UHM this is UH a test", Some("en")));
    }

    #[test]
    fn a_filler_eats_its_comma_and_its_period_ends_the_sentence() {
        assert_eq!("Well, I think. that's right", remove("Well, uhm, I think, uh. that's right", None));
        assert_eq!("So I was, thinking about this", remove("  Uhm, so I was, uh, thinking about this  ", None));
        assert_eq!("so, we go", remove("so, uh, we go", None));
    }

    #[test]
    fn sentence_end_period_is_kept() {
        assert_eq!("Let me check.", remove("Let me check, um.", Some("en")));
        assert_eq!("I think. Yes", remove("I think um. Yes", Some("en")));
        assert_eq!("So I think", remove("Um. So I think", Some("en")));
        assert_eq!("That works. Let me check.", remove("That works. Um. let me check.", Some("en")));
    }

    #[test]
    fn question_and_exclamation_marks_end_the_sentence() {
        assert_eq!("What?", remove("Hmm? What?", None));
        assert_eq!("Is it ready?", remove("Is it ready, um?", Some("en")));
        assert_eq!("I see.", remove("Ah! I see.", Some("en")));
    }

    #[test]
    fn an_ellipsis_is_eaten_without_ending_a_sentence() {
        assert_eq!("I think so.", remove("Um... I think so.", Some("en")));
        assert_eq!("I think", remove("Um… I think", Some("en")));
    }

    #[test]
    fn fillers_joined_by_hyphen_or_apostrophe_are_kept() {
        for text in ["Mm-hmm.", "Uh-huh, that's right.", "Uh-oh.", "Um's"] {
            assert_eq!(text, remove(text, Some("en")));
        }
    }

    #[test]
    fn line_breaks_are_kept() {
        assert_eq!("a\n\nb", remove("a\n\nb", None));
        assert_eq!("Hello.\nSo I think", remove("Hello.\nUm, so I think", Some("en")));
        assert_eq!("so\nyes", remove("so uh\nyes", None));
        assert_eq!("a b", remove("a \t b", None));
    }

    #[test]
    fn a_sentence_opening_filler_hands_on_its_capital() {
        assert_eq!("So I think", remove("Um, so I think", Some("en")));
        assert_eq!("So I think we should ship it.", remove("Um, so I think we should ship it.", Some("en")));
        assert_eq!("That works. Let me check.", remove("That works. Um, let me check.", Some("en")));
    }

    #[test]
    fn a_mid_sentence_filler_does_not_capitalize() {
        assert_eq!("He said, not today.", remove("He said, Um, not today.", Some("en")));
        assert_eq!("I was thinking", remove("I was uh thinking", None));
    }

    #[test]
    fn fillers_inside_words_are_kept() {
        assert_eq!("umbrella hummus Uhura ahead", remove("umbrella hummus Uhura ahead", Some("en")));
    }

    #[test]
    fn portuguese_um_is_kept() {
        for language in [None, Some("pt"), Some("pt-BR")] {
            assert_eq!("eu vi um carro", remove("eu vi um carro", language));
        }
    }

    #[test]
    fn german_and_french_fillers_need_their_language() {
        assert_eq!("Ich weiß nicht", remove("Äh, ich weiß nicht", Some("de")));
        assert_eq!("Ich weiß nicht", remove("Äh, ich weiß nicht", Some("DE_at")));
        assert_eq!("je pense", remove("euh je pense", Some("fr")));
        assert_eq!("äh, I see", remove("äh, I see", Some("en")));
    }

    #[test]
    fn cyrillic_fillers_are_whole_words() {
        assert_eq!("Я думаю", remove("Хм, я думаю", None));
        assert_eq!("да", remove("ммм да", None));
        assert_eq!("хмель", remove("хмель", None));
    }

    #[test]
    fn whitespace_collapses_and_is_trimmed() {
        assert_eq!("Hello world test", remove("Hello    world   test", None));
        assert_eq!("Hello world", remove("  Hello world  ", None));
    }

    #[test]
    fn only_fillers_or_punctuation_left_is_blank() {
        assert_eq!("", remove("Uh. Hmm.", None));
        assert_eq!("", remove("Hmm?", None));
        assert_eq!("?", remove("?", None));
    }

    #[test]
    fn gated_filler_candidates() {
        assert!(has_gated_filler("so um yes"));
        assert!(has_gated_filler("Äh"));
        assert!(has_gated_filler("euh"));
        assert!(!has_gated_filler("uh hmm"));
        assert!(!has_gated_filler("umbrella"));
    }

    #[test]
    fn output_language_of_cloud_providers() {
        let defaults = defaults();
        assert_eq!(None, output_language(&defaults));
        assert_eq!(Some("en".into()), output_language(&Settings { stt_language: "EN".into(), ..defaults.clone() }));
        assert_eq!(Some("pt".into()), output_language(&Settings { stt_language: "pt-BR".into(), ..defaults.clone() }));
        for provider in [STT_PROVIDER_DEEPGRAM, STT_PROVIDER_VERCEL_GATEWAY] {
            let settings = Settings { stt_provider_id: provider.into(), stt_language: "de".into(), ..defaults.clone() };
            assert_eq!(Some("de".into()), output_language(&settings));
        }
    }

    #[test]
    fn output_language_of_soniox_needs_exactly_one_code() {
        let soniox = Settings { stt_provider_id: STT_PROVIDER_SONIOX.into(), stt_language: "fr".into(), ..defaults() };
        let with = |languages: &str| Settings { soniox_languages: languages.into(), ..soniox.clone() };
        assert_eq!(Some("en".into()), output_language(&with("en")));
        assert_eq!(Some("en".into()), output_language(&with("en, en")));
        assert_eq!(None, output_language(&with("en, bn")));
        assert_eq!(None, output_language(&with("")));
    }

    #[test]
    fn output_language_on_device() {
        let on_device =
            Settings { stt_provider_id: STT_PROVIDER_ON_DEVICE.into(), stt_language: "de".into(), ..defaults() };
        let with = |model: &str, language: &str| Settings {
            on_device_stt_model: model.into(),
            stt_language: language.into(),
            ..on_device.clone()
        };
        for id in ["nemotron-streaming-en", "parakeet-unified-en", "moonshine-base-en", "unknown"] {
            assert_eq!(Some("en".into()), output_language(&with(id, "de")), "{id}");
        }
        assert_eq!(None, output_language(&with("parakeet-tdt-v3", "en")));
        assert_eq!(Some("de".into()), output_language(&with("whisper-small", "de")));
        assert_eq!(None, output_language(&with("whisper-small", "")));
        assert_eq!(None, output_language(&with("whisper-small", "xx")));
    }
}
