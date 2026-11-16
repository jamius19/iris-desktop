/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Overlay window size and placement, from Handy's `overlay.rs` (`is_mouse_within_monitor`,
//! `windows_overlay_bounds`) with its tests. Monitor rectangles and the cursor are physical pixels.

use crate::{OverlayConfig, Position};

/// Gap between the window and the screen edge, logical px (Handy, Windows and Linux).
pub const TOP_OFFSET: f64 = 4.0;
pub const BOTTOM_OFFSET: f64 = 40.0;

// Keep in sync with `overlay.slint`.
const ROW_HEIGHT: f64 = 40.0;
const PANEL_PADDING: f64 = 12.0;
/// At least one line of Inter 15 px (about 18.2 px).
const LINE_HEIGHT: f64 = 20.0;
const NOTICE_MIN_LINES: i32 = 4;
/// Room around the widest card (`overlay.slint` keeps the card 8 px narrower than the window).
const MARGIN: f64 = 4.0;
/// The compact pill (172 px) plus room for a few words.
const MIN_WIDE_WIDTH: f64 = 240.0;

/// The card's fixed width once live text shows, on a monitor `monitor_width` logical px wide: the Width setting's share
/// of it (10–30 %) in whole px, at least `MIN_WIDE_WIDTH`. Also the widest notice.
pub fn wide_width(config: &OverlayConfig, monitor_width: f64) -> f64 {
    (monitor_width * config.width_pct.clamp(10, 30) as f64 / 100.0).floor().max(MIN_WIDE_WIDTH)
}

/// Overlay window size (logical): the wide card and the tallest panel (notices show at least 4 lines).
pub fn overlay_size(config: &OverlayConfig, monitor_width: f64) -> (f64, f64) {
    let lines = config.lines.clamp(1, 6).max(NOTICE_MIN_LINES) as f64;
    (wide_width(config, monitor_width) + 2.0 * MARGIN, ROW_HEIGHT + PANEL_PADDING + lines * LINE_HEIGHT + MARGIN)
}

/// Half-open hit test of `point` against a monitor at `origin` with `size`.
pub fn is_within_monitor(point: (i32, i32), origin: (i32, i32), size: (u32, u32)) -> bool {
    point.0 >= origin.0
        && point.0 < origin.0 + size.0 as i32
        && point.1 >= origin.1
        && point.1 < origin.1 + size.1 as i32
}

/// The window rectangle `(x, y, width, height)` in the destination monitor's physical pixels: centred, `TOP_OFFSET` /
/// `BOTTOM_OFFSET` from the edge. The window grows with `text_scale`; the offsets ride the DPI `scale` alone.
pub fn place_on_monitor(
    origin: (i32, i32),
    size: (u32, u32),
    scale: f64,
    text_scale: f64,
    logical_size: (f64, f64),
    position: Position,
) -> (i32, i32, i32, i32) {
    let content_scale = scale * text_scale;
    let width = (logical_size.0 * content_scale).round().max(1.0) as i32;
    let height = (logical_size.1 * content_scale).round().max(1.0) as i32;
    let x = (origin.0 as f64 + (size.0 as f64 - width as f64) / 2.0).round() as i32;
    let y = match position {
        Position::Top => (origin.1 as f64 + TOP_OFFSET * scale).round() as i32,
        Position::Bottom => (origin.1 as f64 + size.1 as f64 - height as f64 - BOTTOM_OFFSET * scale).round() as i32,
    };
    (x, y, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Handy's window sizes, used by its placement tests.
    const OVERLAY_WIDTH: f64 = 266.0;
    const OVERLAY_HEIGHT: f64 = 50.0;
    const OVERLAY_STREAM_WIDTH: f64 = 400.0;
    const OVERLAY_STREAM_HEIGHT: f64 = 120.0;

    #[test]
    fn monitor_hit_test_uses_half_open_physical_bounds() {
        let origin = (-2560, -200);
        let size = (2560, 1440);

        assert!(is_within_monitor((-2560, -200), origin, size));
        assert!(is_within_monitor((-1, 1239), origin, size));
        assert!(!is_within_monitor((0, 0), origin, size));
        assert!(!is_within_monitor((-1, 1240), origin, size));
    }

    #[test]
    fn windows_cursor_hit_test_does_not_scale_physical_monitor_bounds() {
        let origin = (1920, 0);
        let size = (3840, 2160);
        let cursor = (5000, 1000);

        assert!(is_within_monitor(cursor, origin, size));

        // The old mixed-coordinate comparison excludes a cursor visibly inside a secondary display at 150 %.
        let scale = 1.5;
        let logical_origin = ((origin.0 as f64 / scale) as i32, (origin.1 as f64 / scale) as i32);
        let logical_size = ((size.0 as f64 / scale) as u32, (size.1 as f64 / scale) as u32);
        assert!(!is_within_monitor(cursor, logical_origin, logical_size));
    }

    #[test]
    fn windows_overlay_bounds_use_destination_monitor_scale() {
        let origin = (1920, 0);
        let size = (3840, 2160);

        assert_eq!(
            place_on_monitor(origin, size, 1.5, 1.0, (OVERLAY_WIDTH, OVERLAY_HEIGHT), Position::Bottom),
            (3641, 2025, 399, 75)
        );
        assert_eq!(
            place_on_monitor(origin, size, 1.5, 1.0, (OVERLAY_WIDTH, OVERLAY_HEIGHT), Position::Top),
            (3641, 6, 399, 75)
        );
    }

    #[test]
    fn windows_overlay_bounds_support_negative_monitor_origins() {
        assert_eq!(
            place_on_monitor(
                (-2560, -200),
                (2560, 1440),
                1.25,
                1.0,
                (OVERLAY_STREAM_WIDTH, OVERLAY_STREAM_HEIGHT),
                Position::Bottom,
            ),
            (-1530, 1040, 500, 150)
        );
    }

    #[test]
    fn windows_overlay_bounds_grow_with_text_scale_without_moving_the_anchored_edge() {
        let origin = (-2560, -200);
        let size = (2560, 1440);

        let (x, y, width, height) =
            place_on_monitor(origin, size, 1.25, 1.1, (OVERLAY_STREAM_WIDTH, OVERLAY_STREAM_HEIGHT), Position::Bottom);
        // 400x120 logical at 1.25 DPI x 1.1 text, still centred horizontally.
        assert_eq!((x, y, width, height), (-1555, 1025, 550, 165));
        // Bottom edge unchanged from the 1.0 case above (1040 + 150).
        assert_eq!(y + height, 1190);

        let (_, top_y, _, _) =
            place_on_monitor(origin, size, 1.25, 1.1, (OVERLAY_STREAM_WIDTH, OVERLAY_STREAM_HEIGHT), Position::Top);
        // The top offset rides the DPI scale alone, so the top edge doesn't move.
        assert_eq!(top_y, -195);
    }

    fn width(width_pct: i32) -> OverlayConfig {
        OverlayConfig { width_pct, ..OverlayConfig::default() }
    }

    #[test]
    fn wide_width_is_the_width_share_of_the_monitor_in_whole_px_with_a_floor() {
        // The default 14 %, about a seventh.
        assert_eq!(wide_width(&OverlayConfig::default(), 1920.0), 268.0);
        assert_eq!(wide_width(&width(14), 2560.0), 358.0);
        assert_eq!(wide_width(&width(30), 1920.0), 576.0);
        assert_eq!(wide_width(&width(10), 3840.0), 384.0);
        // Never narrower than the floor.
        assert_eq!(wide_width(&width(10), 1920.0), 240.0);
        assert_eq!(wide_width(&width(14), 1366.0), 240.0);
        assert_eq!(wide_width(&width(14), 0.0), 240.0);
    }

    #[test]
    fn wide_width_clamps_the_share_to_10_to_30_percent() {
        // A hand-edited or Android-sized value can't make the card half the screen.
        assert_eq!(wide_width(&width(50), 1920.0), 576.0);
        assert_eq!(wide_width(&width(0), 3840.0), 384.0);
    }

    #[test]
    fn overlay_size_fits_the_wide_card_and_a_four_line_notice() {
        let config = OverlayConfig { lines: 2, ..OverlayConfig::default() };
        assert_eq!(overlay_size(&config, 1920.0), (276.0, 136.0));
        let config = OverlayConfig { width_pct: 30, ..config };
        assert_eq!(overlay_size(&config, 1920.0), (584.0, 136.0));
        // Never narrower than the floor; six lines.
        let config = OverlayConfig { lines: 6, ..OverlayConfig::default() };
        assert_eq!(overlay_size(&config, 640.0), (248.0, 176.0));
    }
}
