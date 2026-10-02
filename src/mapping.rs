use crate::settings::Mode;

/// Converts a physical keyboard identifier (scan code in bits 0..7, extended
/// flag in bit 8) into a MIDI note. `seed` makes random mode stable for a run.
pub fn note_for_key(key_id: u16, mode: Mode, transpose: i32, seed: u64) -> Option<u8> {
    if key_id > 0x1ff {
        return None;
    }
    let transpose = transpose.clamp(-24, 24);
    let offset = match mode {
        Mode::WickiHayden => wicki_offset(key_id).unwrap_or_else(|| fallback_offset(key_id)),
        Mode::Chromatic => {
            chromatic_offset(key_id).unwrap_or_else(|| fallback_chromatic_offset(key_id))
        }
        Mode::Random => random_offset(key_id, seed)?,
    };
    let note = 72i32 + offset + transpose;
    (0..=127).contains(&note).then_some(note as u8)
}

fn wicki_offset(key_id: u16) -> Option<i32> {
    let (row, column) = keyboard_position(key_id)?;
    let row_base = match row {
        0 => -19, // Z row
        1 => -14, // A row
        2 => -9,  // Q row
        3 => -4,  // number row
        _ => return None,
    };
    // Alternating whole-step/third relationships form the Wicki-Hayden grid.
    Some(row_base + column as i32 * 2)
}

fn chromatic_offset(key_id: u16) -> Option<i32> {
    let (row, column) = keyboard_position(key_id)?;
    let row_anchor = [-12, -7, -1, 4].get(row).copied()?;
    Some(row_anchor + column as i32)
}

fn fallback_offset(key_id: u16) -> i32 {
    // Stable bounded notes for modifiers, function keys, navigation keys and
    // other physical keys outside the four instrument rows.
    let x = key_id as u32;
    ((x.wrapping_mul(0x045d_9f3b) ^ (x >> 7)) % 25) as i32 - 12
}

fn fallback_chromatic_offset(key_id: u16) -> i32 {
    fallback_offset(key_id)
}

fn keyboard_position(key_id: u16) -> Option<(usize, usize)> {
    if key_id & 0x100 != 0 {
        return None;
    }
    let scan = (key_id & 0xff) as u8;
    const Z_ROW: &[u8] = &[0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35];
    const A_ROW: &[u8] = &[
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28,
    ];
    const Q_ROW: &[u8] = &[
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
    ];
    const NUM_ROW: &[u8] = &[
        0x29, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
    ];
    for (row, keys) in [Z_ROW, A_ROW, Q_ROW, NUM_ROW].iter().enumerate() {
        if let Some(column) = keys.iter().position(|&code| code == scan) {
            return Some((row, column));
        }
    }
    None
}

fn random_offset(key_id: u16, seed: u64) -> Option<i32> {
    if key_id > 0x1ff {
        return None;
    }
    let mut x = seed ^ key_id as u64 ^ 0x9e37_79b9_7f4a_7c15;
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    Some((x % 25) as i32 - 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_four_physical_rows_and_bounds_unknown_keys() {
        assert_eq!(note_for_key(0x2c, Mode::WickiHayden, 0, 1), Some(53));
        assert_eq!(note_for_key(0x1e, Mode::WickiHayden, 0, 1), Some(58));
        assert_eq!(note_for_key(0x10, Mode::WickiHayden, 0, 1), Some(63));
        assert_eq!(note_for_key(0x29, Mode::WickiHayden, 0, 1), Some(68));
        assert_eq!(note_for_key(0x02, Mode::WickiHayden, 0, 1), Some(70));
        assert!(note_for_key(0x45, Mode::Chromatic, 0, 1).is_some());
        assert!(note_for_key(0x11c, Mode::Random, 0, 1).is_some());
        assert_eq!(note_for_key(0x200, Mode::Random, 0, 1), None);
    }

    #[test]
    fn random_mapping_is_repeatable_for_each_run_seed() {
        assert_eq!(
            note_for_key(0x2c, Mode::Random, 0, 123),
            note_for_key(0x2c, Mode::Random, 0, 123)
        );
        assert_ne!(
            note_for_key(0x2c, Mode::Random, 0, 1),
            note_for_key(0x2c, Mode::Random, 0, 9)
        );
    }

    #[test]
    fn mode_anchors_and_ranges_match_layout_contract() {
        assert_eq!(note_for_key(0x2c, Mode::Chromatic, 0, 1), Some(60));
        assert_eq!(note_for_key(0x1e, Mode::Chromatic, 0, 1), Some(65));
        assert_eq!(note_for_key(0x10, Mode::Chromatic, 0, 1), Some(71));
        assert_eq!(note_for_key(0x29, Mode::Chromatic, 0, 1), Some(76));
        assert_eq!(note_for_key(0x2d, Mode::WickiHayden, 0, 1), Some(55));
        assert_eq!(note_for_key(0x2c, Mode::WickiHayden, 24, 1), Some(77));
        assert_eq!(note_for_key(0x2c, Mode::WickiHayden, -24, 1), Some(29));
        for key in 0..=0x1ff {
            let note = note_for_key(key, Mode::Random, 0, 32).unwrap();
            assert!((60..=84).contains(&note));
        }
    }
}
