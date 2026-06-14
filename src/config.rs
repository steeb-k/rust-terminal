// SPDX-License-Identifier: GPL-3.0-or-later
//! Registry-backed configuration, mirroring the sibling `start-pe` pattern:
//! read `HKLM` first (the PE build writes machine-wide because the shell runs as
//! SYSTEM), then overlay `HKCU` so a per-user install still wins.
//!
//! The accent color is shared with StartPE: it lives under `Software\StartPE`
//! as `StartButtonColor` (REG_DWORD, COLORREF 0x00BBGGRR). The terminal's own
//! settings live under `Software\RustTerminal`.

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

/// StartPE's default accent (purple, RGB 230,90,180) when the key is absent.
pub const ACCENT_DEFAULT: u32 = 0x00E6_5AB4;

const STARTPE_KEY: &str = r"Software\StartPE";
const TERM_KEY: &str = r"Software\RustTerminal";

pub struct Config {
    /// Shared accent (COLORREF 0x00BBGGRR) from StartPE.
    pub accent: u32,
    /// Shell command line to launch.
    pub shell: String,
    /// Preferred monospace font face (falls back if unavailable).
    pub font_face: String,
    /// Terminal font cell height in pixels.
    pub font_px: i32,
    /// Scrollback capacity in lines.
    pub scrollback: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            accent: ACCENT_DEFAULT,
            shell: "powershell.exe".into(),
            font_face: "Consolas".into(),
            font_px: 16,
            scrollback: 5000,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config::default();
        for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let root = RegKey::predef(hive);
            // Shared accent from StartPE.
            if let Ok(k) = root.open_subkey(STARTPE_KEY) {
                if let Ok(v) = k.get_value::<u32, _>("StartButtonColor") {
                    c.accent = v;
                }
            }
            // Terminal-specific settings.
            if let Ok(k) = root.open_subkey(TERM_KEY) {
                if let Ok(v) = k.get_value::<String, _>("Shell") {
                    if !v.trim().is_empty() {
                        c.shell = v;
                    }
                }
                if let Ok(v) = k.get_value::<String, _>("FontFace") {
                    if !v.trim().is_empty() {
                        c.font_face = v;
                    }
                }
                if let Ok(v) = k.get_value::<u32, _>("FontSize") {
                    c.font_px = (v as i32).clamp(8, 48);
                }
                if let Ok(v) = k.get_value::<u32, _>("Scrollback") {
                    c.scrollback = (v as usize).clamp(0, 100_000);
                }
            }
        }
        c
    }
}
