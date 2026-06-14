// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal Unicode clipboard get/set via the Win32 clipboard API.

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT: u32 = 13;

/// Put `text` on the clipboard as `CF_UNICODETEXT`.
pub fn set_text(hwnd: HWND, text: &str) {
    if text.is_empty() {
        return;
    }
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    unsafe {
        if OpenClipboard(hwnd).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(hmem) = GlobalAlloc(GMEM_MOVEABLE, utf16.len() * 2) {
            let dst = GlobalLock(hmem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(utf16.as_ptr(), dst, utf16.len());
                let _ = GlobalUnlock(hmem);
                // Ownership of hmem transfers to the clipboard on success.
                let _ = SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Read `CF_UNICODETEXT` from the clipboard, if present.
pub fn get_text(hwnd: HWND) -> Option<String> {
    unsafe {
        if OpenClipboard(hwnd).is_err() {
            return None;
        }
        let result = match GetClipboardData(CF_UNICODETEXT) {
            Ok(handle) if !handle.is_invalid() => {
                let hg = HGLOBAL(handle.0);
                let p = GlobalLock(hg) as *const u16;
                if p.is_null() {
                    None
                } else {
                    let mut len = 0usize;
                    while *p.add(len) != 0 {
                        len += 1;
                    }
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                    let _ = GlobalUnlock(hg);
                    Some(s)
                }
            }
            _ => None,
        };
        let _ = CloseClipboard();
        result
    }
}
