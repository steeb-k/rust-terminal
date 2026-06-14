// SPDX-License-Identifier: GPL-3.0-or-later
//! GDI renderer: self-drawn dark chrome (tab strip) plus the active terminal
//! grid below it. Double-buffered; cells drawn in runs of identical attributes.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, EnumFontFamiliesExW, ExtTextOutW, FillRect, GetDC,
    GetTextMetricsW, PatBlt, ReleaseDC, SelectObject, SetBkColor, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DSTINVERT, ETO_CLIPPED, ETO_OPAQUE,
    FW_BOLD, FW_NORMAL, HBITMAP, HDC, HFONT, LOGFONTW, OPAQUE, OUT_TT_PRECIS, PAINTSTRUCT, SRCCOPY,
    TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::chrome::{caption_xs, close_box, newtab_x, tab_x, BTN_W, CHROME_H, TAB_W};
use crate::colors::{rgb, DEFAULT_BG, SEL_BG, SEL_FG};
use crate::grid::{Cell, CursorStyle, FLAG_BOLD, FLAG_REVERSE, FLAG_UNDERLINE};
use crate::parser::Term;

const CHROME_BG: u32 = rgb(28, 28, 30);
const TAB_TEXT: u32 = rgb(225, 225, 225);
const TAB_TEXT_DIM: u32 = rgb(150, 150, 150);

pub struct Fonts {
    pub normal: HFONT,
    pub bold: HFONT,
    pub ui: HFONT,
    pub glyph: HFONT,
    pub cw: i32,
    pub ch: i32,
}

impl Fonts {
    pub fn new(px: i32, face: &str) -> Fonts {
        unsafe {
            // Pick the first available monospace face from the preference chain.
            let chosen = pick_mono(face);
            let normal = make_font(px, FW_NORMAL.0 as i32, PCWSTR(chosen.as_ptr()), 1 /*FIXED_PITCH*/);
            let bold = make_font(px, FW_BOLD.0 as i32, PCWSTR(chosen.as_ptr()), 1);
            let ui = make_font(18, FW_NORMAL.0 as i32, w!("Segoe UI"), 2 /*VARIABLE_PITCH*/);
            let glyph = make_font(14, FW_NORMAL.0 as i32, w!("Segoe MDL2 Assets"), 0);
            let hdc = GetDC(None);
            let old = SelectObject(hdc, normal);
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(hdc, &mut tm);
            SelectObject(hdc, old);
            ReleaseDC(None, hdc);
            Fonts {
                normal,
                bold,
                ui,
                glyph,
                cw: tm.tmAveCharWidth.max(1),
                ch: (tm.tmHeight + tm.tmExternalLeading).max(1),
            }
        }
    }

    /// Grid (cols, rows) fitting a terminal area of the given pixel size.
    pub fn grid_size(&self, width: i32, height: i32) -> (u16, u16) {
        let cols = (width / self.cw).max(1) as u16;
        let rows = (height / self.ch).max(1) as u16;
        (cols, rows)
    }
}

/// EnumFontFamiliesExW callback: flag that a matching face exists, then stop.
unsafe extern "system" fn font_seen(
    _lf: *const LOGFONTW,
    _tm: *const TEXTMETRICW,
    _ty: u32,
    lparam: LPARAM,
) -> i32 {
    *(lparam.0 as *mut bool) = true;
    0
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn face_exists(name: &str) -> bool {
    let hdc = GetDC(None);
    let mut lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
    for (i, c) in name.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    let mut found = false;
    EnumFontFamiliesExW(
        hdc,
        &lf,
        Some(font_seen),
        LPARAM(&mut found as *mut bool as isize),
        0,
    );
    ReleaseDC(None, hdc);
    found
}

/// First available monospace face: the preferred one, then a PE-safe chain.
unsafe fn pick_mono(preferred: &str) -> Vec<u16> {
    for face in [preferred, "Consolas", "Cascadia Mono", "Lucida Console", "Courier New"] {
        if face_exists(face) {
            return wide(face);
        }
    }
    wide("Courier New")
}

unsafe fn make_font(px: i32, weight: i32, face: PCWSTR, pitch: u32) -> HFONT {
    CreateFontW(
        px,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_TT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        pitch,
        face,
    )
}

pub fn paint(hwnd: HWND, term: &Term, fonts: &Fonts, labels: &[String], active: usize, accent: u32, is_max: bool) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        render_to(hdc, rc.right - rc.left, rc.bottom - rc.top, term, fonts, labels, active, accent, is_max);
        let _ = EndPaint(hwnd, &ps);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_to(
    target: HDC,
    width: i32,
    height: i32,
    term: &Term,
    fonts: &Fonts,
    labels: &[String],
    active: usize,
    accent: u32,
    is_max: bool,
) {
    unsafe {
        let rc = RECT { left: 0, top: 0, right: width, bottom: height };
        let mem = CreateCompatibleDC(target);
        let bmp = CreateCompatibleBitmap(target, width, height);
        let old_bmp = SelectObject(mem, bmp);

        let bg = CreateSolidBrush(COLORREF(DEFAULT_BG));
        FillRect(mem, &rc, bg);
        let _ = DeleteObject(bg);

        draw_chrome(mem, width, fonts, labels, active, accent, is_max);

        SetBkMode(mem, OPAQUE);
        draw_cells(mem, term, fonts, CHROME_H);
        draw_cursor(mem, term, fonts, CHROME_H, accent);

        let _ = BitBlt(target, 0, 0, width, height, mem, 0, 0, SRCCOPY);

        SelectObject(mem, old_bmp);
        let _ = DeleteObject(HBITMAP(bmp.0));
        let _ = DeleteDC(mem);
    }
}

unsafe fn fill(mem: HDC, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
    let r = RECT { left: x0, top: y0, right: x1, bottom: y1 };
    let b = CreateSolidBrush(COLORREF(color));
    FillRect(mem, &r, b);
    let _ = DeleteObject(b);
}

unsafe fn text(mem: HDC, x: i32, y: i32, clip: RECT, s: &str, color: u32) {
    SetTextColor(mem, COLORREF(color));
    let u: Vec<u16> = s.encode_utf16().collect();
    let _ = ExtTextOutW(mem, x, y, ETO_CLIPPED, Some(&clip), PCWSTR(u.as_ptr()), u.len() as u32, None);
}

unsafe fn draw_chrome(mem: HDC, width: i32, fonts: &Fonts, labels: &[String], active: usize, accent: u32, is_max: bool) {
    fill(mem, 0, 0, width, CHROME_H, CHROME_BG);
    SelectObject(mem, fonts.ui);
    SetBkMode(mem, TRANSPARENT);

    for (i, label) in labels.iter().enumerate() {
        let x0 = tab_x(i);
        let x1 = x0 + TAB_W;
        let actv = i == active;
        if actv {
            fill(mem, x0, 0, x1, CHROME_H, DEFAULT_BG);
            fill(mem, x0, 0, x1, 2, accent); // accent top bar
        } else if i > 0 {
            fill(mem, x0, 6, x0 + 1, CHROME_H - 6, rgb(60, 60, 60)); // separator
        }
        let label_clip = RECT { left: x0 + 10, top: 0, right: x1 - 26, bottom: CHROME_H };
        let color = if actv { TAB_TEXT } else { TAB_TEXT_DIM };
        text(mem, x0 + 10, 6, label_clip, label, color);

        let (cx0, _, cx1, _) = close_box(i);
        let close_clip = RECT { left: cx0, top: 0, right: cx1 + 2, bottom: CHROME_H };
        text(mem, cx0, 6, close_clip, "\u{00D7}", TAB_TEXT_DIM);
    }

    let nx = newtab_x(labels.len());
    let plus_clip = RECT { left: nx, top: 0, right: nx + 24, bottom: CHROME_H };
    text(mem, nx + 7, 5, plus_clip, "+", TAB_TEXT);

    // Caption buttons (Segoe MDL2 Assets glyphs): minimize, maximize/restore, close.
    SelectObject(mem, fonts.glyph);
    let (min_x, max_x, close_x) = caption_xs(width);
    let max_glyph = if is_max { "\u{E923}" } else { "\u{E922}" }; // restore / maximize
    let btns = [(min_x, "\u{E921}"), (max_x, max_glyph), (close_x, "\u{E8BB}")];
    for (bx, glyph) in btns {
        let clip = RECT { left: bx, top: 0, right: bx + BTN_W, bottom: CHROME_H };
        text(mem, bx + 16, 8, clip, glyph, TAB_TEXT_DIM);
    }
}

unsafe fn draw_cells(mem: HDC, term: &Term, fonts: &Fonts, y_off: i32) {
    let g = &term.grid;
    let dx = vec![fonts.cw; g.cols];
    let mut cur_font: HFONT = HFONT::default();
    let mut selected_bold = false;
    let blank = Cell::default();
    let sel = g.sel_norm();

    for y in 0..g.rows {
        let line = g.display_line(y);
        let vline = g.display_vline(y);
        let eff = |x: usize| -> (char, u32, u32, u8) {
            let c = line.get(x).copied().unwrap_or(blank);
            let (mut fg, mut bg) = (c.fg, c.bg);
            if c.flags & FLAG_REVERSE != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            if let Some((al, ac, bl, bc)) = sel {
                let on = (vline > al || (vline == al && x >= ac))
                    && (vline < bl || (vline == bl && x <= bc));
                if on {
                    fg = SEL_FG;
                    bg = SEL_BG;
                }
            }
            (c.ch, fg, bg, c.flags)
        };

        let mut x = 0usize;
        while x < g.cols {
            let start = x;
            let (_, f0, b0, fl0) = eff(start);
            while x < g.cols {
                let (_, f, b, fl) = eff(x);
                if f != f0 || b != b0 || fl != fl0 {
                    break;
                }
                x += 1;
            }
            let run_len = x - start;

            let want_bold = fl0 & FLAG_BOLD != 0;
            if cur_font.is_invalid() || want_bold != selected_bold {
                cur_font = if want_bold { fonts.bold } else { fonts.normal };
                SelectObject(mem, cur_font);
                selected_bold = want_bold;
            }
            SetTextColor(mem, COLORREF(f0));
            SetBkColor(mem, COLORREF(b0));

            let t: Vec<u16> = (start..x).map(|cx| eff(cx).0 as u16).collect();
            let px = start as i32 * fonts.cw;
            let py = y_off + y as i32 * fonts.ch;
            let rect = RECT { left: px, top: py, right: px + run_len as i32 * fonts.cw, bottom: py + fonts.ch };
            let _ = ExtTextOutW(mem, px, py, ETO_OPAQUE, Some(&rect), PCWSTR(t.as_ptr()), run_len as u32, Some(dx.as_ptr()));

            if fl0 & FLAG_UNDERLINE != 0 {
                let uy = py + fonts.ch - 1;
                fill(mem, px, uy, rect.right, uy + 1, f0);
            }
        }
    }
}

unsafe fn draw_cursor(mem: HDC, term: &Term, fonts: &Fonts, y_off: i32, accent: u32) {
    let g = &term.grid;
    if !g.cursor_visible || g.view_offset != 0 {
        return;
    }
    let px = g.cx as i32 * fonts.cw;
    let py = y_off + g.cy as i32 * fonts.ch;
    match g.cursor_style {
        CursorStyle::Block => {
            let _ = PatBlt(mem, px, py, fonts.cw, fonts.ch, DSTINVERT);
        }
        CursorStyle::Underline => fill(mem, px, py + fonts.ch - 2, px + fonts.cw, py + fonts.ch, accent),
        CursorStyle::Bar => fill(mem, px, py, px + 2, py + fonts.ch, accent),
    }
}
