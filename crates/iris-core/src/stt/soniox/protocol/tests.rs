/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `SonioxProtocolTest.kt`.

use super::*;

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn token(text: &str, is_final: bool) -> SonioxToken {
    SonioxToken { text: text.into(), is_final }
}

#[test]
fn regions_map_their_ids_and_unknown_is_us() {
    assert_eq!(SonioxRegion::Us, soniox_region("us"));
    assert_eq!(SonioxRegion::Eu, soniox_region("eu"));
    assert_eq!(SonioxRegion::Jp, soniox_region("jp"));
    assert_eq!(SonioxRegion::In, soniox_region("in"));
    assert_eq!(SonioxRegion::Us, soniox_region("mars"));
    assert_eq!(SonioxRegion::Us, soniox_region(""));
    assert_eq!("https://api.eu.soniox.com", SonioxRegion::Eu.rest_url());
    assert_eq!("https://stt-rt.eu.soniox.com/transcribe-websocket", SonioxRegion::Eu.web_socket_url());
}

#[test]
fn language_hints_are_trimmed_lowercased_and_unique() {
    assert_eq!(strings(&["en", "bn"]), soniox_language_hints(" en, BN,,en "));
    assert!(soniox_language_hints("").is_empty());
    assert!(soniox_language_hints(" , ").is_empty());
}

#[test]
fn start_message_states_the_raw_format() {
    assert_eq!(
        json(r#"{"model":"stt-rt-v6","audio_format":"pcm_s16le","sample_rate":16000,"num_channels":1}"#),
        json(&soniox_start_message("stt-rt-v6", &[], true, &[]))
    );
}

#[test]
fn start_message_hints_and_strict_only_when_asked() {
    assert_eq!(
        json(
            r#"{"model":"stt-rt-v6","audio_format":"pcm_s16le","sample_rate":16000,"num_channels":1,
               "language_hints":["en","bn"],"language_hints_strict":true}"#
        ),
        json(&soniox_start_message("stt-rt-v6", &strings(&["en", "bn"]), true, &[]))
    );
    assert_eq!(
        json(
            r#"{"model":"stt-rt-v6","audio_format":"pcm_s16le","sample_rate":16000,"num_channels":1,
               "language_hints":["en"]}"#
        ),
        json(&soniox_start_message("stt-rt-v6", &strings(&["en"]), false, &[]))
    );
}

#[test]
fn create_body_has_the_file_and_the_same_hint_rules() {
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1"}"#),
        json(&soniox_create_transcription_body("stt-async-v6", "f1", &[], true, &[]))
    );
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1","language_hints":["en"],"language_hints_strict":true}"#),
        json(&soniox_create_transcription_body("stt-async-v6", "f1", &strings(&["en"]), true, &[]))
    );
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1","language_hints":["en"]}"#),
        json(&soniox_create_transcription_body("stt-async-v6", "f1", &strings(&["en"]), false, &[]))
    );
}

#[test]
fn custom_words_go_in_the_context_terms_of_both_messages() {
    assert_eq!(
        json(
            r#"{"model":"stt-rt-v6","audio_format":"pcm_s16le","sample_rate":16000,"num_channels":1,
               "context":{"terms":["Soniox","Jamius Siam"]}}"#
        ),
        json(&soniox_start_message("stt-rt-v6", &[], false, &strings(&["Soniox", "Jamius Siam"])))
    );
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1","language_hints":["en"],"context":{"terms":["Soniox"]}}"#),
        json(&soniox_create_transcription_body("stt-async-v6", "f1", &strings(&["en"]), false, &strings(&["Soniox"])))
    );
}

#[test]
fn no_custom_words_no_context() {
    assert_eq!(
        json(r#"{"model":"stt-rt-v6","audio_format":"pcm_s16le","sample_rate":16000,"num_channels":1}"#),
        json(&soniox_start_message("stt-rt-v6", &[], false, &[]))
    );
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1"}"#),
        json(&soniox_create_transcription_body("stt-async-v6", "f1", &[], false, &[]))
    );
}

#[test]
fn tokens_are_parsed_without_markers() {
    let event = parse_soniox_event(
        r#"{"tokens":[{"text":"Hello","start_ms":600,"is_final":true},{"text":" wor","is_final":false},
           {"text":"<fin>","is_final":true},{"text":"<end>","is_final":true},{"start_ms":1},{"text":7},"x",
           {"text":" there"}],"final_audio_proc_ms":760,"total_audio_proc_ms":880}"#,
    );

    assert_eq!(
        Some(SonioxEvent::Tokens {
            tokens: vec![token("Hello", true), token(" wor", false), token(" there", false)],
            finished: false,
        }),
        event
    );
}

#[test]
fn finished_response_is_parsed() {
    assert_eq!(
        Some(SonioxEvent::Tokens { tokens: vec![], finished: true }),
        parse_soniox_event(r#"{"tokens":[],"final_audio_proc_ms":1560,"total_audio_proc_ms":1680,"finished":true}"#)
    );
}

#[test]
fn error_frame_is_parsed() {
    assert_eq!(
        Some(SonioxEvent::Error { code: 401, message: Some("Invalid API key.".into()) }),
        parse_soniox_event(
            r#"{"tokens":[],"error_code":401,"error_type":"unauthenticated","error_message":"Invalid API key.",
               "request_id":"r"}"#
        )
    );
    assert_eq!(
        Some(SonioxEvent::Error { code: 503, message: None }),
        parse_soniox_event(r#"{"tokens":[],"error_code":503}"#)
    );
}

#[test]
fn malformed_or_unknown_messages_are_ignored() {
    for text in ["not json", "[1]", "\"x\"", r#"{"finished":true}"#, r#"{"tokens":{}}"#] {
        assert_eq!(None, parse_soniox_event(text), "{text}");
    }
}

#[test]
fn transcript_appends_finals_and_replaces_the_tail() {
    let mut transcript = SonioxTranscript::default();

    transcript.on_tokens(&[token("How", true), token(" a", false)]);
    assert_eq!("How a", transcript.live_text());
    assert_eq!("How", transcript.final_text());

    transcript.on_tokens(&[token(" are", true), token(" you", false), token("?", false)]);
    assert_eq!("How are you?", transcript.live_text());
    assert_eq!("How are", transcript.final_text());

    transcript.on_tokens(&[token(" you?", true), token(" ", true)]);
    assert_eq!("How are you?", transcript.live_text());
    assert_eq!("How are you?", transcript.final_text());
}

#[test]
fn error_messages_map_by_code() {
    assert_eq!("Soniox rejected the API key. Check it in Iris settings.", soniox_error_message(401, Some("x")));
    assert_eq!("Soniox account is out of credits or over budget", soniox_error_message(402, None));
    assert_eq!("Soniox API key doesn't have permission for this", soniox_error_message(403, None));
    assert_eq!("Soniox limit reached: Too many requests", soniox_error_message(429, Some("Too many requests")));
    assert_eq!("Soniox limit reached", soniox_error_message(429, None));
    assert_eq!("Soniox server error (503)", soniox_error_message(503, Some("Cannot continue request")));
    assert_eq!("Soniox error: Invalid language hint.", soniox_error_message(400, Some("Invalid language hint.")));
    assert_eq!("Soniox request failed (408)", soniox_error_message(408, None));
}

#[test]
fn http_error_bodies_map() {
    assert_eq!(
        "Soniox rejected the API key. Check it in Iris settings.",
        soniox_http_error_message(401, r#"{"status_code":401,"error_type":"unauthenticated","message":"Bad key"}"#)
    );
    assert_eq!(
        "Soniox error: Model not found",
        soniox_http_error_message(
            400,
            r#"{"status_code":400,"error_type":"invalid_request","message":"Model not found",
               "validation_errors":[]}"#
        )
    );
    assert_eq!(
        "Soniox error: Invalid language hint.",
        soniox_http_error_message(
            400,
            r#"{"status_code":400,"error_type":"invalid_request","message":"",
               "validation_errors":[{"error_type":"invalid","location":"language_hints","message":
               "Invalid language hint."}]}"#
        )
    );
    assert_eq!("Soniox request failed (400)", soniox_http_error_message(400, "<html>Bad request</html>"));
    assert_eq!("Soniox server error (502)", soniox_http_error_message(502, ""));
}

#[test]
fn no_audio_error_is_told_apart_by_its_message() {
    assert!(is_no_audio_error(400, Some("No audio received.")));
    assert!(!is_no_audio_error(400, Some("Invalid language hint.")));
    assert!(!is_no_audio_error(400, None));
    assert!(!is_no_audio_error(408, Some("No audio received.")));
}

// Reduced: Android's "Network error: IOException" (an exception without a message) can't happen; a `NetError`
// always has one.
#[test]
fn closed_and_network_messages() {
    assert_eq!("Soniox closed the connection: Going away", soniox_closed_message(1001, "Going away"));
    assert_eq!("Soniox closed the connection (code 1006)", soniox_closed_message(1006, ""));
    assert_eq!("Network error: timeout", soniox_network_error_message(&NetError::timeout()));
}

#[test]
fn rest_helpers_parse_their_fields() {
    assert_eq!(Some("f1".into()), parse_soniox_id(r#"{"id":"f1","filename":"audio.wav","size":10}"#));
    assert_eq!(None, parse_soniox_id(r#"{"id":1}"#));
    assert_eq!(None, parse_soniox_id("nope"));
    assert_eq!(
        Some(SonioxJob { status: "queued".into(), error_message: None }),
        parse_soniox_job(r#"{"id":"t1","status":"queued"}"#)
    );
    assert_eq!(
        Some(SonioxJob { status: "error".into(), error_message: Some("Audio can't be decoded".into()) }),
        parse_soniox_job(
            r#"{"status":"error","error_type":"invalid_audio_file","error_message":"Audio can't be decoded"}"#
        )
    );
    assert_eq!(
        Some(SonioxJob { status: "failed".into(), error_message: Some("file_not_found".into()) }),
        parse_soniox_job(r#"{"status":"failed","error_type":"file_not_found","error_message":null}"#)
    );
    assert_eq!(None, parse_soniox_job(r#"{"id":"t1"}"#));
    assert_eq!(Some("Hello".into()), parse_soniox_transcript_text(r#"{"id":"t1","text":"Hello","tokens":[]}"#));
    assert_eq!(None, parse_soniox_transcript_text(r#"{"id":"t1"}"#));
}

#[test]
fn job_failure_statuses() {
    assert!(is_soniox_job_failed("error"));
    assert!(is_soniox_job_failed("failed"));
    assert!(!is_soniox_job_failed("completed"));
    assert!(!is_soniox_job_failed("queued"));
    assert!(!is_soniox_job_failed("processing"));
}
