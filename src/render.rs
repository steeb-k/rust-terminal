// SPDX-License-Identifier: GPL-3.0-or-later
//! GDI renderer: self-drawn dark chrome (tab strip) plus the active terminal
//! grid below it. Double-buffered; cells drawn in runs of identical attributes.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateRoundRectRgn,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, EnumFontFamiliesExW, ExtTextOutW, FillRect,
    FrameRgn, GetDC, GetTextMetricsW, PatBlt, ReleaseDC, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DSTINVERT, ETO_CLIPPED,
    ETO_OPAQUE, FW_BOLD, FW_NORMAL, HBITMAP, HDC, HFONT, LOGFONTW, OPAQUE, OUT_TT_PRECIS,
    PAINTSTRUCT, SRCCOPY, TEXTMETRICW, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::chrome::{
    caption_xs, close_box, newtab_x, tab_x, CaptionBtn, BTN_W, CHROME_H, PAD_X, PAD_Y, RADIUS, TAB_W,
};
use crate::colors::{rgb, DEFAULT_BG, SEL_BG, SEL_FG};
use crate::grid::{Cell, CursorStyle, FLAG_BOLD, FLAG_REVERSE, FLAG_UNDERLINE};
use crate::parser::Term;

const CHROME_BG: u32 = rgb(28, 28, 30);
const TAB_TEXT: u32 = rgb(225, 225, 225);
const TAB_TEXT_DIM: u32 = rgb(150, 150, 150);
const HOVER_BG: u32 = rgb(60, 60, 62);
const CLOSE_HOVER_BG: u32 = rgb(232, 17, 35);
/// 1px window outline color when the window is not focused.
const BORDER_INACTIVE: u32 = rgb(90, 90, 90);
/// Extra vertical space added per text row for comfortable line spacing.
const LINE_GAP: i32 = 4;

pub struct Fonts {
    pub normal: HFONT,
    pub bold: HFONT,
    pub ui: HFONT,
    pub glyph: HFONT,
    pub cw: i32,
    pub ch: i32,
    /// Pixels of leading above the glyph within each (gapped) cell.
    pub lead: i32,
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
            let base_h = (tm.tmHeight + tm.tmExternalLeading).max(1);
            Fonts {
                normal,
                bold,
                ui,
                glyph,
                cw: tm.tmAveCharWidth.max(1),
                ch: base_h + LINE_GAP,
                lead: LINE_GAP / 2,
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

pub struct Chrome<'a> {
    pub labels: &'a [String],
    pub active: usize,
    pub accent: u32,
    pub is_max: bool,
    pub hover: Option<CaptionBtn>,
    /// Index of the tab whose close button is hovered, if any.
    pub hover_close: Option<usize>,
    /// Whether the window has focus (accent border vs. gray border).
    pub focused: bool,
}

pub fn paint(hwnd: HWND, term: &Term, fonts: &Fonts, chrome: &Chrome) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        render_to(hdc, rc.right - rc.left, rc.bottom - rc.top, term, fonts, chrome);
        let _ = EndPaint(hwnd, &ps);
    }
}

pub fn render_to(target: HDC, width: i32, height: i32, term: &Term, fonts: &Fonts, chrome: &Chrome) {
    unsafe {
        let rc = RECT { left: 0, top: 0, right: width, bottom: height };
        let mem = CreateCompatibleDC(target);
        let bmp = CreateCompatibleBitmap(target, width, height);
        let old_bmp = SelectObject(mem, bmp);

        let bg = CreateSolidBrush(COLORREF(DEFAULT_BG));
        FillRect(mem, &rc, bg);
        let _ = DeleteObject(bg);

        draw_chrome(mem, width, fonts, chrome);

        SetBkMode(mem, OPAQUE);
        draw_cells(mem, term, fonts, PAD_X, CHROME_H + PAD_Y);
        draw_cursor(mem, term, fonts, PAD_X, CHROME_H + PAD_Y, chrome.accent);

        // 1px window outline, drawn last so it sits on top of chrome and content.
        let border = if chrome.focused { chrome.accent } else { BORDER_INACTIVE };
        let radius = if chrome.is_max { 0 } else { RADIUS };
        draw_border(mem, width, height, radius, border);

        let _ = BitBlt(target, 0, 0, width, height, mem, 0, 0, SRCCOPY);

        SelectObject(mem, old_bmp);
        let _ = DeleteObject(HBITMAP(bmp.0));
        let _ = DeleteDC(mem);
    }
}

/// Stroke a 1px outline just inside the window region, following the rounded
/// corners (radius 0 = square, when maximized). The region matches the one set
/// on the window via `SetWindowRgn`, so the border hugs the visible edge.
unsafe fn draw_border(mem: HDC, w: i32, h: i32, radius: i32, color: u32) {
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius);
    let br = CreateSolidBrush(COLORREF(color));
    let _ = FrameRgn(mem, rgn, br, 1, 1);
    let _ = DeleteObject(br);
    let _ = DeleteObject(rgn);
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

unsafe fn draw_chrome(mem: HDC, width: i32, fonts: &Fonts, c: &Chrome) {
    fill(mem, 0, 0, width, CHROME_H, CHROME_BG);
    SetBkMode(mem, TRANSPARENT);
    let ty = (CHROME_H - 18) / 2; // vertically center the ~18px tab text

    // Tab backgrounds + separators; accent bar on the active tab's top edge.
    for i in 0..c.labels.len() {
        let x0 = tab_x(i);
        let x1 = x0 + TAB_W;
        if i == c.active {
            fill(mem, x0, 0, x1, CHROME_H, DEFAULT_BG);
            fill(mem, x0, 0, x1, 2, c.accent);
        } else if i > 0 {
            fill(mem, x0, 8, x0 + 1, CHROME_H - 8, rgb(60, 60, 60));
        }
    }

    // Tab labels.
    SelectObject(mem, fonts.ui);
    for (i, label) in c.labels.iter().enumerate() {
        let x0 = tab_x(i);
        let x1 = x0 + TAB_W;
        let color = if i == c.active { TAB_TEXT } else { TAB_TEXT_DIM };
        let label_clip = RECT { left: x0 + 12, top: 0, right: x1 - 36, bottom: CHROME_H };
        text(mem, x0 + 12, ty, label_clip, label, color);
    }
    // New-tab "+".
    let nx = newtab_x(c.labels.len());
    let plus_clip = RECT { left: nx, top: 0, right: nx + 26, bottom: CHROME_H };
    text(mem, nx + 8, ty - 1, plus_clip, "+", TAB_TEXT);

    // Tab close buttons + caption buttons (Segoe MDL2 Assets glyphs).
    SelectObject(mem, fonts.glyph);
    for i in 0..c.labels.len() {
        let (bx0, by0, bx1, by1) = close_box(i);
        let hovered = c.hover_close == Some(i);
        let fg = if hovered {
            fill(mem, bx0, by0, bx1, by1, HOVER_BG);
            rgb(255, 255, 255)
        } else if i == c.active {
            TAB_TEXT
        } else {
            TAB_TEXT_DIM
        };
        let clip = RECT { left: bx0, top: 0, right: bx1, bottom: CHROME_H };
        // E711 is a smaller close glyph that suits the tab close box.
        text(mem, bx0 + 6, (CHROME_H - 12) / 2, clip, "\u{E711}", fg);
    }

    // Caption buttons: minimize, maximize/restore, close — hover-highlighted
    // (red for close, lighter gray for min/max).
    let gy = (CHROME_H - 14) / 2;
    let (min_x, max_x, close_x) = caption_xs(width);
    let max_glyph = if c.is_max { "\u{E923}" } else { "\u{E922}" };
    let btns = [
        (min_x, "\u{E921}", CaptionBtn::Min),
        (max_x, max_glyph, CaptionBtn::Max),
        (close_x, "\u{E8BB}", CaptionBtn::Close),
    ];
    for (bx, glyph, which) in btns {
        let hovered = c.hover == Some(which);
        let fg = if hovered {
            let bg = if which == CaptionBtn::Close { CLOSE_HOVER_BG } else { HOVER_BG };
            fill(mem, bx, 0, bx + BTN_W, CHROME_H, bg);
            rgb(255, 255, 255)
        } else {
            TAB_TEXT_DIM
        };
        let clip = RECT { left: bx, top: 0, right: bx + BTN_W, bottom: CHROME_H };
        text(mem, bx + 16, gy, clip, glyph, fg);
    }
}

unsafe fn draw_cells(mem: HDC, term: &Term, fonts: &Fonts, x_off: i32, y_off: i32) {
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
            let px = x_off + start as i32 * fonts.cw;
            let py = y_off + y as i32 * fonts.ch;
            // Opaque fill covers the full (gapped) cell; the glyph sits below the leading.
            let rect = RECT { left: px, top: py, right: px + run_len as i32 * fonts.cw, bottom: py + fonts.ch };
            let _ = ExtTextOutW(mem, px, py + fonts.lead, ETO_OPAQUE, Some(&rect), PCWSTR(t.as_ptr()), run_len as u32, Some(dx.as_ptr()));

            if fl0 & FLAG_UNDERLINE != 0 {
                let uy = py + fonts.ch - 1;
                fill(mem, px, uy, rect.right, uy + 1, f0);
            }
        }
    }
}

unsafe fn draw_cursor(mem: HDC, term: &Term, fonts: &Fonts, x_off: i32, y_off: i32, accent: u32) {
    let g = &term.grid;
    if !g.cursor_visible || g.view_offset != 0 {
        return;
    }
    let px = x_off + g.cx as i32 * fonts.cw;
    let py = y_off + g.cy as i32 * fonts.ch;
    match g.cursor_style {
        CursorStyle::Block => {
            let _ = PatBlt(mem, px, py, fonts.cw, fonts.ch, DSTINVERT);
        }
        CursorStyle::Underline => fill(mem, px, py + fonts.ch - 2, px + fonts.cw, py + fonts.ch, accent),
        CursorStyle::Bar => fill(mem, px, py, px + 2, py + fonts.ch, accent),
    }
}
