/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Provider options as typed in Settings (Android's `ProviderOptions.kt`).

use serde_json::{Map, Value};

/// None when [text] is blank, else the JSON object it holds; Err with a short user-readable message when it isn't
/// valid JSON or isn't an object. serde_json is strict JSON already (no comments, trailing commas, unquoted keys or
/// values, NaN), which Android checks by hand.
pub fn parse_provider_options(text: &str) -> Result<Option<Map<String, Value>>, &'static str> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str(text) {
        Ok(Value::Object(object)) => Ok(Some(object)),
        Ok(_) => Err("Must be a JSON object: { ... }"),
        Err(_) => Err("Not valid JSON"),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_provider_options;

    // Android ProviderOptionsTest.

    #[test]
    fn blank_is_null() {
        assert_eq!(Ok(None), parse_provider_options(""));
        assert_eq!(Ok(None), parse_provider_options(" \n\t "));
    }

    #[test]
    fn invalid_json_fails() {
        let texts = [
            "{",
            "{a:1}",
            r#"{"a":1,}"#,
            "{} x",
            "{'a':1}",
            r#"{"order": [openai]}"#,
            r#"{"a": True}"#,
            r#"{"a": NaN}"#,
            r#"{"a": 1e}"#,
            r#"{"a": 01}"#,
            r#"{"a": tru}"#,
        ];
        for text in texts {
            assert_eq!(Err("Not valid JSON"), parse_provider_options(text), "{text}");
        }
    }

    #[test]
    fn non_objects_fail() {
        for text in ["[1]", "1", "\"x\"", "true", "null"] {
            assert_eq!(Err("Must be a JSON object: { ... }"), parse_provider_options(text), "{text}");
        }
    }
}
