//! Maps an [`rmux_sdk::PaneSnapshot`] onto a synthesized ANSI byte stream.
//!
//! RMUX panes are rendered as a full-screen external terminal surface: rather
//! than poking Warp's grid cells directly, we synthesize an ANSI escape
//! sequence that reproduces the captured snapshot exactly (glyphs, colors,
//! attributes, cursor) and feed it through the same `ansi::Processor` /
//! `GridHandler` pipeline that local and remote PTYs already use. This keeps
//! a single, well-tested code path responsible for turning terminal content
//! into Warp grid cells.

use rmux_sdk::{PaneAttributes, PaneCell, PaneColor, PaneCursor, PaneSnapshot};

/// Disables autowrap, resets style, clears the screen, and homes the cursor.
const PREAMBLE: &[u8] = b"\x1b[?7l\x1b[0m\x1b[2J\x1b[H";

/// Renders a full [`PaneSnapshot`] as a self-contained ANSI byte sequence.
///
/// The output always starts from a clean slate (clear screen, reset style,
/// disabled autowrap) so repeated calls never accumulate stale state from a
/// prior render in the destination grid handler.
pub fn render_pane_snapshot_as_ansi(snapshot: &PaneSnapshot) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREAMBLE.len() + snapshot.cells.len() * 4);
    out.extend_from_slice(PREAMBLE);

    let mut last_style: Option<(PaneColor, PaneColor, PaneAttributes)> = None;
    for row in 0..snapshot.rows {
        if row > 0 {
            out.extend_from_slice(b"\r\n");
        }
        let Some(cells) = snapshot.row_cells(row) else {
            continue;
        };
        for cell in cells {
            if cell.is_padding() {
                // The leading glyph's own width already accounts for this
                // trailing column; Warp's grid handler derives wide-char
                // spacers itself from the glyph's rendered width.
                continue;
            }
            write_cell(&mut out, cell, &mut last_style);
        }
    }

    write_cursor(&mut out, &snapshot.cursor);
    out
}

fn write_cell(
    out: &mut Vec<u8>,
    cell: &PaneCell,
    last_style: &mut Option<(PaneColor, PaneColor, PaneAttributes)>,
) {
    let style = (cell.foreground, cell.background, cell.attributes);
    if *last_style != Some(style) {
        out.extend_from_slice(&sgr_sequence(
            cell.foreground,
            cell.background,
            cell.attributes,
        ));
        *last_style = Some(style);
    }
    out.extend_from_slice(cell.text().as_bytes());
}

fn sgr_sequence(fg: PaneColor, bg: PaneColor, attrs: PaneAttributes) -> Vec<u8> {
    let mut codes: Vec<String> = vec!["0".to_owned()];
    codes.extend(attribute_codes(attrs));
    codes.push(color_code(fg, true));
    codes.push(color_code(bg, false));
    format!("\x1b[{}m", codes.join(";")).into_bytes()
}

fn attribute_codes(attrs: PaneAttributes) -> Vec<String> {
    let mut codes = Vec::new();
    if attrs.contains(PaneAttributes::BOLD) {
        codes.push("1".to_owned());
    }
    if attrs.contains(PaneAttributes::DIM) {
        codes.push("2".to_owned());
    }
    if attrs.contains(PaneAttributes::ITALIC) {
        codes.push("3".to_owned());
    }
    if !(attrs & PaneAttributes::ALL_UNDERSCORE).is_empty() {
        codes.push("4".to_owned());
    }
    if attrs.contains(PaneAttributes::BLINK) {
        codes.push("5".to_owned());
    }
    if attrs.contains(PaneAttributes::REVERSE) {
        codes.push("7".to_owned());
    }
    if attrs.contains(PaneAttributes::HIDDEN) {
        codes.push("8".to_owned());
    }
    if attrs.contains(PaneAttributes::STRIKETHROUGH) {
        codes.push("9".to_owned());
    }
    codes
}

/// Returns the SGR color code for `color`, using the 30-series/40-series
/// bases selected by `is_foreground`.
fn color_code(color: PaneColor, is_foreground: bool) -> String {
    let (default_code, base, bright_base) = if is_foreground {
        (39, 30, 90)
    } else {
        (49, 40, 100)
    };
    match color {
        PaneColor::Default | PaneColor::None | PaneColor::Terminal => default_code.to_string(),
        PaneColor::Ansi { index } => (base + u32::from(index.min(7))).to_string(),
        PaneColor::BrightAnsi { index } => (bright_base + u32::from(index.min(7))).to_string(),
        PaneColor::Indexed { index } => {
            format!("{};5;{index}", if is_foreground { 38 } else { 48 })
        }
        PaneColor::Rgb { red, green, blue } => {
            format!(
                "{};2;{red};{green};{blue}",
                if is_foreground { 38 } else { 48 }
            )
        }
        // `PaneColor` is `#[non_exhaustive]`; treat any unmodeled/future
        // encoding (including `Encoded`) as the terminal default.
        _ => default_code.to_string(),
    }
}

fn write_cursor(out: &mut Vec<u8>, cursor: &PaneCursor) {
    out.extend_from_slice(format!("\x1b[{};{}H", cursor.row + 1, cursor.col + 1).as_bytes());
    out.extend_from_slice(if cursor.visible {
        b"\x1b[?25h"
    } else {
        b"\x1b[?25l"
    });
}

#[cfg(test)]
mod tests {
    use rmux_sdk::{PaneCell, PaneCursor, PaneGlyph, PaneSnapshot};

    use super::*;

    fn snapshot_from_rows(cols: u16, rows: &[Vec<PaneCell>]) -> PaneSnapshot {
        let mut cells = Vec::new();
        for row in rows {
            assert_eq!(row.len(), cols as usize);
            cells.extend(row.iter().cloned());
        }
        PaneSnapshot::new(cols, rows.len() as u16, cells, PaneCursor::default()).unwrap()
    }

    fn as_text(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("ansi bytes must be valid utf8")
    }

    #[test]
    fn renders_plain_glyphs() {
        let snapshot = snapshot_from_rows(
            2,
            &[vec![
                PaneCell::new(PaneGlyph::new("a", 1)),
                PaneCell::new(PaneGlyph::new("b", 1)),
            ]],
        );
        let text = as_text(&render_pane_snapshot_as_ansi(&snapshot));
        assert!(text.contains("ab"), "expected glyph text in {text:?}");
    }

    #[test]
    fn skips_wide_char_padding_cells() {
        let mut wide = PaneCell::new(PaneGlyph::new("\u{4e2d}", 2));
        wide.foreground = PaneColor::Default;
        let snapshot = snapshot_from_rows(2, &[vec![wide, PaneCell::padding()]]);
        let text = as_text(&render_pane_snapshot_as_ansi(&snapshot));
        // The wide glyph's text appears exactly once; the padding cell must
        // not contribute a second, spurious character.
        assert_eq!(text.matches('\u{4e2d}').count(), 1);
    }

    #[test]
    fn maps_ansi_indexed_rgb_and_default_colors() {
        let mut ansi_cell = PaneCell::new(PaneGlyph::new("a", 1));
        ansi_cell.foreground = PaneColor::ansi(3);
        let mut indexed_cell = PaneCell::new(PaneGlyph::new("b", 1));
        indexed_cell.background = PaneColor::indexed(200);
        let mut rgb_cell = PaneCell::new(PaneGlyph::new("c", 1));
        rgb_cell.foreground = PaneColor::rgb(10, 20, 30);
        let default_cell = PaneCell::new(PaneGlyph::new("d", 1));

        let snapshot =
            snapshot_from_rows(4, &[vec![ansi_cell, indexed_cell, rgb_cell, default_cell]]);
        let text = as_text(&render_pane_snapshot_as_ansi(&snapshot));

        assert!(text.contains(";33;"), "expected ansi fg code in {text:?}");
        assert!(text.contains("48;5;200"), "expected indexed bg in {text:?}");
        assert!(
            text.contains("38;2;10;20;30"),
            "expected rgb fg in {text:?}"
        );
        assert!(
            text.contains(";39;49m"),
            "expected default colors in {text:?}"
        );
    }

    #[test]
    fn maps_attributes_to_sgr_codes() {
        let mut cell = PaneCell::new(PaneGlyph::new("x", 1));
        cell.attributes =
            PaneAttributes::BOLD | PaneAttributes::UNDERLINE | PaneAttributes::REVERSE;
        let snapshot = snapshot_from_rows(1, &[vec![cell]]);
        let text = as_text(&render_pane_snapshot_as_ansi(&snapshot));
        assert!(text.contains(";1;"), "expected bold in {text:?}");
        assert!(text.contains(";4;"), "expected underline in {text:?}");
        assert!(text.contains(";7;"), "expected reverse in {text:?}");
    }

    #[test]
    fn only_emits_sgr_on_style_change() {
        let a = PaneCell::new(PaneGlyph::new("a", 1));
        let b = PaneCell::new(PaneGlyph::new("b", 1));
        let snapshot = snapshot_from_rows(2, &[vec![a, b]]);
        let text = as_text(&render_pane_snapshot_as_ansi(&snapshot));
        // Both cells share the default style, so there must be exactly one
        // SGR sequence before the glyph text (plus none afterwards).
        assert_eq!(text.matches("\x1b[0;").count(), 1);
    }

    #[test]
    fn renders_cursor_position_and_visibility() {
        let cell = PaneCell::new(PaneGlyph::new("a", 1));
        let mut snapshot = snapshot_from_rows(1, &[vec![cell]]);
        snapshot.cursor = PaneCursor::new(0, 0, false, 0);
        let text = as_text(&render_pane_snapshot_as_ansi(&snapshot));
        assert!(text.ends_with("\x1b[1;1H\x1b[?25l"));
    }
}
