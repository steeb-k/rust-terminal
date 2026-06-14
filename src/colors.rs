// SPDX-License-Identifier: GPL-3.0-or-later
//! Color palette. Colors are stored as GDI `COLORREF` values (0x00BBGGRR).

/// Build a `COLORREF` (0x00BBGGRR) from RGB components.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (b as u32) << 16 | (g as u32) << 8 | r as u32
}

/// The 16 ANSI colors (Campbell scheme — the Windows Terminal default).
pub const ANSI16: [u32; 16] = [
    rgb(12, 12, 12),    // 0  black
    rgb(197, 15, 31),   // 1  red
    rgb(19, 161, 14),   // 2  green
    rgb(193, 156, 0),   // 3  yellow
    rgb(0, 55, 218),    // 4  blue
    rgb(136, 23, 152),  // 5  magenta
    rgb(58, 150, 221),  // 6  cyan
    rgb(204, 204, 204), // 7  white
    rgb(118, 118, 118), // 8  bright black
    rgb(231, 72, 86),   // 9  bright red
    rgb(22, 198, 12),   // 10 bright green
    rgb(249, 241, 165), // 11 bright yellow
    rgb(59, 120, 255),  // 12 bright blue
    rgb(180, 0, 158),   // 13 bright magenta
    rgb(97, 214, 214),  // 14 bright cyan
    rgb(242, 242, 242), // 15 bright white
];

/// Default foreground / background for a fresh cell.
pub const DEFAULT_FG: u32 = ANSI16[7];
pub const DEFAULT_BG: u32 = rgb(18, 18, 18);

/// Selection highlight colors.
pub const SEL_BG: u32 = rgb(38, 79, 120);
pub const SEL_FG: u32 = rgb(255, 255, 255);

/// Resolve a 256-color palette index to a `COLORREF`.
pub fn xterm256(idx: u8) -> u32 {
    match idx {
        0..=15 => ANSI16[idx as usize],
        16..=231 => {
            // 6x6x6 color cube.
            let i = idx - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let lvl = |v: u8| if v == 0 { 0u8 } else { 55 + v * 40 };
            rgb(lvl(r), lvl(g), lvl(b))
        }
        232..=255 => {
            // grayscale ramp
            let v = 8 + (idx - 232) * 10;
            rgb(v, v, v)
        }
    }
}
