// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard input -> PTY byte encoding.
//!
//! `WM_CHAR` delivers typed text plus Enter/Tab/Backspace and Ctrl+letter
//! control codes, so the shell does its own Tab completion. `WM_KEYDOWN`
//! supplies navigation and function keys as VT escapes, honoring DECCKM
//! (application cursor keys) and Shift/Alt/Ctrl modifiers (xterm style).

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_F1, VK_F10, VK_F11,
    VK_F12, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT, VK_LEFT,
    VK_MENU, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_UP,
};

/// Encode a `WM_CHAR` UTF-16 code unit as UTF-8 bytes to send to the shell.
pub fn char_bytes(code_unit: u16) -> Vec<u8> {
    match char::from_u32(code_unit as u32) {
        Some(c) => {
            let mut b = [0u8; 4];
            c.encode_utf8(&mut b).as_bytes().to_vec()
        }
        None => Vec::new(),
    }
}

/// xterm modifier code: 1 + (shift|alt<<1|ctrl<<2). 0 means no modifiers.
fn modifier_mask() -> u8 {
    unsafe {
        let down = |vk: VIRTUAL_KEY| (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0;
        let mut m = 0u8;
        if down(VK_SHIFT) {
            m |= 1;
        }
        if down(VK_MENU) {
            m |= 2;
        }
        if down(VK_CONTROL) {
            m |= 4;
        }
        m
    }
}

/// Encode a navigation/function `WM_KEYDOWN` as a VT escape sequence, or `None`
/// for keys that `WM_CHAR` handles instead.
pub fn key_bytes(vk: VIRTUAL_KEY, app_cursor: bool) -> Option<Vec<u8>> {
    let m = modifier_mask();

    // Cursor/Home/End: letter-final form (SS3 in app-cursor mode when unmodified).
    let letter = match vk {
        VK_UP => Some(b'A'),
        VK_DOWN => Some(b'B'),
        VK_RIGHT => Some(b'C'),
        VK_LEFT => Some(b'D'),
        VK_HOME => Some(b'H'),
        VK_END => Some(b'F'),
        _ => None,
    };
    if let Some(f) = letter {
        return Some(if m == 0 {
            let intro: &[u8] = if app_cursor { b"\x1bO" } else { b"\x1b[" };
            [intro, &[f]].concat()
        } else {
            format!("\x1b[1;{}{}", m + 1, f as char).into_bytes()
        });
    }

    // Tilde-terminated keys.
    let tilde = match vk {
        VK_INSERT => Some(2),
        VK_DELETE => Some(3),
        VK_PRIOR => Some(5), // Page Up
        VK_NEXT => Some(6),  // Page Down
        _ => None,
    };
    if let Some(num) = tilde {
        return Some(if m == 0 {
            format!("\x1b[{num}~").into_bytes()
        } else {
            format!("\x1b[{num};{}~", m + 1).into_bytes()
        });
    }

    // Function keys (modifiers omitted for brevity).
    let fk: &[u8] = match vk {
        VK_F1 => b"\x1bOP",
        VK_F2 => b"\x1bOQ",
        VK_F3 => b"\x1bOR",
        VK_F4 => b"\x1bOS",
        VK_F5 => b"\x1b[15~",
        VK_F6 => b"\x1b[17~",
        VK_F7 => b"\x1b[18~",
        VK_F8 => b"\x1b[19~",
        VK_F9 => b"\x1b[20~",
        VK_F10 => b"\x1b[21~",
        VK_F11 => b"\x1b[23~",
        VK_F12 => b"\x1b[24~",
        _ => return None,
    };
    Some(fk.to_vec())
}
