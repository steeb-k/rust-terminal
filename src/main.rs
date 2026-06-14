// SPDX-License-Identifier: GPL-3.0-or-later
//! rust-terminal — a GDI-rendered terminal emulator for Windows PE.
//!
//! M4: borderless dark window (self-drawn tab strip, rounded, drag/resize) with
//! multiple tabs, each owning its own ConPTY session. Registry config and the
//! shared StartPE accent arrive in M5.

// GUI app: no console window. The pseudoconsole spawns its own conhost, so this
// does not affect ConPTY input/output.
#![windows_subsystem = "windows"]

mod chrome;
mod clipboard;
mod colors;
mod config;
mod conpty;
mod grid;
mod input;
mod parser;
mod render;

use std::cell::RefCell;

use windows::core::{w, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, ScreenToClient,
    SetWindowRgn, HBRUSH, HRGN, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CONTROL, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    LoadCursorW, PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTCLOSE,
    HTLEFT, HTMAXBUTTON, HTMINBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IDC_ARROW, MINMAXINFO,
    MSG, SW_SHOW, WM_CHAR, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT,
    WM_PRINTCLIENT, WM_SIZE, WNDCLASSW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
    WS_THICKFRAME,
};
use windows::Win32::UI::WindowsAndMessaging::IsZoomed;

use chrome::{Hit, BORDER, CHROME_H};
use config::Config;
use conpty::{Pty, WM_PTY_DATA};
use parser::Term;
use render::Fonts;

/// Window corner radius.
const RADIUS: i32 = 12;

struct Session {
    pty: Pty,
    term: Term,
}

struct App {
    fonts: Fonts,
    tabs: Vec<Session>,
    active: usize,
    accent: u32,
    selecting: bool,
    cfg: Config,
}

impl App {
    fn cur(&self) -> &Session {
        &self.tabs[self.active]
    }
    fn cur_mut(&mut self) -> &mut Session {
        &mut self.tabs[self.active]
    }
    /// A clean default tab name derived from the configured shell.
    fn default_label(&self) -> &'static str {
        let s = self.cfg.shell.to_lowercase();
        if s.contains("powershell") || s.contains("pwsh") {
            "PowerShell"
        } else if s.contains("cmd") {
            "Command Prompt"
        } else {
            "Shell"
        }
    }
    fn labels(&self) -> Vec<String> {
        let default = self.default_label();
        self.tabs
            .iter()
            .map(|s| {
                let t = &s.term.title;
                // Ignore PowerShell's noisy "...\powershell.exe" path title.
                if t.is_empty() || t.ends_with(".exe") {
                    default.to_string()
                } else {
                    t.clone()
                }
            })
            .collect()
    }
}

thread_local! {
    static STATE: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Grid (cols, rows) for the terminal area (client minus the chrome strip).
fn term_grid_size(hwnd: HWND, fonts: &Fonts) -> (u16, u16) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    fonts.grid_size(rc.right - rc.left, (rc.bottom - rc.top - CHROME_H).max(1))
}

fn spawn_session(hwnd: HWND, shell: &str, cols: u16, rows: u16, scrollback: usize) -> Option<Session> {
    match Pty::spawn(shell, cols, rows, hwnd) {
        Ok(pty) => Some(Session { pty, term: Term::new(cols as usize, rows as usize, scrollback) }),
        Err(_) => None,
    }
}

/// Map a mouse `lparam` to a (virtual line, column) in the active grid, taking
/// the chrome offset into account.
fn mouse_cell(app: &App, lparam: LPARAM) -> (usize, usize) {
    let x = (lparam.0 & 0xffff) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32 - CHROME_H;
    let g = &app.cur().term.grid;
    let col = (x / app.fonts.cw).clamp(0, g.cols as i32 - 1) as usize;
    let row = (y.max(0) / app.fonts.ch).clamp(0, g.rows as i32 - 1) as usize;
    (g.display_vline(row), col)
}

fn copy_selection(hwnd: HWND) {
    STATE.with_borrow(|s| {
        if let Some(app) = s.as_ref() {
            let text = app.cur().term.grid.selection_text();
            if !text.is_empty() {
                clipboard::set_text(hwnd, &text);
            }
        }
    });
}

fn paste_clipboard(hwnd: HWND) {
    let Some(raw) = clipboard::get_text(hwnd) else {
        return;
    };
    let text = raw.replace("\r\n", "\r").replace('\n', "\r");
    STATE.with_borrow_mut(|s| {
        if let Some(app) = s.as_mut() {
            let sess = app.cur_mut();
            sess.term.grid.sel_clear();
            sess.term.grid.snap_to_bottom();
            let payload = if sess.term.grid.bracketed_paste {
                format!("\x1b[200~{text}\x1b[201~")
            } else {
                text
            };
            sess.pty.write(payload.as_bytes());
        }
    });
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn main() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("RustTerm_Main");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH::default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            Default::default(),
            class,
            w!("rust-terminal"),
            WS_POPUP | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            960,
            600,
            None,
            None,
            hinstance,
            None,
        )?;

        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCALCSIZE if wparam.0 != 0 => {
                // Remove the standard frame: client area covers the whole window.
                LRESULT(0)
            }

            WM_CREATE => {
                let cfg = Config::load();
                let fonts = Fonts::new(cfg.font_px, &cfg.font_face);
                let (cols, rows) = term_grid_size(hwnd, &fonts);
                match spawn_session(hwnd, &cfg.shell, cols, rows, cfg.scrollback) {
                    Some(sess) => STATE.with_borrow_mut(|s| {
                        *s = Some(App {
                            fonts,
                            tabs: vec![sess],
                            active: 0,
                            accent: cfg.accent,
                            selecting: false,
                            cfg,
                        })
                    }),
                    None => PostQuitMessage(1),
                }
                LRESULT(0)
            }

            WM_GETMINMAXINFO => {
                // Constrain maximize to the monitor work area so it doesn't cover
                // the taskbar (needed because we removed the standard frame).
                let mmi = lparam.0 as *mut MINMAXINFO;
                let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO {
                    cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                    let (wk, mo) = (mi.rcWork, mi.rcMonitor);
                    (*mmi).ptMaxPosition = POINT { x: wk.left - mo.left, y: wk.top - mo.top };
                    (*mmi).ptMaxSize = POINT { x: wk.right - wk.left, y: wk.bottom - wk.top };
                }
                (*mmi).ptMinTrackSize = POINT { x: 360, y: 220 };
                LRESULT(0)
            }

            WM_NCHITTEST => {
                let mut pt = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let (w, h) = (rc.right, rc.bottom);
                let (x, y) = (pt.x, pt.y);
                let (l, r, t, b) = (x < BORDER, x >= w - BORDER, y < BORDER, y >= h - BORDER);
                let ht: u32 = if t && l {
                    HTTOPLEFT
                } else if t && r {
                    HTTOPRIGHT
                } else if b && l {
                    HTBOTTOMLEFT
                } else if b && r {
                    HTBOTTOMRIGHT
                } else if l {
                    HTLEFT
                } else if r {
                    HTRIGHT
                } else if t {
                    HTTOP
                } else if b {
                    HTBOTTOM
                } else {
                    let ntabs = STATE.with_borrow(|s| s.as_ref().map(|a| a.tabs.len()).unwrap_or(0));
                    match chrome::hit(ntabs, w, x, y) {
                        Hit::Drag => HTCAPTION,
                        Hit::Minimize => HTMINBUTTON,
                        Hit::Maximize => HTMAXBUTTON,
                        Hit::WindowClose => HTCLOSE,
                        _ => HTCLIENT,
                    }
                };
                LRESULT(ht as isize)
            }

            WM_PTY_DATA => {
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        for sess in app.tabs.iter_mut() {
                            let bytes = sess.pty.take_pending();
                            if !bytes.is_empty() {
                                sess.term.feed(&bytes);
                            }
                            if !sess.term.responses.is_empty() {
                                let r = std::mem::take(&mut sess.term.responses);
                                sess.pty.write(&r);
                            }
                        }
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_PAINT => {
                let is_max = IsZoomed(hwnd).as_bool();
                STATE.with_borrow(|s| {
                    if let Some(app) = s.as_ref() {
                        let labels = app.labels();
                        render::paint(hwnd, &app.cur().term, &app.fonts, &labels, app.active, app.accent, is_max);
                    }
                });
                LRESULT(0)
            }

            WM_PRINTCLIENT => {
                let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
                let is_max = IsZoomed(hwnd).as_bool();
                STATE.with_borrow(|s| {
                    if let Some(app) = s.as_ref() {
                        let mut rc = RECT::default();
                        let _ = GetClientRect(hwnd, &mut rc);
                        let labels = app.labels();
                        render::render_to(hdc, rc.right, rc.bottom, &app.cur().term, &app.fonts, &labels, app.active, app.accent, is_max);
                    }
                });
                LRESULT(0)
            }

            WM_SIZE => {
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        let (cols, rows) = term_grid_size(hwnd, &app.fonts);
                        for sess in app.tabs.iter_mut() {
                            sess.term.grid.resize(cols as usize, rows as usize);
                            sess.pty.resize(cols, rows);
                        }
                    }
                });
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                // Square corners when maximized, rounded otherwise.
                if IsZoomed(hwnd).as_bool() {
                    SetWindowRgn(hwnd, HRGN::default(), true);
                } else {
                    let rgn = CreateRoundRectRgn(0, 0, rc.right + 1, rc.bottom + 1, RADIUS, RADIUS);
                    SetWindowRgn(hwnd, rgn, true);
                }
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_CHAR => {
                if key_down(VK_CONTROL) && key_down(VK_SHIFT) {
                    return LRESULT(0);
                }
                let bytes = input::char_bytes(wparam.0 as u16);
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        let sess = app.cur_mut();
                        sess.term.grid.sel_clear();
                        sess.term.grid.snap_to_bottom();
                        sess.pty.write(&bytes);
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wparam.0 as u16);
                if key_down(VK_CONTROL) && key_down(VK_SHIFT) {
                    match wparam.0 as u16 {
                        0x43 => copy_selection(hwnd),  // 'C'
                        0x56 => paste_clipboard(hwnd), // 'V'
                        0x54 => new_tab(hwnd),         // 'T'
                        0x57 => close_tab(hwnd, None), // 'W'
                        _ => {}
                    }
                    return LRESULT(0);
                }
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        let app_cursor = app.cur().term.grid.app_cursor;
                        if let Some(seq) = input::key_bytes(vk, app_cursor) {
                            let sess = app.cur_mut();
                            sess.term.grid.sel_clear();
                            sess.term.grid.snap_to_bottom();
                            sess.pty.write(&seq);
                        }
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let ntabs = STATE.with_borrow(|s| s.as_ref().map(|a| a.tabs.len()).unwrap_or(0));
                match chrome::hit(ntabs, rc.right, x, y) {
                    Hit::Tab(i) => {
                        STATE.with_borrow_mut(|s| {
                            if let Some(app) = s.as_mut() {
                                app.active = i;
                            }
                        });
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    Hit::Close(i) => close_tab(hwnd, Some(i)),
                    Hit::NewTab => new_tab(hwnd),
                    Hit::Terminal => {
                        SetCapture(hwnd);
                        STATE.with_borrow_mut(|s| {
                            if let Some(app) = s.as_mut() {
                                app.selecting = true;
                                let (vline, col) = mouse_cell(app, lparam);
                                app.cur_mut().term.grid.sel_begin(vline, col);
                            }
                        });
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    // Drag + caption buttons are handled via WM_NCHITTEST.
                    _ => {}
                }
                LRESULT(0)
            }

            WM_MOUSEMOVE => {
                if wparam.0 & 0x0001 != 0 {
                    STATE.with_borrow_mut(|s| {
                        if let Some(app) = s.as_mut() {
                            if app.selecting {
                                let (vline, col) = mouse_cell(app, lparam);
                                app.cur_mut().term.grid.sel_update(vline, col);
                            }
                        }
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }

            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        app.selecting = false;
                    }
                });
                LRESULT(0)
            }

            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16;
                let lines = (delta as i32 / 120) * 3;
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        app.cur_mut().term.grid.scroll_view(lines as isize);
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }

            WM_ERASEBKGND => LRESULT(1),

            WM_DESTROY => {
                STATE.with_borrow_mut(|s| *s = None);
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn new_tab(hwnd: HWND) {
    let params = STATE.with_borrow(|s| {
        s.as_ref().map(|a| {
            let (cols, rows) = term_grid_size(hwnd, &a.fonts);
            (cols, rows, a.cfg.shell.clone(), a.cfg.scrollback)
        })
    });
    let Some((cols, rows, shell, scrollback)) = params else {
        return;
    };
    if let Some(sess) = spawn_session(hwnd, &shell, cols, rows, scrollback) {
        STATE.with_borrow_mut(|s| {
            if let Some(app) = s.as_mut() {
                app.tabs.push(sess);
                app.active = app.tabs.len() - 1;
            }
        });
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

/// Close tab `i` (or the active tab if `None`). Quits when the last tab closes.
fn close_tab(hwnd: HWND, i: Option<usize>) {
    let empty = STATE.with_borrow_mut(|s| {
        if let Some(app) = s.as_mut() {
            let idx = i.unwrap_or(app.active).min(app.tabs.len() - 1);
            app.tabs.remove(idx); // drops the Session -> ConPTY teardown
            if app.tabs.is_empty() {
                return true;
            }
            app.active = app.active.min(app.tabs.len() - 1);
        }
        false
    });
    if empty {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    } else {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}
