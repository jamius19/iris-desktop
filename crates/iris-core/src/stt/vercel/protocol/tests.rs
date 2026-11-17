/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `GatewayTranscriptionProtocolTest.kt`.

use super::*;

fn assert_json(expected: &str, actual: &str) {
    let expected: Value = serde_json::from_str(expected).unwrap();
    let actual: Value = serde_json::from_str(actual).unwrap();
    assert_eq!(expected, actual);
}

/// `options` as a JSON string (Kotlin's `toString()` of a nullable object).
fn options(model: &str, language: &str, streaming: bool) -> String {
    Value::Object(provider_options(model, language, streaming).expect("options")).to_string()
}

fn model(id: &str, r#type: Option<&str>, tags: &[&str]) -> GatewayModel {
    GatewayModel {
        id: id.to_string(),
        r#type: r#type.map(str::to_string),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        reasoning_efforts: None,
    }
}

#[test]
fn start_frame_without_options() {
    assert_json(
        r#"{"type":"transcription-stream.start","inputAudioFormat":{"type":"audio/pcm","rate":16000}}"#,
        &start_frame(16_000, None),
    );
}

#[test]
fn start_frame_with_options() {
    assert_json(
        r#"{"type":"transcription-stream.start","inputAudioFormat":{"type":"audio/pcm","rate":24000},
           "providerOptions":{"openai":{"language":"en"}}}"#,
        &start_frame(24_000, provider_options("openai/gpt-live-transcribe", "en", true).as_ref()),
    );
}

#[test]
fn audio_done_frame() {
    assert_json(r#"{"type":"transcription-stream.audio-done"}"#, AUDIO_DONE);
}

#[test]
fn language_option_per_provider() {
    assert_json(r#"{"openai":{"language":"fr"}}"#, &options("openai/gpt-transcribe", " fr ", true));
    assert_json(r#"{"mistral":{"language":"fr"}}"#, &options("mistral/voxtral", "fr", true));
    assert_json(r#"{"elevenlabs":{"languageCode":"fr"}}"#, &options("elevenlabs/scribe-v2-realtime", "fr", true));
    assert_json(r#"{"google":{"languageCodes":["fr"]}}"#, &options("google/gemini-3.5-transcribe-live", "fr", true));
    assert_eq!(None, provider_options("microsoft/mai-transcribe-2-streaming", "fr", true));
}

#[test]
fn upload_options_list_gpt_transcribes_language_and_skip_grok_interim_results() {
    assert_json(r#"{"openai":{"languages":["fr"]}}"#, &options("openai/gpt-transcribe", "fr", false));
    assert_json(r#"{"openai":{"language":"fr"}}"#, &options("openai/gpt-4o-transcribe", "fr", false));
    assert_json(r#"{"elevenlabs":{"languageCode":"fr"}}"#, &options("elevenlabs/scribe-v2", "fr", false));
    assert_eq!(None, provider_options("spacexai/grok-stt", "en", false));
}

#[test]
fn blank_language_sends_no_options() {
    assert_eq!(None, provider_options("openai/gpt-live-transcribe", " ", true));
    assert_eq!(None, provider_options("google/gemini-3.5-transcribe-live", "", true));
}

#[test]
fn grok_always_asks_for_interim_results_under_both_names() {
    let interim = r#"{"xai":{"streaming":{"interimResults":true}},"spacexai":{"streaming":{"interimResults":true}}}"#;
    assert_json(interim, &options("spacexai/grok-stt", "", true));
    assert_json(interim, &options("xai/grok-stt", "en", true));
}

#[test]
fn sample_rate_per_model() {
    assert_eq!(24_000, gateway_sample_rate("openai/gpt-live-transcribe"));
    assert_eq!(24_000, gateway_sample_rate("openai/gpt-realtime-whisper"));
    assert_eq!(16_000, gateway_sample_rate("google/gemini-3.5-transcribe-live"));
    assert_eq!(16_000, gateway_sample_rate("spacexai/grok-stt"));
}

#[test]
fn bare_model_id_counts_as_open_ai() {
    assert_eq!(24_000, gateway_sample_rate("gpt-live-transcribe"));
    assert_json(r#"{"openai":{"language":"en"}}"#, &options("gpt-live-transcribe", "en", true));
}

#[test]
fn stream_url_encodes_the_model_id() {
    assert_eq!(
        "https://ai-gateway.vercel.sh/v4/ai/transcription-model?ai-model-id=openai%2Fgpt-live-transcribe",
        stream_url(TRANSCRIPTION_URL, "openai/gpt-live-transcribe")
    );
}

#[test]
fn streaming_filter_keeps_transcription_models_tagged_for_the_web_socket() {
    let catalog = [
        model("elevenlabs/scribe-v2", Some("transcription"), &[]),
        model("openai/gpt-6-luna", Some("language"), &[]),
        model("openai/gpt-live-transcribe", Some("transcription"), &[STREAMING_TAG]),
        model("openai/gpt-realtime-whisper", Some("transcription"), &["websocket-realtime", STREAMING_TAG]),
        model("openai/whisper-1", Some("transcription"), &["open-weights"]),
        model("x/untyped", None, &[STREAMING_TAG]),
    ];

    let ids: Vec<_> = catalog.iter().filter(|m| is_streaming_transcription_model(m)).map(|m| m.id.as_str()).collect();

    assert_eq!(vec!["openai/gpt-live-transcribe", "openai/gpt-realtime-whisper"], ids);
}

#[test]
fn all_transcription_models_are_listed_and_the_tag_picks_live_or_after_stop() {
    let catalog = [
        model("elevenlabs/scribe-v2", Some("transcription"), &[]),
        model("openai/gpt-6-luna", Some("language"), &[]),
        model("openai/gpt-live-transcribe", Some("transcription"), &[STREAMING_TAG]),
    ];

    let ids: Vec<_> = catalog.iter().filter(|m| is_transcription_model(m)).map(|m| m.id.as_str()).collect();

    assert_eq!(vec!["elevenlabs/scribe-v2", "openai/gpt-live-transcribe"], ids);
    assert!(gateway_stt_live("openai/gpt-live-transcribe", Some(&catalog)));
    assert!(!gateway_stt_live(" elevenlabs/scribe-v2 ", Some(&catalog)));
    // Not in the catalog, or no catalog: live.
    assert!(gateway_stt_live("acme/custom-stt", Some(&catalog)));
    assert!(gateway_stt_live("elevenlabs/scribe-v2", None));
}

#[test]
fn upload_body_is_json_with_the_base64_wav() {
    // Lengths 0, 1 and 2 mod 3, and more than Android's 48 KiB slice.
    for size in [0, 1, 2, 3, 48 * 1024, 48 * 1024 + 1, 100_000] {
        let wav: Vec<u8> = (0..size).map(|i| (i * 7) as u8).collect();
        let options = provider_options("openai/gpt-transcribe", "fr", false);

        let body = upload_body(&wav, options.as_ref());

        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(wav, STANDARD.decode(json["audio"].as_str().unwrap()).unwrap());
        assert_eq!("audio/wav", json["mediaType"]);
        assert_eq!(options.map(Value::Object), json.get("providerOptions").cloned());
    }
    let plain = String::from_utf8(upload_body(&[1], None)).unwrap();
    assert_eq!(r#"{"audio":"AQ==","mediaType":"audio/wav"}"#, plain);
}

#[test]
fn parses_upload_response() {
    assert_eq!(
        Some(("Hello.".to_string(), vec!["\"Unsupported option\"".to_string()])),
        parse_upload_response(
            r#"{"text":"Hello.","segments":[],"language":"en","durationInSeconds":1.2,
               "warnings":["Unsupported option"]}"#
        )
    );
    assert_eq!(Some((String::new(), Vec::new())), parse_upload_response(r#"{"text":""}"#));
    assert_eq!(None, parse_upload_response(r#"{"segments":[]}"#));
    assert_eq!(None, parse_upload_response("not json"));
}

#[test]
fn parses_parts() {
    // Warnings are compared as JSON: serde_json may order an object's keys differently from the server.
    let Some(GatewayPart::StreamStart { warnings }) =
        parse_gateway_part(r#"{"type":"stream-start","warnings":[{"type":"unsupported","feature":"language"}]}"#)
    else {
        panic!("not a stream-start");
    };
    assert_eq!(1, warnings.len());
    assert_json(r#"{"type":"unsupported","feature":"language"}"#, &warnings[0]);
    assert_eq!(
        Some(GatewayPart::Delta("Hi".into())),
        parse_gateway_part(r#"{"type":"transcript-delta","id":"1","delta":"Hi"}"#)
    );
    assert_eq!(
        Some(GatewayPart::Partial { text: "Hi there".into(), start_second: Some(1.5), duration_in_seconds: Some(2.0) }),
        parse_gateway_part(
            r#"{"type":"transcript-partial","text":"Hi there","startSecond":1.5,"durationInSeconds":2}"#
        )
    );
    assert_eq!(
        Some(GatewayPart::Partial { text: "Hi".into(), start_second: None, duration_in_seconds: None }),
        parse_gateway_part(r#"{"type":"transcript-partial","text":"Hi"}"#)
    );
    assert_eq!(
        Some(GatewayPart::Final("Hi.".into())),
        parse_gateway_part(r#"{"type":"transcript-final","text":"Hi."}"#)
    );
    assert_eq!(
        Some(GatewayPart::Finish("Hi.".into())),
        parse_gateway_part(r#"{"type":"finish","text":"Hi.","segments":[],"language":"en"}"#)
    );
}

#[test]
fn parses_error_object_or_string() {
    assert_eq!(
        Some(GatewayPart::Error("AI Gateway error: Upstream failed".into())),
        parse_gateway_part(r#"{"type":"error","error":{"name":"Error","message":"Upstream failed"}}"#)
    );
    assert_eq!(
        Some(GatewayPart::Error("AI Gateway error: Boom".into())),
        parse_gateway_part(r#"{"type":"error","error":"Boom"}"#)
    );
    assert_eq!(
        Some(GatewayPart::Error("AI Gateway error".into())),
        parse_gateway_part(r#"{"type":"error","error":{}}"#)
    );
}

#[test]
fn ignores_unknown_malformed_and_incomplete_parts() {
    assert_eq!(None, parse_gateway_part(r#"{"type":"response-metadata","modelId":"x"}"#));
    assert_eq!(None, parse_gateway_part(r#"{"type":"raw","rawValue":{}}"#));
    assert_eq!(None, parse_gateway_part(r#"{"type":"something-new"}"#));
    assert_eq!(None, parse_gateway_part("not json"));
    assert_eq!(None, parse_gateway_part("[1,2]"));
    assert_eq!(None, parse_gateway_part(r#"{"type":"transcript-delta"}"#));
    assert_eq!(None, parse_gateway_part(r#"{"type":"transcript-delta","delta":3}"#));
    assert_eq!(None, parse_gateway_part(r#"{"type":"transcript-partial"}"#));
    assert_eq!(None, parse_gateway_part(r#"{"type":"transcript-final","text":null}"#));
    assert_eq!(None, parse_gateway_part(r#"{"type":"error"}"#));
}

#[test]
fn deltas_append() {
    let mut transcript = LiveTranscript::new();
    transcript.on_delta("Hello");
    transcript.on_delta(" world");
    assert_eq!("Hello world", transcript.text());
}

#[test]
fn gemini_partial_repeating_the_deltas_is_shown_once() {
    let mut transcript = LiveTranscript::new();
    transcript.on_delta("Hello");
    transcript.on_partial("Hello world", None, None);
    assert_eq!("Hello world", transcript.text());
}

#[test]
fn mai_partial_after_deltas_is_appended() {
    let mut transcript = LiveTranscript::new();
    transcript.on_delta("Hello ");
    transcript.on_partial("world", None, None);
    assert_eq!("Hello world", transcript.text());
}

#[test]
fn grok_chunked_partials_advance_but_revisions_replace() {
    let mut transcript = LiveTranscript::new();
    transcript.on_partial("one two", Some(0.0), Some(3.0));
    transcript.on_partial("one two three", Some(0.0), Some(3.2)); // Same start: a revision.
    assert_eq!("one two three", transcript.text());
    transcript.on_partial("four five", Some(3.2), Some(2.0)); // Starts where the last one ended: a new chunk.
    assert_eq!("one two three four five", transcript.text());
    transcript.on_partial("four five six", Some(3.2), Some(2.5));
    assert_eq!("one two three four five six", transcript.text());
}

#[test]
fn final_settles_and_clears_the_segment() {
    let mut transcript = LiveTranscript::new();
    transcript.on_delta("Hello wor");
    transcript.on_final(" Hello world. ");
    assert_eq!("Hello world.", transcript.text());
    transcript.on_partial("Next", Some(0.0), Some(1.0));
    assert_eq!("Hello world. Next", transcript.text());
    transcript.on_final("Next one.");
    transcript.on_final(" ");
    assert_eq!("Hello world. Next one.", transcript.text());
}

#[test]
fn cjk_joins_without_spaces() {
    let mut transcript = LiveTranscript::new();
    transcript.on_final("你好");
    transcript.on_final("世界");
    assert_eq!("你好世界", transcript.text());
    assert_eq!("a b", joined("a", "b"));
    assert_eq!("ab", joined("", "ab"));
    assert_eq!("ab", joined("ab", ""));
}

#[test]
fn only_trims_detects_cut_words() {
    assert!(only_trims("one two three", "one two"));
    assert!(!only_trims("one two", "one two"));
    assert!(!only_trims("one two", "one tw0 three"));
    assert!(!only_trims("one two three", "one too"));
}
