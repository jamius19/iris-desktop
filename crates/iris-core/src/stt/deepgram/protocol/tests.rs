/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `DeepgramProtocolTest.kt`.

use std::collections::BTreeSet;

use super::*;

fn parsed(url: &str) -> url::Url {
    url::Url::parse(url).expect("URL")
}

/// The first value of the query parameter `name`.
fn query(url: &str, name: &str) -> Option<String> {
    query_values(url, name).into_iter().next()
}

fn query_values(url: &str, name: &str) -> Vec<String> {
    parsed(url).query_pairs().filter(|(k, _)| k == name).map(|(_, v)| v.into_owned()).collect()
}

fn query_names(url: &str) -> BTreeSet<String> {
    parsed(url).query_pairs().map(|(k, _)| k.into_owned()).collect()
}

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn streaming_query_states_the_audio_format_and_interim_results() {
    let url = listen_url(LISTEN_URL, "nova-3", "en", true, &[]);

    assert_eq!(Some("api.deepgram.com"), parsed(&url).host_str());
    assert_eq!("/v1/listen", parsed(&url).path());
    assert_eq!(Some("nova-3".into()), query(&url, "model"));
    assert_eq!(Some("en".into()), query(&url, "language"));
    assert_eq!(Some("true".into()), query(&url, "smart_format"));
    assert_eq!(Some("true".into()), query(&url, "punctuate"));
    assert_eq!(Some("linear16".into()), query(&url, "encoding"));
    assert_eq!(Some("16000".into()), query(&url, "sample_rate"));
    assert_eq!(Some("1".into()), query(&url, "channels"));
    assert_eq!(Some("true".into()), query(&url, "interim_results"));
    assert_eq!(None, query(&url, "endpointing"));
}

#[test]
fn blank_language_is_multi_with_endpointing() {
    let url = listen_url(LISTEN_URL, "nova-3", "", true, &[]);

    assert_eq!(Some("multi".into()), query(&url, "language"));
    assert_eq!(Some("100".into()), query(&url, "endpointing"));
}

#[test]
fn upload_query_has_no_streaming_parameters() {
    let url = listen_url(LISTEN_URL, "nova-3", "", false, &[]);

    assert_eq!(names(&["model", "language", "smart_format", "punctuate"]), query_names(&url));
    assert_eq!(Some("multi".into()), query(&url, "language"));
}

#[test]
fn custom_words_are_keyterms_on_nova3() {
    for model in ["nova-3", "nova-3-medical"] {
        let url = listen_url(LISTEN_URL, model, "en", false, &words(&["Deepgram", "Jamius Siam"]));

        assert_eq!(words(&["Deepgram", "Jamius Siam"]), query_values(&url, "keyterm"));
        let encoded = parsed(&url).query().unwrap().to_string();
        assert_eq!(
            "keyterm=Deepgram&keyterm=Jamius%20Siam&redact_usage=true",
            encoded.split_once("punctuate=true&").unwrap().1
        );
    }
}

#[test]
fn no_keyterms_means_no_redact_usage() {
    assert_eq!(None, query(&listen_url(LISTEN_URL, "nova-3", "en", true, &[]), "redact_usage"));
    assert_eq!(None, query(&listen_url(LISTEN_URL, "nova-2", "en", false, &words(&["Deepgram"])), "redact_usage"));
}

#[test]
fn nova2_gets_no_keyterms() {
    let url = listen_url(LISTEN_URL, "nova-2", "en", true, &words(&["Deepgram"]));

    assert_eq!(Vec::<String>::new(), query_values(&url, "keyterm"));
    assert_eq!(0, dropped_keyterms("nova-2", &vec!["x".repeat(60); 100]));
}

#[test]
fn keyterms_over_the_token_budget_are_dropped() {
    // 60 characters = 30 estimated tokens: 15 fit in 450.
    let words: Vec<String> = (0..100).map(|i| format!("{i:x<60}")).collect();

    assert_eq!(&words[..15], deepgram_keyterms(&words));
    assert_eq!(words[..15].to_vec(), query_values(&listen_url(LISTEN_URL, "nova-3", "", false, &words), "keyterm"));
    assert_eq!(85, dropped_keyterms("nova-3", &words));
    assert_eq!(0, dropped_keyterms("nova-3", &self::words(&["Deepgram"])));
}

#[test]
fn no_keyterms_is_todays_url() {
    assert_eq!(listen_url(LISTEN_URL, "nova-3", "en", true, &[]), listen_url(LISTEN_URL, "nova-3", "en", true, &[]));
    assert_eq!(
        names(&["model", "language", "smart_format", "punctuate"]),
        query_names(&listen_url(LISTEN_URL, "nova-3", "", false, &[]))
    );
}

#[test]
fn model_id_is_encoded() {
    let url = listen_url(LISTEN_URL, "nova 3&x=1", "en", false, &[]);

    assert_eq!(Some("nova 3&x=1".into()), query(&url, "model"));
    let encoded = parsed(&url).query().unwrap().to_string();
    assert_eq!("model=nova%203%26x%3D1", encoded.split_once("&language").unwrap().0);
}

#[test]
fn parses_final_and_interim_results() {
    assert_eq!(
        Some(DeepgramEvent::Result { is_final: true, transcript: "Hello there.".into() }),
        parse_deepgram_event(&result("Hello there.", true))
    );
    assert_eq!(
        Some(DeepgramEvent::Result { is_final: false, transcript: "Hello".into() }),
        parse_deepgram_event(&result("Hello", false))
    );
}

#[test]
fn parses_metadata_and_errors() {
    assert_eq!(
        Some(DeepgramEvent::Metadata),
        parse_deepgram_event(r#"{"type":"Metadata","request_id":"r","duration":1.5,"channels":1}"#)
    );
    assert_eq!(
        Some(DeepgramEvent::Error("Could not parse the message".into())),
        parse_deepgram_event(
            r#"{"type":"Error","variant":"SchemaError","description":"Could not parse the message",
               "message":"{oops"}"#
        )
    );
    assert_eq!(
        Some(DeepgramEvent::Error("legacy".into())),
        parse_deepgram_event(r#"{"type":"Error","message":"legacy"}"#)
    );
}

#[test]
fn ignores_unknown_and_malformed_messages() {
    assert_eq!(None, parse_deepgram_event(r#"{"type":"SpeechStarted","channel":[0],"timestamp":1.0}"#));
    assert_eq!(None, parse_deepgram_event("not json"));
    assert_eq!(None, parse_deepgram_event("[1,2]"));
    assert_eq!(None, parse_deepgram_event(r#"{"type":"Results","is_final":true}"#));
    assert_eq!(None, parse_deepgram_event(r#"{"type":"Results","is_final":true,"channel":{"alternatives":[]}}"#));
}

#[test]
fn transcript_joins_finals_and_shows_the_latest_interim() {
    let mut transcript = DeepgramTranscript::default();

    transcript.on_result(false, "Hel");
    assert_eq!("Hel", transcript.live_text());
    transcript.on_result(false, "Hello");
    assert_eq!("Hello", transcript.live_text());
    transcript.on_result(true, "Hello.");
    transcript.on_result(false, "How");
    assert_eq!("Hello. How", transcript.live_text());
    assert_eq!("Hello.", transcript.final_text());
    transcript.on_result(true, " ");
    assert_eq!("Hello.", transcript.live_text());
    transcript.on_result(true, "How are you?");
    assert_eq!("Hello. How are you?", transcript.final_text());
}

#[test]
fn transcript_joins_cjk_unspaced() {
    let mut transcript = DeepgramTranscript::default();

    transcript.on_result(true, "你好");
    transcript.on_result(true, "世界");

    assert_eq!("你好世界", transcript.final_text());
}

#[test]
fn parses_the_pre_recorded_transcript() {
    let body = r#"{"metadata":{"request_id":"r","duration":1.25,"channels":1},
        "results":{"channels":[{"alternatives":[{"transcript":"Yeah, as much as.","confidence":0.98,
        "words":[]}]}]}}"#;

    assert_eq!(Some("Yeah, as much as.".into()), parse_listen_response(body));
    assert_eq!(None, parse_listen_response(r#"{"results":{"channels":[]}}"#));
    assert_eq!(None, parse_listen_response(r#"{"request_id":"r"}"#));
    assert_eq!(None, parse_listen_response("<html>"));
}

#[test]
fn http_errors_map_to_readable_messages() {
    assert_eq!(
        "Deepgram rejected the API key. Check it in Iris settings.",
        deepgram_http_error_message(401, &err_body("INVALID_AUTH", "Invalid credentials."), None)
    );
    assert_eq!("Deepgram account is out of credits", deepgram_http_error_message(402, "", None));
    assert_eq!("Deepgram rate limit reached", deepgram_http_error_message(429, "", None));
    assert_eq!("Deepgram server error (HTTP 503)", deepgram_http_error_message(503, "", None));
    assert_eq!(
        "Deepgram error: No such model/language/tier combination found.",
        deepgram_http_error_message(
            400,
            &err_body("Bad Request", "No such model/language/tier combination found."),
            Some("header message")
        )
    );
    assert_eq!(
        "Deepgram error: Project does not have access to the requested model.",
        deepgram_http_error_message(
            403,
            &err_body("INSUFFICIENT_PERMISSIONS", "Project does not have access to the requested model."),
            None
        )
    );
    assert_eq!(
        "Deepgram error: Invalid JSON submitted.",
        deepgram_http_error_message(400, r#"{"category":"INVALID_JSON","message":"Invalid JSON submitted."}"#, None)
    );
    assert_eq!("Deepgram error: From the header", deepgram_http_error_message(400, "", Some("From the header")));
    assert_eq!("Deepgram request failed (HTTP 400)", deepgram_http_error_message(400, "", None));
}

#[test]
fn close_messages() {
    assert_eq!("Deepgram closed the connection: NET-0001", deepgram_closed_message(1011, "NET-0001"));
    assert_eq!("Deepgram closed the connection (code 1011)", deepgram_closed_message(1011, ""));
}

fn err_body(code: &str, message: &str) -> String {
    format!(r#"{{"err_code":"{code}","err_msg":"{message}"}}"#)
}

fn result(transcript: &str, is_final: bool) -> String {
    format!(
        r#"{{"type":"Results","channel_index":[0,1],"duration":1.0,"start":0.0,"is_final":{is_final},
           "speech_final":{is_final},"channel":{{"alternatives":[{{"transcript":"{transcript}","confidence":0.9,
           "words":[]}}]}}}}"#
    )
}
