/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Dropdown placement, slider math and text-field commit rules (Android's `DropdownPlacement.kt`, `SliderMath.kt`,
//! `Autosave.kt`).

use std::ops::RangeInclusive;

/// A dropdown panel's top [y] and the height it may use, all in window pixels; [above] = it opens upwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropdownPlacement {
    pub y: i32,
    pub max_height: i32,
    pub above: bool,
}

/// Places a panel of [content_height] [gap] below the anchor if it fits above [max_bottom] (the window's bottom minus a
/// margin), else on the side with more room (above down to [min_top]), its height capped to that room.
pub fn dropdown_placement(
    anchor_top: i32,
    anchor_bottom: i32,
    min_top: i32,
    max_bottom: i32,
    content_height: i32,
    gap: i32,
) -> DropdownPlacement {
    let room_below = max_bottom - (anchor_bottom + gap);
    let room_above = anchor_top - gap - min_top;
    if content_height <= room_below || room_below >= room_above {
        return DropdownPlacement {
            y: anchor_bottom + gap,
            max_height: content_height.min(room_below).max(0),
            above: false,
        };
    }
    let height = content_height.min(room_above).max(0);
    DropdownPlacement { y: anchor_top - gap - height, max_height: height, above: true }
}

/// [models] whose id contains the trimmed [query] (case-insensitive); all of them for a blank query, none if None.
pub fn filter_models(models: Option<&[String]>, query: &str) -> Vec<String> {
    let query = query.trim().to_lowercase();
    models.unwrap_or_default().iter().filter(|m| m.to_lowercase().contains(&query)).cloned().collect()
}

/// Where [value] sits in [range], 0..1.
pub fn slider_fraction(value: i32, range: RangeInclusive<i32>) -> f32 {
    let (first, last) = range.into_inner();
    if last == first {
        return 0.0;
    }
    ((value - first) as f32 / (last - first) as f32).clamp(0.0, 1.0)
}

/// The value at [fraction] (clamped to 0..1), snapped to `first + k * step` and kept within [range].
pub fn slider_value(fraction: f32, range: RangeInclusive<i32>, step: i32) -> i32 {
    let (first, last) = range.into_inner();
    let raw = fraction.clamp(0.0, 1.0) * (last - first) as f32;
    // Half up, as Kotlin's roundToInt.
    let steps = (raw / step as f32 + 0.5).floor() as i32;
    (first + steps * step).clamp(first, last)
}

/// What committing a text field's draft does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Commit {
    /// The trimmed draft equals the stored value.
    None,
    Save(String),
    /// Not saved; the error goes in the field's helper slot.
    Invalid(String),
}

/// Trims [draft]: unchanged from [stored] = [`Commit::None`], else [validate] (None = valid) decides.
pub fn commit_decision(draft: &str, stored: &str, validate: impl FnOnce(&str) -> Option<String>) -> Commit {
    let trimmed = draft.trim();
    if trimmed == stored {
        return Commit::None;
    }
    match validate(trimmed) {
        Some(error) => Commit::Invalid(error),
        None => Commit::Save(trimmed.to_string()),
    }
}

#[cfg(test)]
mod dropdown_placement_tests {
    use super::*;

    fn placement(y: i32, max_height: i32, above: bool) -> DropdownPlacement {
        DropdownPlacement { y, max_height, above }
    }

    #[test]
    fn fits_below() {
        assert_eq!(placement(210, 300, false), dropdown_placement(100, 200, 50, 2000, 300, 10));
    }

    #[test]
    fn more_room_above_opens_above_with_capped_height() {
        // Below: 1800 - 1710 = 90; above: 1590 - 50 = 1540.
        assert_eq!(placement(1290, 300, true), dropdown_placement(1600, 1700, 50, 1800, 300, 10));
        assert_eq!(placement(50, 240, true), dropdown_placement(300, 400, 50, 500, 1000, 10));
    }

    #[test]
    fn neither_fits_takes_the_larger_side_capped() {
        // Below: 900 - 510 = 390; above: 390 - 100 = 290.
        assert_eq!(placement(510, 390, false), dropdown_placement(400, 500, 100, 900, 1000, 10));
    }

    #[test]
    fn ime_raises_the_bottom_limit() {
        let no_ime = dropdown_placement(1000, 1100, 50, 2300, 280, 10);
        let with_ime = dropdown_placement(1000, 1100, 50, 1300, 280, 10);
        assert_eq!(placement(1110, 280, false), no_ime);
        assert_eq!(placement(710, 280, true), with_ime);
    }

    #[test]
    fn anchor_below_the_limit_gets_no_negative_height() {
        assert_eq!(placement(30, 0, true), dropdown_placement(40, 80, 50, 60, 280, 10));
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filter_is_case_insensitive_and_trimmed() {
        let models = strings(&["openai/gpt-live-transcribe", "openai/whisper-1", "elevenlabs/scribe-v2"]);
        assert_eq!(strings(&["openai/gpt-live-transcribe"]), filter_models(Some(&models), " LIVE "));
        assert_eq!(strings(&["elevenlabs/scribe-v2"]), filter_models(Some(&models), "ElevenLabs"));
    }

    #[test]
    fn blank_query_shows_all_and_null_models_none() {
        let models = strings(&["a", "b"]);
        assert_eq!(models, filter_models(Some(&models), ""));
        assert_eq!(models, filter_models(Some(&models), "  "));
        assert_eq!(Vec::<String>::new(), filter_models(None, "a"));
    }
}

#[cfg(test)]
mod slider_math_tests {
    use super::*;

    #[test]
    fn fraction_of_first_last_and_middle() {
        assert_eq!(0.0, slider_fraction(100, 100..=200));
        assert_eq!(1.0, slider_fraction(200, 100..=200));
        assert_eq!(0.5, slider_fraction(150, 100..=200));
    }

    #[test]
    fn fraction_of_out_of_range_value_is_clamped() {
        assert_eq!(0.0, slider_fraction(50, 100..=200));
        assert_eq!(1.0, slider_fraction(250, 100..=200));
    }

    #[test]
    fn fraction_of_single_value_range_is_zero() {
        assert_eq!(0.0, slider_fraction(5, 5..=5));
    }

    #[test]
    fn value_at_ends_and_beyond() {
        assert_eq!(100, slider_value(0.0, 100..=200, 10));
        assert_eq!(200, slider_value(1.0, 100..=200, 10));
        assert_eq!(100, slider_value(-0.3, 100..=200, 10));
        assert_eq!(200, slider_value(1.4, 100..=200, 10));
    }

    #[test]
    fn value_snaps_to_nearest_step() {
        assert_eq!(150, slider_value(0.54, 100..=200, 10));
        assert_eq!(160, slider_value(0.56, 100..=200, 10));
    }

    #[test]
    fn value_with_step_one() {
        assert_eq!(1, slider_value(0.0, 1..=6, 1));
        assert_eq!(3, slider_value(0.45, 1..=6, 1));
        assert_eq!(6, slider_value(1.0, 1..=6, 1));
    }

    #[test]
    fn non_divisible_range_stays_in_range() {
        assert_eq!(9, slider_value(1.0, 1..=10, 4));
        assert_eq!(5, slider_value(0.5, 1..=10, 4));
    }
}

#[cfg(test)]
mod autosave_tests {
    use super::*;

    fn no_blank(value: &str) -> Option<String> {
        value.trim().is_empty().then(|| "Can't be empty.".to_string())
    }

    #[test]
    fn unchanged_draft_is_none() {
        assert_eq!(Commit::None, commit_decision("en", "en", |_| None));
    }

    #[test]
    fn whitespace_only_change_is_none() {
        assert_eq!(Commit::None, commit_decision("  en \n", "en", |_| None));
    }

    #[test]
    fn changed_draft_is_saved_trimmed() {
        assert_eq!(Commit::Save("bn".into()), commit_decision(" bn ", "en", |_| None));
    }

    #[test]
    fn validator_error_is_invalid() {
        assert_eq!(Commit::Invalid("Bad".into()), commit_decision("x", "en", |_| Some("Bad".into())));
    }

    #[test]
    fn validator_sees_the_trimmed_value() {
        let mut seen = None;
        commit_decision("  x  ", "en", |value| {
            seen = Some(value.to_string());
            None
        });
        assert_eq!(Some("x".to_string()), seen);
    }

    #[test]
    fn blank_is_saved_when_allowed() {
        assert_eq!(Commit::Save(String::new()), commit_decision("   ", "en", |_| None));
    }

    #[test]
    fn blank_is_invalid_when_rejected() {
        assert_eq!(Commit::Invalid("Can't be empty.".into()), commit_decision("   ", "en", no_blank));
    }
}
