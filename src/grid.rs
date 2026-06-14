// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal screen buffer.
//!
//! M2 scope: primary + alternate screens, a capped scrollback ring (primary
//! only), a scroll region (DECSTBM), line/character insert/delete, cursor
//! save/restore and cursor styles, and a scrollback view offset for the wheel.

use std::collections::VecDeque;

use crate::colors::{DEFAULT_BG, DEFAULT_FG};

pub const FLAG_BOLD: u8 = 1 << 0;
pub const FLAG_ITALIC: u8 = 1 << 1;
pub const FLAG_UNDERLINE: u8 = 1 << 2;
pub const FLAG_REVERSE: u8 = 1 << 3;

#[derive(Clone, Copy, PartialEq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    pub flags: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell { ch: ' ', fg: DEFAULT_FG, bg: DEFAULT_BG, flags: 0 }
    }
}

/// A text selection, endpoints in virtual-line space (scrollback + active).
#[derive(Clone, Copy)]
pub struct Sel {
    pub a: (usize, usize), // (vline, col) anchor
    pub b: (usize, usize), // (vline, col) head
}

#[derive(Clone, Copy)]
struct Saved {
    cx: usize,
    cy: usize,
    fg: u32,
    bg: u32,
    flags: u8,
}

pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    primary: Vec<Cell>,
    alt: Vec<Cell>,
    pub on_alt: bool,
    pub scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,

    pub cx: usize,
    pub cy: usize,
    pub fg: u32,
    pub bg: u32,
    pub flags: u8,
    pub wrap_pending: bool,

    pub cursor_visible: bool,
    pub cursor_style: CursorStyle,

    /// Scroll region, 0-based inclusive [top, bot].
    pub top: usize,
    pub bot: usize,

    /// Terminal modes tracked for input/paste/mouse (acted on in input/M3).
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_report: bool,
    pub mouse_sgr: bool,

    /// Scrollback view: 0 = bottom (live), N = scrolled up N lines.
    pub view_offset: usize,

    saved: Option<Saved>,
    /// Primary cursor stashed while on the alternate screen.
    primary_cursor: (usize, usize),

    pub selection: Option<Sel>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Grid {
            cols,
            rows,
            primary: vec![Cell::default(); cols * rows],
            alt: vec![Cell::default(); cols * rows],
            on_alt: false,
            scrollback: VecDeque::new(),
            max_scrollback,
            cx: 0,
            cy: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            flags: 0,
            wrap_pending: false,
            cursor_visible: true,
            cursor_style: CursorStyle::Block,
            top: 0,
            bot: rows - 1,
            app_cursor: false,
            bracketed_paste: false,
            mouse_report: false,
            mouse_sgr: false,
            view_offset: 0,
            saved: None,
            primary_cursor: (0, 0),
            selection: None,
        }
    }

    #[inline]
    fn buf(&self) -> &Vec<Cell> {
        if self.on_alt { &self.alt } else { &self.primary }
    }

    #[inline]
    fn buf_mut(&mut self) -> &mut Vec<Cell> {
        if self.on_alt { &mut self.alt } else { &mut self.primary }
    }

    #[inline]
    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.cols + x
    }

    fn blank(&self) -> Cell {
        Cell { ch: ' ', fg: self.fg, bg: self.bg, flags: 0 }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let regrid = |old: &[Cell], oc: usize, or_: usize| -> Vec<Cell> {
            let mut next = vec![Cell::default(); cols * rows];
            for y in 0..rows.min(or_) {
                for x in 0..cols.min(oc) {
                    next[y * cols + x] = old[y * oc + x];
                }
            }
            next
        };
        self.primary = regrid(&self.primary, self.cols, self.rows);
        self.alt = regrid(&self.alt, self.cols, self.rows);
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bot = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.wrap_pending = false;
        self.clamp_view();
    }

    // ---- printing & basic movement -------------------------------------

    pub fn put(&mut self, ch: char) {
        if self.wrap_pending {
            self.cx = 0;
            self.line_feed();
            self.wrap_pending = false;
        }
        let cell = Cell { ch, fg: self.fg, bg: self.bg, flags: self.flags };
        let i = self.idx(self.cx, self.cy);
        self.buf_mut()[i] = cell;
        if self.cx + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.cx += 1;
        }
    }

    pub fn carriage_return(&mut self) {
        self.cx = 0;
        self.wrap_pending = false;
    }

    pub fn line_feed(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.bot {
            self.scroll_up_in(self.top, self.bot, 1, true);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    /// Reverse line feed (RI): up one, scrolling the region down at the top.
    pub fn reverse_line_feed(&mut self) {
        self.wrap_pending = false;
        if self.cy == self.top {
            self.scroll_down_in(self.top, self.bot, 1);
        } else if self.cy > 0 {
            self.cy -= 1;
        }
    }

    pub fn backspace(&mut self) {
        self.wrap_pending = false;
        if self.cx > 0 {
            self.cx -= 1;
        }
    }

    pub fn tab(&mut self) {
        let next = ((self.cx / 8) + 1) * 8;
        self.cx = next.min(self.cols - 1);
        self.wrap_pending = false;
    }

    pub fn set_cursor(&mut self, x: usize, y: usize) {
        self.cx = x.min(self.cols - 1);
        self.cy = y.min(self.rows - 1);
        self.wrap_pending = false;
    }

    pub fn move_cursor(&mut self, dx: isize, dy: isize) {
        self.cx = (self.cx as isize + dx).clamp(0, self.cols as isize - 1) as usize;
        self.cy = (self.cy as isize + dy).clamp(0, self.rows as isize - 1) as usize;
        self.wrap_pending = false;
    }

    // ---- scrolling -----------------------------------------------------

    /// Scroll rows [top, bot] up by `n`, blanking the bottom. If `capture` and
    /// the region is the full primary screen, evicted lines go to scrollback.
    pub fn scroll_up_in(&mut self, top: usize, bot: usize, n: usize, capture: bool) {
        let n = n.min(bot - top + 1);
        if n == 0 {
            return;
        }
        let cols = self.cols;
        let cap = capture && !self.on_alt && top == 0 && bot == self.rows - 1;
        if cap {
            for r in 0..n {
                let s = (top + r) * cols;
                let line = self.buf()[s..s + cols].to_vec();
                self.scrollback.push_back(line);
            }
            while self.scrollback.len() > self.max_scrollback {
                self.scrollback.pop_front();
            }
        }
        let blank = self.blank();
        let buf = self.buf_mut();
        let src = (top + n) * cols;
        let end = (bot + 1) * cols;
        buf.copy_within(src..end, top * cols);
        let fill = (bot + 1 - n) * cols..(bot + 1) * cols;
        buf[fill].fill(blank);
    }

    /// Scroll rows [top, bot] down by `n`, blanking the top.
    pub fn scroll_down_in(&mut self, top: usize, bot: usize, n: usize) {
        let n = n.min(bot - top + 1);
        if n == 0 {
            return;
        }
        let cols = self.cols;
        let blank = self.blank();
        let buf = self.buf_mut();
        let src = top * cols;
        let count = (bot + 1 - top - n) * cols;
        buf.copy_within(src..src + count, (top + n) * cols);
        buf[top * cols..(top + n) * cols].fill(blank);
    }

    pub fn insert_lines(&mut self, n: usize) {
        if self.cy >= self.top && self.cy <= self.bot {
            self.scroll_down_in(self.cy, self.bot, n);
        }
    }

    pub fn delete_lines(&mut self, n: usize) {
        if self.cy >= self.top && self.cy <= self.bot {
            self.scroll_up_in(self.cy, self.bot, n, false);
        }
    }

    // ---- in-line character editing -------------------------------------

    pub fn insert_chars(&mut self, n: usize) {
        let cols = self.cols;
        let row = self.idx(0, self.cy);
        let cx = self.cx;
        let n = n.min(cols - cx);
        let blank = self.blank();
        let buf = self.buf_mut();
        let line = &mut buf[row..row + cols];
        line.copy_within(cx..cols - n, cx + n);
        line[cx..cx + n].fill(blank);
    }

    pub fn delete_chars(&mut self, n: usize) {
        let cols = self.cols;
        let row = self.idx(0, self.cy);
        let cx = self.cx;
        let n = n.min(cols - cx);
        let blank = self.blank();
        let buf = self.buf_mut();
        let line = &mut buf[row..row + cols];
        line.copy_within(cx + n.., cx);
        line[cols - n..].fill(blank);
    }

    pub fn erase_chars(&mut self, n: usize) {
        let cols = self.cols;
        let row = self.idx(0, self.cy);
        let cx = self.cx;
        let end = (cx + n).min(cols);
        let blank = self.blank();
        let buf = self.buf_mut();
        buf[row + cx..row + end].fill(blank);
    }

    // ---- erase ---------------------------------------------------------

    pub fn erase_display(&mut self, mode: u16) {
        let blank = self.blank();
        let cur = self.idx(self.cx, self.cy);
        let len = self.buf().len();
        let buf = self.buf_mut();
        match mode {
            0 => buf[cur..len].fill(blank),
            1 => buf[0..=cur].fill(blank),
            _ => buf.fill(blank),
        }
    }

    pub fn erase_line(&mut self, mode: u16) {
        let cols = self.cols;
        let cx = self.cx;
        let line = self.idx(0, self.cy);
        let blank = self.blank();
        let buf = self.buf_mut();
        match mode {
            0 => buf[line + cx..line + cols].fill(blank),
            1 => buf[line..=line + cx].fill(blank),
            _ => buf[line..line + cols].fill(blank),
        }
    }

    pub fn reset_attrs(&mut self) {
        self.fg = DEFAULT_FG;
        self.bg = DEFAULT_BG;
        self.flags = 0;
    }

    // ---- scroll region / cursor save / alt screen ----------------------

    pub fn set_scroll_region(&mut self, top: usize, bot: usize) {
        let top = top.min(self.rows - 1);
        let bot = bot.min(self.rows - 1);
        if top < bot {
            self.top = top;
            self.bot = bot;
        } else {
            self.top = 0;
            self.bot = self.rows - 1;
        }
        self.set_cursor(0, 0);
    }

    pub fn save_cursor(&mut self) {
        self.saved = Some(Saved {
            cx: self.cx,
            cy: self.cy,
            fg: self.fg,
            bg: self.bg,
            flags: self.flags,
        });
    }

    pub fn restore_cursor(&mut self) {
        if let Some(s) = self.saved {
            self.cx = s.cx.min(self.cols - 1);
            self.cy = s.cy.min(self.rows - 1);
            self.fg = s.fg;
            self.bg = s.bg;
            self.flags = s.flags;
            self.wrap_pending = false;
        }
    }

    pub fn enter_alt(&mut self) {
        if self.on_alt {
            return;
        }
        self.primary_cursor = (self.cx, self.cy);
        self.on_alt = true;
        self.alt.fill(Cell::default());
        self.top = 0;
        self.bot = self.rows - 1;
        self.set_cursor(0, 0);
        self.view_offset = 0;
    }

    pub fn leave_alt(&mut self) {
        if !self.on_alt {
            return;
        }
        self.on_alt = false;
        self.top = 0;
        self.bot = self.rows - 1;
        let (x, y) = self.primary_cursor;
        self.set_cursor(x, y);
    }

    pub fn full_reset(&mut self) {
        self.primary.fill(Cell::default());
        self.alt.fill(Cell::default());
        self.scrollback.clear();
        self.on_alt = false;
        self.reset_attrs();
        self.top = 0;
        self.bot = self.rows - 1;
        self.cx = 0;
        self.cy = 0;
        self.wrap_pending = false;
        self.cursor_visible = true;
        self.cursor_style = CursorStyle::Block;
        self.app_cursor = false;
        self.bracketed_paste = false;
        self.view_offset = 0;
        self.saved = None;
    }

    // ---- scrollback view ----------------------------------------------

    pub fn max_offset(&self) -> usize {
        self.scrollback.len()
    }

    pub fn clamp_view(&mut self) {
        self.view_offset = self.view_offset.min(self.max_offset());
    }

    pub fn scroll_view(&mut self, delta: isize) {
        let max = self.max_offset() as isize;
        let v = (self.view_offset as isize + delta).clamp(0, max);
        self.view_offset = v as usize;
    }

    pub fn snap_to_bottom(&mut self) {
        self.view_offset = 0;
    }

    // ---- selection ----------------------------------------------------

    /// Cells of a virtual line (scrollback lines first, then active rows).
    fn line_cells(&self, vline: usize) -> &[Cell] {
        let sb = self.scrollback.len();
        if vline < sb {
            &self.scrollback[vline]
        } else {
            let r = vline - sb;
            let start = r * self.cols;
            &self.buf()[start..start + self.cols]
        }
    }

    /// Map a display row (0..rows) to a virtual line index.
    pub fn display_vline(&self, y: usize) -> usize {
        (self.scrollback.len() - self.view_offset) + y
    }

    pub fn sel_begin(&mut self, vline: usize, col: usize) {
        self.selection = Some(Sel { a: (vline, col), b: (vline, col) });
    }

    pub fn sel_update(&mut self, vline: usize, col: usize) {
        if let Some(s) = self.selection.as_mut() {
            s.b = (vline, col);
        }
    }

    pub fn sel_clear(&mut self) {
        self.selection = None;
    }

    /// Selection normalized to reading order: (a_line, a_col, b_line, b_col).
    pub fn sel_norm(&self) -> Option<(usize, usize, usize, usize)> {
        let s = self.selection?;
        let (mut a, mut b) = (s.a, s.b);
        if (b.0, b.1) < (a.0, a.1) {
            std::mem::swap(&mut a, &mut b);
        }
        Some((a.0, a.1, b.0, b.1))
    }

    /// Extract the selected text, trimming trailing blanks per line.
    pub fn selection_text(&self) -> String {
        let Some((a_line, a_col, b_line, b_col)) = self.sel_norm() else {
            return String::new();
        };
        let total = self.scrollback.len() + self.rows;
        let mut out = String::new();
        for vline in a_line..=b_line.min(total.saturating_sub(1)) {
            let cells = self.line_cells(vline);
            let from = if vline == a_line { a_col } else { 0 };
            let to = if vline == b_line { b_col } else { self.cols - 1 };
            let mut line = String::new();
            for x in from..=to.min(cells.len().saturating_sub(1)) {
                line.push(cells[x].ch);
            }
            out.push_str(line.trim_end());
            if vline != b_line {
                out.push_str("\r\n");
            }
        }
        out
    }

    /// Cells of display row `y` (0..rows), honoring the scrollback view offset.
    pub fn display_line(&self, y: usize) -> &[Cell] {
        let sb = self.scrollback.len();
        let vy = (sb - self.view_offset) + y;
        if vy < sb {
            &self.scrollback[vy]
        } else {
            let r = vy - sb;
            let start = r * self.cols;
            &self.buf()[start..start + self.cols]
        }
    }
}
