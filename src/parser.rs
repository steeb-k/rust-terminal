// SPDX-License-Identifier: GPL-3.0-or-later
//! VT/ANSI parsing. PTY bytes feed `vte::Parser`; the `Perform` impl mutates the
//! `Grid`. Replies to device queries (DSR/DA) accumulate in `responses`, which
//! the caller writes back to the PTY after each feed. Unknown sequences are
//! ignored so they can never corrupt the screen.

use vte::{Params, Parser, Perform};

use crate::colors::{xterm256, ANSI16, DEFAULT_BG, DEFAULT_FG};
use crate::grid::{
    CursorStyle, Grid, FLAG_BOLD, FLAG_ITALIC, FLAG_REVERSE, FLAG_UNDERLINE,
};

pub struct Term {
    parser: Parser,
    pub grid: Grid,
    pub title: String,
    /// Bytes to send back to the shell (replies to DSR/DA queries).
    pub responses: Vec<u8>,
}

impl Term {
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        Term {
            parser: Parser::new(),
            grid: Grid::new(cols, rows, scrollback),
            title: String::new(),
            responses: Vec::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let sb_before = self.grid.scrollback.len();
        let mut perf = Performer {
            grid: &mut self.grid,
            title: &mut self.title,
            responses: &mut self.responses,
        };
        for &b in bytes {
            self.parser.advance(&mut perf, b);
        }
        let grew = self.grid.scrollback.len().saturating_sub(sb_before);
        if grew > 0 {
            // Scrollback indices shifted; drop any selection to avoid stale highlight.
            self.grid.sel_clear();
            // Keep the viewport anchored to the same content while scrolled up.
            if self.grid.view_offset > 0 {
                self.grid.scroll_view(grew as isize);
            }
        }
    }
}

struct Performer<'a> {
    grid: &'a mut Grid,
    title: &'a mut String,
    responses: &'a mut Vec<u8>,
}

fn flat(params: &Params) -> Vec<u16> {
    let mut v = Vec::new();
    for p in params.iter() {
        v.push(*p.first().unwrap_or(&0));
    }
    v
}

impl<'a> Performer<'a> {
    fn sgr(&mut self, params: &Params) {
        let p = flat(params);
        if p.is_empty() {
            self.grid.reset_attrs();
            return;
        }
        let mut i = 0;
        while i < p.len() {
            match p[i] {
                0 => self.grid.reset_attrs(),
                1 => self.grid.flags |= FLAG_BOLD,
                3 => self.grid.flags |= FLAG_ITALIC,
                4 => self.grid.flags |= FLAG_UNDERLINE,
                7 => self.grid.flags |= FLAG_REVERSE,
                22 => self.grid.flags &= !FLAG_BOLD,
                23 => self.grid.flags &= !FLAG_ITALIC,
                24 => self.grid.flags &= !FLAG_UNDERLINE,
                27 => self.grid.flags &= !FLAG_REVERSE,
                30..=37 => self.grid.fg = ANSI16[(p[i] - 30) as usize],
                39 => self.grid.fg = DEFAULT_FG,
                40..=47 => self.grid.bg = ANSI16[(p[i] - 40) as usize],
                49 => self.grid.bg = DEFAULT_BG,
                90..=97 => self.grid.fg = ANSI16[(p[i] - 90 + 8) as usize],
                100..=107 => self.grid.bg = ANSI16[(p[i] - 100 + 8) as usize],
                38 | 48 => {
                    let is_fg = p[i] == 38;
                    if let Some(&mode) = p.get(i + 1) {
                        if mode == 5 {
                            if let Some(&n) = p.get(i + 2) {
                                let col = xterm256(n as u8);
                                if is_fg { self.grid.fg = col } else { self.grid.bg = col }
                            }
                            i += 2;
                        } else if mode == 2 {
                            if let (Some(&r), Some(&g), Some(&b)) =
                                (p.get(i + 2), p.get(i + 3), p.get(i + 4))
                            {
                                let col = crate::colors::rgb(r as u8, g as u8, b as u8);
                                if is_fg { self.grid.fg = col } else { self.grid.bg = col }
                            }
                            i += 4;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_mode(&mut self, modes: &[u16], on: bool) {
        for &m in modes {
            match m {
                1 => self.grid.app_cursor = on,
                25 => self.grid.cursor_visible = on,
                1000 => self.grid.mouse_report = on,
                1006 => self.grid.mouse_sgr = on,
                2004 => self.grid.bracketed_paste = on,
                1049 | 47 | 1047 => {
                    if on {
                        self.grid.enter_alt();
                    } else {
                        self.grid.leave_alt();
                    }
                }
                _ => {}
            }
        }
    }
}

impl<'a> Perform for Performer<'a> {
    fn print(&mut self, c: char) {
        self.grid.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.grid.backspace(),
            0x09 => self.grid.tab(),
            0x0A | 0x0B | 0x0C => self.grid.line_feed(),
            0x0D => self.grid.carriage_return(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let p = flat(params);
        let n = |idx: usize, def: usize| -> usize {
            match p.get(idx).copied().unwrap_or(0) {
                0 => def,
                v => v as usize,
            }
        };
        let private = intermediates.first() == Some(&b'?');
        let space = intermediates.first() == Some(&b' ');
        match action {
            'A' => self.grid.move_cursor(0, -(n(0, 1) as isize)),
            'B' | 'e' => self.grid.move_cursor(0, n(0, 1) as isize),
            'C' | 'a' => self.grid.move_cursor(n(0, 1) as isize, 0),
            'D' => self.grid.move_cursor(-(n(0, 1) as isize), 0),
            'E' => {
                // CNL: cursor next line
                let dy = n(0, 1) as isize;
                self.grid.move_cursor(0, dy);
                self.grid.carriage_return();
            }
            'F' => {
                let dy = n(0, 1) as isize;
                self.grid.move_cursor(0, -dy);
                self.grid.carriage_return();
            }
            'G' | '`' => {
                let y = self.grid.cy;
                self.grid.set_cursor(n(0, 1) - 1, y);
            }
            'd' => {
                let x = self.grid.cx;
                self.grid.set_cursor(x, n(0, 1) - 1);
            }
            'H' | 'f' => self.grid.set_cursor(n(1, 1) - 1, n(0, 1) - 1),
            'J' => self.grid.erase_display(p.first().copied().unwrap_or(0)),
            'K' => self.grid.erase_line(p.first().copied().unwrap_or(0)),
            'L' => self.grid.insert_lines(n(0, 1)),
            'M' => self.grid.delete_lines(n(0, 1)),
            '@' => self.grid.insert_chars(n(0, 1)),
            'P' => self.grid.delete_chars(n(0, 1)),
            'X' => self.grid.erase_chars(n(0, 1)),
            'S' => {
                let (t, b) = (self.grid.top, self.grid.bot);
                self.grid.scroll_up_in(t, b, n(0, 1), false);
            }
            'T' => {
                let (t, b) = (self.grid.top, self.grid.bot);
                self.grid.scroll_down_in(t, b, n(0, 1));
            }
            'r' => self.grid.set_scroll_region(n(0, 1) - 1, n(1, self.grid.rows) - 1),
            's' => self.grid.save_cursor(),
            'u' => self.grid.restore_cursor(),
            'h' => self.set_mode(&p, true),
            'l' => self.set_mode(&p, false),
            'm' => self.sgr(params),
            'n' if !private => {
                // Device Status Report.
                match p.first().copied().unwrap_or(0) {
                    5 => self.responses.extend_from_slice(b"\x1b[0n"),
                    6 => {
                        let r = self.grid.cy + 1;
                        let c = self.grid.cx + 1;
                        self.responses
                            .extend_from_slice(format!("\x1b[{r};{c}R").as_bytes());
                    }
                    _ => {}
                }
            }
            'c' if !private => {
                // Primary Device Attributes: claim to be a VT100 with AVO.
                self.responses.extend_from_slice(b"\x1b[?1;2c");
            }
            'q' if space => {
                // DECSCUSR: set cursor style.
                self.grid.cursor_style = match p.first().copied().unwrap_or(1) {
                    3 | 4 => CursorStyle::Underline,
                    5 | 6 => CursorStyle::Bar,
                    _ => CursorStyle::Block,
                };
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' => self.grid.save_cursor(),
            b'8' => self.grid.restore_cursor(),
            b'D' => self.grid.line_feed(),         // IND
            b'E' => {
                self.grid.line_feed();
                self.grid.carriage_return();
            } // NEL
            b'M' => self.grid.reverse_line_feed(), // RI
            b'c' => self.grid.full_reset(),        // RIS
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let (Some(&kind), Some(&text)) = (params.first(), params.get(1)) {
            if kind == b"0" || kind == b"2" {
                *self.title = String::from_utf8_lossy(text).into_owned();
            }
        }
    }
}
