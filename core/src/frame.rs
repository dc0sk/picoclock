//! The 24x8 LED matrix frame buffer and text rendering, ported from the Waveshare C demo's `display_char`.
//!
//! Layout: the buffer is groups of 8 bytes (one byte per matrix row, one group per 8-column driver chip).
//! Byte `[8*g + r]` holds the 8 pixels of row `r` in group `g`; byte 0 of each group is unused by the glyphs
//! and carries status/weekday LEDs. Only the first four groups are physically shown; the rest is scroll space.

use crate::font::{GLYPH_ROWS, ZIKU};

pub const FRAME_BYTES: usize = 112;
/// Columns taken by the status LEDs on the left of the matrix.
pub const COLUMN_OFFSET: usize = 2;

#[derive(Clone, Copy)]
pub struct Frame(pub [u8; FRAME_BYTES]);

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

fn glyph_index(c: char) -> usize {
    match c {
        '0'..='9' => c as usize - '0' as usize,
        'A'..='F' => c as usize - 'A' as usize + 10,
        'H' => 16,
        'L' => 17,
        'N' => 18,
        'P' => 19,
        'U' => 20,
        ':' => 21,
        'T' => 25,
        '.' => 26,
        '-' => 27,
        'M' => 28,
        'O' => 0,
        '/' => 29,
        _ => 24, // blank
    }
}

/// (byte index, bit mask) pairs lighting the two weekday LEDs, Monday first.
const WEEKDAY_LEDS: [[(usize, u8); 2]; 7] = [
    [(0, 1 << 3), (0, 1 << 4)],
    [(0, 1 << 6), (0, 1 << 7)],
    [(8, 1 << 1), (8, 1 << 2)],
    [(8, 1 << 4), (8, 1 << 5)],
    [(8, 1 << 7), (16, 1 << 0)],
    [(16, 1 << 2), (16, 1 << 3)],
    [(16, 1 << 5), (16, 1 << 6)],
];

/// Status LEDs (the icons down the left edge of the panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Move,
    Alarm,
    Countdown,
    Fahrenheit,
    Celsius,
    Am,
    Pm,
    Countup,
    Chime,
    AutoLight,
}

impl Icon {
    /// (byte index, bit mask) from the demo's `define.h`.
    fn bits(self) -> (usize, u8) {
        match self {
            Icon::Move => (0, 0x03),
            Icon::Alarm => (1, 0x03),
            Icon::Countdown => (2, 0x03),
            Icon::Fahrenheit => (3, 0x01),
            Icon::Celsius => (3, 0x02),
            Icon::Am => (4, 0x01),
            Icon::Pm => (4, 0x02),
            Icon::Countup => (5, 0x03),
            Icon::Chime => (6, 0x03),
            Icon::AutoLight => (7, 0x03),
        }
    }
}

impl Frame {
    pub const fn new() -> Self {
        Frame([0; FRAME_BYTES])
    }

    /// Draws one glyph with its left edge at column `x` (after the status-LED offset), preserving neighbours.
    pub fn put_char(&mut self, x: usize, c: char) {
        self.put_raw(x, glyph_index(c));
    }

    fn put_raw(&mut self, x: usize, index: usize) {
        let x = x + COLUMN_OFFSET;
        let (j, k) = (x / 8, x % 8);
        let g = index * GLYPH_ROWS;
        for i in 1..8usize {
            let row = ZIKU[g + i - 1];
            if k > 0 {
                let a = 8 * j + i;
                self.0[a] = (self.0[a] & (0xFFu8 >> (8 - k))) | (row << k);
                if j < FRAME_BYTES / 8 - 1 {
                    let b = 8 * j + 8 + i;
                    self.0[b] = (self.0[b] & (0xFFu8 << (8 - k))) | (row >> (8 - k));
                }
            } else {
                self.0[8 * j + i] = row;
            }
        }
    }

    /// Left-aligned text, 5 columns per character.
    pub fn put_str(&mut self, x: usize, text: &str) {
        for (i, c) in text.chars().enumerate() {
            self.put_char(x + 5 * i, c);
        }
    }

    /// Draws a glyph by table index (for symbols that share a character with a hex digit, like the degree signs).
    pub fn put_glyph(&mut self, x: usize, index: usize) {
        self.put_raw(x, index);
    }

    pub fn clear_matrix(&mut self) {
        for g in 0..4 {
            for r in 1..8 {
                self.0[8 * g + r] &= if g == 0 { 0x03 } else { 0 };
            }
        }
    }

    /// `HH:MM` using the demo's glyph spacing. With `colon` false the colon is blanked (for blinking).
    pub fn put_time(&mut self, hour: u8, minute: u8, colon: bool) {
        self.put_char(0, (b'0' + hour / 10) as char);
        self.put_char(5, (b'0' + hour % 10) as char);
        self.put_char(10, if colon { ':' } else { ' ' });
        self.put_char(13, (b'0' + minute / 10) as char);
        self.put_char(18, (b'0' + minute % 10) as char);
    }

    pub fn put_dashes(&mut self) {
        for (x, c) in [(0, '-'), (5, '-'), (10, ':'), (13, '-'), (18, '-')] {
            self.put_char(x, c);
        }
    }

    /// Lights the LEDs for ISO weekday `wd` (1 = Monday) and clears the others.
    pub fn set_weekday(&mut self, wd: u8) {
        for (i, leds) in WEEKDAY_LEDS.iter().enumerate() {
            for &(byte, mask) in leds {
                if i + 1 == wd as usize {
                    self.0[byte] |= mask;
                } else {
                    self.0[byte] &= !mask;
                }
            }
        }
    }

    pub fn set_icon(&mut self, icon: Icon, on: bool) {
        let (byte, mask) = icon.bits();
        if on {
            self.0[byte] |= mask;
        } else {
            self.0[byte] &= !mask;
        }
    }

    /// The four bytes shifted out for matrix row `row` (0..8), first chip first.
    pub fn row_bytes(&self, row: usize) -> [u8; 4] {
        [self.0[row], self.0[8 + row], self.0[16 + row], self.0[24 + row]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_table_has_all_glyphs() {
        assert_eq!(ZIKU.len(), 30 * GLYPH_ROWS);
    }

    #[test]
    fn zero_at_column_zero_is_shifted_by_status_offset() {
        let mut f = Frame::new();
        f.put_char(0, '0');
        // Glyph row 0 of '0' is 0x06; shifted left by COLUMN_OFFSET.
        assert_eq!(f.0[1], 0x06 << COLUMN_OFFSET);
        assert_eq!(f.0[7], 0x06 << COLUMN_OFFSET);
    }

    #[test]
    fn glyph_spanning_two_groups_keeps_neighbours() {
        let mut f = Frame::new();
        f.0[1] = 0b0111_1111; // pixels left of the glyph in group 0
        f.0[9] = 0xFF; // pixels right of the glyph in group 1
        f.put_char(5, '1'); // column 7 after the offset: k = 7, glyph row 0 of '1' is 0x04
        // Group 0 keeps its low 7 bits; bit 7 gets glyph bit 0 (0).
        assert_eq!(f.0[1], 0b0111_1111);
        // Group 1 gets glyph bits 1.. in its low bits; the untouched high bits stay set.
        assert_eq!(f.0[9], 0xFF & (0xFF << 1) | (0x04 >> 1));
    }

    #[test]
    fn time_render_leaves_led_row_alone() {
        let mut f = Frame::new();
        f.put_time(12, 34, true);
        // Row 0 of each chip carries only weekday/status LEDs; glyphs occupy rows 1..=7.
        assert_eq!([f.0[0], f.0[8], f.0[16], f.0[24]], [0; 4]);
        assert_ne!(f.row_bytes(1), [0; 4]);
    }

    #[test]
    fn icons_survive_time_render() {
        let mut f = Frame::new();
        f.set_icon(Icon::Alarm, true);
        f.put_time(88, 88, true);
        assert_eq!(f.0[1] & 0x03, 0x03);
    }

    #[test]
    fn weekday_leds_are_exclusive() {
        let mut f = Frame::new();
        for wd in 1..=7 {
            f.set_weekday(wd);
            let lit: u32 = [f.0[0], f.0[8], f.0[16]].iter().map(|b| b.count_ones()).sum();
            assert_eq!(lit, 2, "weekday {wd}");
        }
    }
}
