//! Morse code for the clock's HAM mode: the time as four digits (HHMM), sent as a list of tone-on / tone-off periods that the
//! buzzer task plays. Standard timing: a dash is 3 dits, the gap inside a digit 1 dit, between digits 3 dits; the speed in
//! words per minute follows the PARIS convention (one dit = 1200 / wpm ms).

use heapless::Vec;

pub const MIN_WPM: u8 = 5;
pub const MAX_WPM: u8 = 40;

/// One dit in ms at the given speed.
pub fn dit_ms(wpm: u8) -> u16 {
    1200 / wpm.clamp(MIN_WPM, MAX_WPM) as u16
}

/// The code of a digit: `.` dit, `-` dash. Five elements each.
pub fn digit_code(d: u8) -> &'static str {
    ["-----", ".----", "..---", "...--", "....-", ".....", "-....", "--...", "---..", "----."][(d % 10) as usize]
}

/// The tone periods for `digits`: `(true, ms)` is tone, `(false, ms)` silence. Ends with the last tone (no trailing gap).
pub fn timeline(digits: &[u8; 4], dit_ms: u16) -> Vec<(bool, u16), 48> {
    let mut out = Vec::new();
    for (i, d) in digits.iter().enumerate() {
        if i > 0 {
            let _ = out.push((false, 3 * dit_ms));
        }
        for (j, c) in digit_code(*d).chars().enumerate() {
            if j > 0 {
                let _ = out.push((false, dit_ms));
            }
            let _ = out.push((true, if c == '-' { 3 * dit_ms } else { dit_ms }));
        }
    }
    out
}

/// HHMM of a local time as four digits.
pub fn hhmm(hour: u8, minute: u8) -> [u8; 4] {
    [hour / 10, hour % 10, minute / 10, minute % 10]
}

#[cfg(test)]
mod tests {
    use super::*;

    // VERIFIES: REQ-HAM-01
    #[test]
    fn digits_are_standard_morse() {
        let codes: std::vec::Vec<_> = (0..10).map(digit_code).collect();
        assert_eq!(codes, ["-----", ".----", "..---", "...--", "....-", ".....", "-....", "--...", "---..", "----."]);
        // every digit has five elements and the digits differ
        assert!(codes.iter().all(|c| c.len() == 5));
        assert_eq!(codes.iter().collect::<std::collections::HashSet<_>>().len(), 10);
    }

    // VERIFIES: REQ-HAM-01
    #[test]
    fn timing_follows_the_dit_unit() {
        assert_eq!(dit_ms(20), 60);
        assert_eq!(dit_ms(18), 66);
        assert_eq!(dit_ms(0), 1200 / MIN_WPM as u16, "speeds are clamped");
        assert_eq!(dit_ms(200), 1200 / MAX_WPM as u16);
        // "1 0 : 5" -> 1005: 1 = .----, 0 = -----, 0 = -----, 5 = .....
        let t = timeline(&hhmm(10, 5), 100);
        let tone: u32 = t.iter().filter(|e| e.0).map(|e| e.1 as u32).sum();
        let gaps: u32 = t.iter().filter(|e| !e.0).map(|e| e.1 as u32).sum();
        // dits: 1 (+4 dashes) + 5 dashes + 5 dashes + 5 dits = 1+12 + 15 + 15 + 5 = 48 units of tone
        assert_eq!(tone, 4800);
        // inside digits 4 gaps of 1 unit each = 16; between digits 3 x 3 units = 9
        assert_eq!(gaps, 2500);
        assert_eq!(t.first(), Some(&(true, 100)), "starts with the dit of the 1");
        assert_eq!(t.last(), Some(&(true, 100)), "ends with the last dit of the 5, no trailing gap");
        assert_eq!(t.len(), 20 * 2 - 1 - 3 + 3, "20 elements, a gap between each, and the long digit gaps replace three of them");
        // tones and gaps alternate
        assert!(t.windows(2).all(|w| w[0].0 != w[1].0));
    }

    // VERIFIES: REQ-HAM-01
    #[test]
    fn the_longest_time_fits_the_buffer() {
        let t = timeline(&[0, 0, 0, 0], 66);
        assert_eq!(t.len(), 20 + 19);
        assert_eq!(hhmm(23, 59), [2, 3, 5, 9]);
        assert_eq!(hhmm(0, 0), [0, 0, 0, 0]);
    }
}
