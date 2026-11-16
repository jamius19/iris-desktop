/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Text language detection for the gated filler words (Android's `LanguageDetector.kt`, on `whatlang` instead of the
//! system TextClassifier).

/// whatlang's languages (ISO 639-3) with their ISO 639-1 codes.
const ISO_639_1: [(&str, &str); 70] = [
    ("epo", "eo"),
    ("eng", "en"),
    ("rus", "ru"),
    ("cmn", "zh"),
    ("spa", "es"),
    ("por", "pt"),
    ("ita", "it"),
    ("ben", "bn"),
    ("fra", "fr"),
    ("deu", "de"),
    ("ukr", "uk"),
    ("kat", "ka"),
    ("ara", "ar"),
    ("hin", "hi"),
    ("jpn", "ja"),
    ("heb", "he"),
    ("yid", "yi"),
    ("pol", "pl"),
    ("amh", "am"),
    ("jav", "jv"),
    ("kor", "ko"),
    ("nob", "nb"),
    ("dan", "da"),
    ("swe", "sv"),
    ("fin", "fi"),
    ("tur", "tr"),
    ("nld", "nl"),
    ("hun", "hu"),
    ("ces", "cs"),
    ("ell", "el"),
    ("bul", "bg"),
    ("bel", "be"),
    ("mar", "mr"),
    ("kan", "kn"),
    ("ron", "ro"),
    ("slv", "sl"),
    ("hrv", "hr"),
    ("srp", "sr"),
    ("mkd", "mk"),
    ("lit", "lt"),
    ("lav", "lv"),
    ("est", "et"),
    ("tam", "ta"),
    ("vie", "vi"),
    ("urd", "ur"),
    ("tha", "th"),
    ("guj", "gu"),
    ("uzb", "uz"),
    ("pan", "pa"),
    ("aze", "az"),
    ("ind", "id"),
    ("tel", "te"),
    ("pes", "fa"),
    ("mal", "ml"),
    ("ori", "or"),
    ("mya", "my"),
    ("nep", "ne"),
    ("sin", "si"),
    ("khm", "km"),
    ("tuk", "tk"),
    ("aka", "ak"),
    ("zul", "zu"),
    ("sna", "sn"),
    ("afr", "af"),
    ("lat", "la"),
    ("slk", "sk"),
    ("cat", "ca"),
    ("tgl", "tl"),
    ("hye", "hy"),
    ("cym", "cy"),
];

/// `text`'s language (ISO 639-1) if whatlang calls its detection reliable, else None.
pub fn detect_language(text: &str) -> Option<String> {
    let info = whatlang::detect(text).filter(|info| info.is_reliable())?;
    ISO_639_1.iter().find(|(code, _)| *code == info.lang().code()).map(|(_, iso)| iso.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reliable_detections_give_the_two_letter_code() {
        let english = "I think we should ship this feature next week, after the team has reviewed the changes.";
        assert_eq!(Some("en".to_string()), detect_language(english));
        let german =
            "Ich glaube, wir sollten diese Funktion nächste Woche veröffentlichen, nachdem das Team sie geprüft hat.";
        assert_eq!(Some("de".to_string()), detect_language(german));
        let french =
            "Je pense que nous devrions publier cette fonctionnalité la semaine prochaine, après la relecture.";
        assert_eq!(Some("fr".to_string()), detect_language(french));
    }

    #[test]
    fn unreliable_or_empty_text_is_none() {
        assert_eq!(None, detect_language(""));
        assert_eq!(None, detect_language("um"));
        assert_eq!(None, detect_language("12345 !!!"));
    }

    #[test]
    fn every_whatlang_language_has_a_code() {
        for lang in whatlang::Lang::all() {
            assert!(ISO_639_1.iter().any(|(code, _)| *code == lang.code()), "{lang:?}");
        }
    }
}
