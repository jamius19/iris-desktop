/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `RealtimeProtocolTest.kt`.

use super::*;

#[track_caller]
fn assert_json(expected: &str, actual: &str) {
    assert_eq!(serde_json::from_str::<Value>(expected).unwrap(), serde_json::from_str::<Value>(actual).unwrap());
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

#[test]
fn session_update_without_language() {
    assert_json(
        r#"{"type":"session.update","session":{"type":"transcription","audio":{"input":{
          "format":{"type":"audio/pcm","rate":24000},
          "transcription":{"model":"gpt-live-transcribe"},
          "turn_detection":null}}}}"#,
        &session_update("gpt-live-transcribe", " ", &[]),
    );
}

#[test]
fn session_update_with_language_uses_languages_array_only() {
    assert_json(
        r#"{"type":"session.update","session":{"type":"transcription","audio":{"input":{
          "format":{"type":"audio/pcm","rate":24000},
          "transcription":{"model":"gpt-live-transcribe","languages":["fr"]},
          "turn_detection":null}}}}"#,
        &session_update("gpt-live-transcribe", "fr", &[]),
    );
}

#[test]
fn session_update_uses_singular_language_for_models_without_the_list() {
    assert_json(
        r#"{"type":"session.update","session":{"type":"transcription","audio":{"input":{
          "format":{"type":"audio/pcm","rate":24000},
          "transcription":{"model":"gpt-realtime-whisper","language":"fr"},
          "turn_detection":null}}}}"#,
        &session_update("gpt-realtime-whisper", "fr", &[]),
    );
    let transcribe = session_update("gpt-transcribe", "fr", &[]);
    assert!(transcribe.contains(r#""languages":["fr"]"#));
    assert!(!transcribe.contains(r#""language":"#));
}

/// The `transcription` object of `message`.
fn transcription(message: &str) -> String {
    let message: Value = serde_json::from_str(message).unwrap();
    message["session"]["audio"]["input"]["transcription"].to_string()
}

#[test]
fn custom_words_go_as_keywords_for_the_gpt_transcribe_models() {
    for model in ["gpt-live-transcribe", "gpt-transcribe"] {
        assert_json(
            &format!(r#"{{"model":"{model}","keywords":["Deepgram","Jamius Siam"]}}"#),
            &transcription(&session_update(model, "", &words(&["Deepgram", "Jamius Siam"]))),
        );
    }
}

#[test]
fn custom_words_go_as_a_prompt_for_whisper_and_gpt4o() {
    for model in ["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe-2025-12-15"] {
        assert_json(
            &format!(r#"{{"model":"{model}","prompt":"Glossary: A, B"}}"#),
            &transcription(&session_update(model, "", &words(&["A", "B"]))),
        );
    }
}

#[test]
fn custom_words_are_left_out_for_models_without_an_option() {
    for model in ["gpt-4o-transcribe-diarize", "gpt-realtime-whisper", "my-model"] {
        assert_json(&format!(r#"{{"model":"{model}"}}"#), &transcription(&session_update(model, "", &words(&["A"]))));
    }
}

#[test]
fn no_custom_words_is_todays_message() {
    for model in ["gpt-live-transcribe", "whisper-1"] {
        assert_json(&format!(r#"{{"model":"{model}"}}"#), &transcription(&session_update(model, "", &[])));
    }
}

#[test]
fn realtime_models_stream_other_ids_upload() {
    for model in ["gpt-live-transcribe", " gpt-transcribe ", "gpt-realtime-whisper", "whisper-1", "gpt-4o-transcribe"] {
        assert!(is_realtime_transcription_model(model), "{model}");
    }
    for model in ["gpt-4o-transcribe-2025-03-20", "my-model", ""] {
        assert!(!is_realtime_transcription_model(model), "{model}");
    }
}

#[test]
fn only_live_models_have_live_text() {
    for model in ["gpt-live-transcribe", " gpt-realtime-whisper "] {
        assert!(has_live_text(model), "{model}");
    }
    for model in ["gpt-transcribe", "whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe", "my-model", ""] {
        assert!(!has_live_text(model), "{model}");
    }
}

#[test]
fn http_error_messages() {
    assert_eq!("OpenAI rejected the API key. Check it in Iris settings.", openai_http_error_message(401, ""));
    assert_eq!("OpenAI rejected the API key. Check it in Iris settings.", openai_http_error_message(403, ""));
    assert_eq!("OpenAI rate limit or quota reached", openai_http_error_message(429, ""));
    assert_eq!("OpenAI server error (HTTP 503)", openai_http_error_message(503, ""));
    assert_eq!(
        "OpenAI error: Invalid file format.",
        openai_http_error_message(400, r#"{"error":{"message":"Invalid file format.","type":"invalid_request"}}"#)
    );
    assert_eq!("OpenAI request failed (HTTP 400)", openai_http_error_message(400, "<html>"));
}

#[test]
fn append_and_commit_messages() {
    assert_json(r#"{"type":"input_audio_buffer.append","audio":"AQID/w=="}"#, &append_audio(&[1, 2, 3, 0xff]));
    assert_json(r#"{"type":"input_audio_buffer.commit"}"#, COMMIT_AUDIO);
}

#[test]
fn parses_transcript_events() {
    assert_eq!(
        Some(RealtimeEvent::Delta { item_id: "item_1".into(), delta: "Hello,".into() }),
        parse_server_event(
            r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"item_1",
               "content_index":0,"delta":"Hello,"}"#
        )
    );
    assert_eq!(
        Some(RealtimeEvent::Completed { item_id: "item_1".into(), transcript: "Hello, how are you?".into() }),
        parse_server_event(
            r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_1",
               "content_index":0,"transcript":"Hello, how are you?","usage":{"type":"duration","seconds":1.2}}"#
        )
    );
    assert_eq!(
        Some(RealtimeEvent::Committed { item_id: "item_1".into() }),
        parse_server_event(r#"{"type":"input_audio_buffer.committed","item_id":"item_1","previous_item_id":null}"#)
    );
    assert_eq!(None, parse_server_event(r#"{"type":"session.updated","session":{}}"#));
}

#[test]
fn maps_errors() {
    assert_eq!(
        Some(RealtimeEvent::Failed("OpenAI error: Invalid language code".into())),
        parse_server_event(
            r#"{"type":"error","event_id":"e1","error":{"type":"invalid_request_error",
               "code":"invalid_value","message":"Invalid language code","param":null,"event_id":null}}"#
        )
    );
    assert_eq!(
        Some(RealtimeEvent::Failed("Transcription failed: Audio too short".into())),
        parse_server_event(
            r#"{"type":"conversation.item.input_audio_transcription.failed","item_id":"item_1",
               "content_index":0,"error":{"type":"server_error","message":"Audio too short"}}"#
        )
    );
    assert_eq!("OpenAI rejected the API key. Check it in Iris settings.", connection_failure_message(Some(401), ""));
    assert_eq!("Network error: timeout", connection_failure_message(None, "timeout"));
}

#[test]
fn malformed_events_are_ignored_or_readable() {
    assert_eq!(None, parse_server_event("not json"));
    assert_eq!(None, parse_server_event("[1,2]"));
    assert_eq!(None, parse_server_event(r#"{"type":"input_audio_buffer.committed"}"#));
    assert_eq!(
        None,
        parse_server_event(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":null}"#)
    );
    assert_eq!(None, parse_server_event(r#"{"type":{"nested":true}}"#));
    assert_eq!(
        Some(RealtimeEvent::Failed("OpenAI error: unknown error".into())),
        parse_server_event(r#"{"type":"error","error":"not an object"}"#)
    );
}

#[test]
fn assembler_concatenates_deltas_per_item() {
    let mut transcript = TranscriptAssembler::default();
    transcript.on_delta("a", "Hello");
    transcript.on_delta("a", " wor");
    transcript.on_delta("a", "ld");
    assert_eq!("Hello world", transcript.live_text());
    assert_eq!(None, transcript.final_text());
}

#[test]
fn final_text_waits_for_committed_item_to_complete() {
    let mut transcript = TranscriptAssembler::default();
    transcript.on_delta("a", "Hello wrld");
    transcript.on_committed("a");
    assert_eq!(None, transcript.final_text());
    transcript.on_completed("a", "Hello world.");
    transcript.on_delta("a", " late");
    assert_eq!("Hello world.", transcript.live_text());
    assert_eq!(Some("Hello world.".into()), transcript.final_text());
}

#[test]
fn multiple_items_join_in_first_seen_order() {
    let mut transcript = TranscriptAssembler::default();
    transcript.on_delta("a", "First.");
    transcript.on_delta("b", " Second");
    transcript.on_committed("b");
    transcript.on_completed("b", "Second.");
    assert_eq!("First. Second.", transcript.live_text());
    // "a" never completed, so it is left out of the final text.
    assert_eq!(Some("Second.".into()), transcript.final_text());
    transcript.on_completed("a", "First.");
    assert_eq!(Some("First. Second.".into()), transcript.final_text());
}
