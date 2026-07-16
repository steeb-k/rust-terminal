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

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, ScreenToClient,
    SetWindowRgn, HBRUSH, HRGN, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
    TME_NONCLIENT, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
    GetMessageW, LoadCursorW, PostQuitMessage, RegisterClassW, ShowWindow, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION,
    HTCLIENT, HTCLOSE, HTLEFT, HTMAXBUTTON, HTMINBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    IDC_ARROW, LoadIconW, MA_ACTIVATE, MA_ACTIVATEANDEAT, MINMAXINFO, MSG, SW_MAXIMIZE, SW_MINIMIZE,
    SW_RESTORE, SW_SHOW, WM_CHAR,
    WM_CREATE,
    WM_DESTROY, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCHITTEST,
    WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_NCPAINT, WM_PAINT,
    WM_PRINTCLIENT, WM_SIZE, WNDCLASSW,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};
use windows::Win32::UI::WindowsAndMessaging::IsZoomed;

use std::sync::atomic::{AtomicU64, Ordering};

use chrome::{CaptionBtn, Hit, BORDER, CHROME_H, PAD_X, PAD_Y, RADIUS};
use config::Config;
use conpty::{Pty, WM_PTY_DATA, WM_PTY_EXIT};
use parser::Term;
use render::{Chrome, Fonts};

/// WM_MOUSELEAVE (lives behind the Win32_UI_Controls feature; define it here).
const WM_MOUSELEAVE: u32 = 0x02A3;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A clean default tab name for a shell command line.
fn shell_label(shell: &str) -> String {
    let s = shell.to_lowercase();
    if s.contains("powershell") || s.contains("pwsh") {
        "PowerShell".into()
    } else if s.contains("cmd") {
        "Command Prompt".into()
    } else {
        "Shell".into()
    }
}

/// Is Windows PowerShell present on this system? (PE images may omit it.)
fn powershell_available() -> bool {
    if let Ok(root) = std::env::var("SystemRoot") {
        let p = format!("{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        return std::path::Path::new(&p).exists();
    }
    false
}

/// Does `System32\<rel>` exist? (PE images are trimmed, so nothing is assumed.)
fn in_system32(rel: &str) -> bool {
    match std::env::var("SystemRoot") {
        Ok(root) => std::path::Path::new(&format!("{root}\\System32\\{rel}")).exists(),
        Err(_) => false,
    }
}

/// Is `exe` reachable on PATH? Used for shells that aren't part of Windows
/// itself (PowerShell 7 installs outside System32 and is absent from most PEs).
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| d.join(exe).is_file())
    })
}

/// One entry in the new-tab dropdown.
#[derive(Clone)]
struct ShellChoice {
    label: String,
    cmd: String,
}

/// The shells to offer in the new-tab dropdown: whichever of the standard ones
/// this image actually ships, plus the configured shell when it's a custom one.
/// Never empty — cmd.exe is the last resort even if the probe came up empty.
fn available_shells(cfg_shell: &str) -> Vec<ShellChoice> {
    fn choice(label: &str, cmd: &str) -> ShellChoice {
        ShellChoice { label: label.into(), cmd: cmd.into() }
    }
    let mut v: Vec<ShellChoice> = Vec::new();
    if powershell_available() {
        v.push(choice("PowerShell", "powershell.exe"));
    }
    if in_system32("cmd.exe") {
        v.push(choice("Command Prompt", "cmd.exe"));
    }
    if on_path("pwsh.exe") {
        v.push(choice("PowerShell 7", "pwsh.exe"));
    }
    // A custom registry `Shell` (anything that isn't one of the above) leads the
    // list, so the configured default stays the first/most obvious choice.
    let cfg = cfg_shell.trim();
    let known = ["powershell.exe", "cmd.exe", "pwsh.exe"];
    if !cfg.is_empty() && !known.contains(&cfg.to_lowercase().as_str()) {
        v.insert(0, choice(&shell_label(cfg), cfg));
    }
    if v.is_empty() {
        v.push(choice("Command Prompt", "cmd.exe"));
    }
    v
}

struct Session {
    pty: Pty,
    term: Term,
    /// Default label (from the shell) when the program sets no usable title.
    label: String,
}

struct App {
    fonts: Fonts,
    tabs: Vec<Session>,
    active: usize,
    accent: u32,
    selecting: bool,
    hover: Option<CaptionBtn>,
    hover_close: Option<usize>,
    /// Whether the window currently has focus (drives the border color).
    focused: bool,
    ps_available: bool,
    /// Shells offered by the new-tab dropdown.
    shells: Vec<ShellChoice>,
    /// Whether the new-tab dropdown is showing.
    menu_open: bool,
    /// Index of the hovered dropdown item, if any.
    menu_hover: Option<usize>,
    cfg: Config,
}

impl App {
    fn cur(&self) -> &Session {
        &self.tabs[self.active]
    }
    fn cur_mut(&mut self) -> &mut Session {
        &mut self.tabs[self.active]
    }
    fn labels(&self) -> Vec<String> {
        self.tabs
            .iter()
            .map(|s| {
                let t = &s.term.title;
                // Ignore PowerShell's noisy "...\powershell.exe" path title.
                if t.is_empty() || t.ends_with(".exe") {
                    s.label.clone()
                } else {
                    t.clone()
                }
            })
            .collect()
    }
    fn shell_labels(&self) -> Vec<String> {
        self.shells.iter().map(|s| s.label.clone()).collect()
    }
}

/// Close the new-tab dropdown if it's open; returns whether anything changed.
fn close_menu(app: &mut App) -> bool {
    let was = app.menu_open;
    app.menu_open = false;
    app.menu_hover = None;
    was
}

thread_local! {
    static STATE: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Grid (cols, rows) for the terminal area (client minus chrome and padding).
fn term_grid_size(hwnd: HWND, fonts: &Fonts) -> (u16, u16) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let w = (rc.right - rc.left - 2 * PAD_X).max(1);
    let h = (rc.bottom - rc.top - CHROME_H - 2 * PAD_Y).max(1);
    fonts.grid_size(w, h)
}

fn spawn_session(hwnd: HWND, shell: &str, cols: u16, rows: u16, scrollback: usize) -> Option<Session> {
    match Pty::spawn(next_id(), shell, cols, rows, hwnd) {
        Ok(pty) => Some(Session {
            pty,
            term: Term::new(cols as usize, rows as usize, scrollback),
            label: shell_label(shell),
        }),
        Err(_) => None,
    }
}

/// Map a mouse `lparam` to a (virtual line, column) in the active grid, taking
/// the chrome offset into account.
fn mouse_cell(app: &App, lparam: LPARAM) -> (usize, usize) {
    let x = (lparam.0 & 0xffff) as i16 as i32 - PAD_X;
    let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32 - CHROME_H - PAD_Y;
    let g = &app.cur().term.grid;
    let col = (x.max(0) / app.fonts.cw).clamp(0, g.cols as i32 - 1) as usize;
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
        // App icon embedded by build.rs as resource id 101.
        let hicon = LoadIconW(hinstance, PCWSTR(101 as *const u16)).unwrap_or_default();
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hIcon: hicon,
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
            // Overlapped (not WS_POPUP): CW_USEDEFAULT only positions overlapped
            // windows via the OS cascade — for a popup it collapses to (0,0), which
            // is why the window used to open jammed in the top-left corner. The
            // frame is still removed by our WM_NCCALCSIZE / WM_NCPAINT handling.
            WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            960,
            600,
            None,
            None,
            hinstance,
            None,
        )?;

        // Remove the Win11 window border (the focus-tinted edge). This is a DWM
        // attribute: it takes effect on the desktop and is a harmless no-op in
        // WinPE, which has no DWM and draws no such border. WM_NCPAINT below
        // suppresses any classic frame border too.
        let none = 0xFFFF_FFFEu32; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const u32 as *const core::ffi::c_void,
            4,
        );

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

            // No non-client area to paint (we draw the whole window ourselves);
            // suppress the classic frame border, which works even without DWM.
            WM_NCPAINT => LRESULT(0),

            // On focus change, stop DefWindowProc from repainting the (inactive)
            // non-client frame — that gray edge is exactly this. Passing -1 as
            // lParam suppresses the NC repaint while keeping the window "active".
            // Track focus so the self-drawn border tints accent / gray, and
            // repaint to reflect it.
            WM_NCACTIVATE => {
                let active = wparam.0 != 0;
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        app.focused = active;
                        // A dropdown left open behind a focus change is a ghost.
                        if !active {
                            close_menu(app);
                        }
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                DefWindowProcW(hwnd, msg, wparam, LPARAM(-1))
            }

            // The click that focuses an inactive window should NOT also register
            // in the terminal (it would start a selection and could pause output).
            // Eat it for the terminal area; let chrome clicks (tabs/buttons) work.
            WM_MOUSEACTIVATE => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                if pt.y >= CHROME_H {
                    LRESULT(MA_ACTIVATEANDEAT as isize)
                } else {
                    LRESULT(MA_ACTIVATE as isize)
                }
            }

            WM_CREATE => {
                let cfg = Config::load();
                let fonts = Fonts::new(cfg.font_px, &cfg.font_face);
                let (cols, rows) = term_grid_size(hwnd, &fonts);
                let ps_available = powershell_available();
                let shell_choices = available_shells(&cfg.shell);

                // First-launch tabs: if PowerShell is present, open both shells so
                // the user can switch (Ctrl+Tab); otherwise just Command Prompt.
                let shells: &[&str] = if ps_available {
                    &["powershell.exe", "cmd.exe"]
                } else {
                    &["cmd.exe"]
                };
                let tabs: Vec<Session> = shells
                    .iter()
                    .filter_map(|sh| spawn_session(hwnd, sh, cols, rows, cfg.scrollback))
                    .collect();
                if tabs.is_empty() {
                    PostQuitMessage(1);
                } else {
                    STATE.with_borrow_mut(|s| {
                        *s = Some(App {
                            fonts,
                            tabs,
                            active: 0,
                            accent: cfg.accent,
                            selecting: false,
                            hover: None,
                            hover_close: None,
                            focused: true,
                            ps_available,
                            shells: shell_choices,
                            menu_open: false,
                            menu_hover: None,
                            cfg,
                        })
                    });
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

            WM_PTY_EXIT => {
                // A shell exited (e.g. the user typed `exit`): close that tab.
                let id = wparam.0 as u64;
                let empty = STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        if let Some(idx) = app.tabs.iter().position(|t| t.pty.id == id) {
                            app.tabs.remove(idx);
                            if app.tabs.is_empty() {
                                return true;
                            }
                            app.active = app.active.min(app.tabs.len() - 1);
                        }
                    }
                    false
                });
                if empty {
                    let _ = DestroyWindow(hwnd);
                } else {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }

            WM_PAINT => {
                let is_max = IsZoomed(hwnd).as_bool();
                STATE.with_borrow(|s| {
                    if let Some(app) = s.as_ref() {
                        let labels = app.labels();
                        let menu_labels = app.shell_labels();
                        let chrome = Chrome {
                            labels: &labels,
                            active: app.active,
                            accent: app.accent,
                            is_max,
                            hover: app.hover,
                            hover_close: app.hover_close,
                            focused: app.focused,
                            menu: app.menu_open.then_some(menu_labels.as_slice()),
                            menu_hover: app.menu_hover,
                        };
                        render::paint(hwnd, &app.cur().term, &app.fonts, &chrome);
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
                        let menu_labels = app.shell_labels();
                        let chrome = Chrome {
                            labels: &labels,
                            active: app.active,
                            accent: app.accent,
                            is_max,
                            hover: app.hover,
                            hover_close: app.hover_close,
                            focused: app.focused,
                            menu: app.menu_open.then_some(menu_labels.as_slice()),
                            menu_hover: app.menu_hover,
                        };
                        render::render_to(hdc, rc.right, rc.bottom, &app.cur().term, &app.fonts, &chrome);
                    }
                });
                LRESULT(0)
            }

            WM_SIZE => {
                STATE.with_borrow_mut(|s| {
                    if let Some(app) = s.as_mut() {
                        // The dropdown is positioned off the chrome, which moves.
                        close_menu(app);
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
                let ctrl = key_down(VK_CONTROL);
                let shift = key_down(VK_SHIFT);
                // Swallow the control chars for keys we handle as app shortcuts so
                // they never reach the shell. Plain Ctrl+C (0x03) still passes.
                if ctrl && shift {
                    return LRESULT(0);
                }
                if ctrl && matches!(wparam.0 as u16, 0x14 | 0x17 | 0x09) {
                    // Ctrl+T (0x14), Ctrl+W (0x17), Ctrl+Tab/Ctrl+I (0x09)
                    return LRESULT(0);
                }
                if matches!(wparam.0 as u16, 0x08 | 0x7f) {
                    // Backspace / Ctrl+Backspace are encoded in WM_KEYDOWN.
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
                // Any keystroke dismisses the new-tab dropdown; Escape does only
                // that (it must not also reach the shell).
                if STATE.with_borrow_mut(|s| s.as_mut().map(close_menu).unwrap_or(false)) {
                    let _ = InvalidateRect(hwnd, None, false);
                    if vk == VK_ESCAPE {
                        return LRESULT(0);
                    }
                }
                let ctrl = key_down(VK_CONTROL);
                let shift = key_down(VK_SHIFT);
                if ctrl && shift {
                    match vk {
                        VIRTUAL_KEY(0x43) => copy_selection(hwnd),  // Ctrl+Shift+C
                        VIRTUAL_KEY(0x56) => paste_clipboard(hwnd), // Ctrl+Shift+V
                        VK_TAB => switch_tab(hwnd, -1),             // Ctrl+Shift+Tab
                        _ => {}
                    }
                    return LRESULT(0);
                }
                if ctrl {
                    match vk {
                        VIRTUAL_KEY(0x54) => {
                            new_tab(hwnd); // Ctrl+T
                            return LRESULT(0);
                        }
                        VIRTUAL_KEY(0x57) => {
                            close_tab(hwnd, None); // Ctrl+W
                            return LRESULT(0);
                        }
                        VK_TAB => {
                            switch_tab(hwnd, 1); // Ctrl+Tab
                            return LRESULT(0);
                        }
                        _ => {} // Ctrl+C/V etc. fall through to the shell
                    }
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

                // An open dropdown swallows the click: on an item it opens that
                // shell, anywhere else (including the "+" again) it dismisses.
                let nshells = STATE
                    .with_borrow(|s| s.as_ref().filter(|a| a.menu_open).map(|a| a.shells.len()));
                if let Some(n) = nshells {
                    let pick = chrome::menu_hit(ntabs, n, rc.right, x, y);
                    let cmd = STATE.with_borrow_mut(|s| {
                        let app = s.as_mut()?;
                        close_menu(app);
                        pick.map(|i| app.shells[i].cmd.clone())
                    });
                    match cmd {
                        Some(cmd) => new_tab_with(hwnd, &cmd),
                        None => {
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
                    return LRESULT(0);
                }

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
                    Hit::NewTab => {
                        // One shell available: nothing to choose between.
                        let single = STATE
                            .with_borrow(|s| s.as_ref().map(|a| a.shells.len() <= 1).unwrap_or(true));
                        if single {
                            new_tab(hwnd);
                        } else {
                            STATE.with_borrow_mut(|s| {
                                if let Some(app) = s.as_mut() {
                                    app.menu_open = true;
                                    app.menu_hover = None;
                                }
                            });
                            let _ = InvalidateRect(hwnd, None, false);
                        }
                    }
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
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let dragging = wparam.0 & 0x0001 != 0;
                let changed = STATE.with_borrow_mut(|s| {
                    let Some(app) = s.as_mut() else { return false };
                    let mut ch = false;
                    // Highlight a tab's close button when the cursor is over it.
                    let hc = if y < CHROME_H {
                        match chrome::hit(app.tabs.len(), rc.right, x, y) {
                            Hit::Close(i) => Some(i),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if app.hover_close != hc {
                        app.hover_close = hc;
                        ch = true;
                    }
                    // Highlight the dropdown item under the cursor.
                    let mh = app
                        .menu_open
                        .then(|| chrome::menu_hit(app.tabs.len(), app.shells.len(), rc.right, x, y))
                        .flatten();
                    if app.menu_hover != mh {
                        app.menu_hover = mh;
                        ch = true;
                    }
                    if app.selecting && dragging {
                        let (vline, col) = mouse_cell(app, lparam);
                        app.cur_mut().term.grid.sel_update(vline, col);
                        ch = true;
                    }
                    ch
                });
                // Arm a leave notification so the close hover clears on exit.
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                if changed {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }

            WM_MOUSELEAVE => {
                let changed = STATE.with_borrow_mut(|s| match s.as_mut() {
                    Some(app) if app.hover_close.is_some() => {
                        app.hover_close = None;
                        true
                    }
                    _ => false,
                });
                if changed {
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

            WM_NCMOUSEMOVE => {
                let btn = match wparam.0 as u32 {
                    HTMINBUTTON => Some(CaptionBtn::Min),
                    HTMAXBUTTON => Some(CaptionBtn::Max),
                    HTCLOSE => Some(CaptionBtn::Close),
                    _ => None,
                };
                set_hover(hwnd, btn);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_NCMOUSELEAVE => {
                set_hover(hwnd, None);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            // Handle caption-button clicks ourselves. Letting DefWindowProc do it
            // makes it briefly draw the native (classic Win9x-looking) caption
            // buttons over our own — intercepting the press suppresses that.
            WM_NCLBUTTONDOWN => match wparam.0 as u32 {
                HTMINBUTTON => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                    LRESULT(0)
                }
                HTMAXBUTTON => {
                    let cmd = if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE };
                    let _ = ShowWindow(hwnd, cmd);
                    LRESULT(0)
                }
                HTCLOSE => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            },

            // Swallow double-clicks on the caption buttons too, so a fast second
            // click can't slip through to DefWindowProc and flash a native button.
            WM_NCLBUTTONDBLCLK => match wparam.0 as u32 {
                HTMINBUTTON | HTMAXBUTTON | HTCLOSE => LRESULT(0),
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            },

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

/// Update the hovered caption button and repaint if it changed; arm a leave
/// notification while a button is hovered.
fn set_hover(hwnd: HWND, btn: Option<CaptionBtn>) {
    let changed = STATE.with_borrow_mut(|s| match s.as_mut() {
        Some(app) if app.hover != btn => {
            app.hover = btn;
            true
        }
        _ => false,
    });
    if btn.is_some() {
        unsafe {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE | TME_NONCLIENT,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
        }
    }
    if changed {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

/// Move the active tab by `dir` (+1 next, -1 previous), wrapping.
fn switch_tab(hwnd: HWND, dir: isize) {
    STATE.with_borrow_mut(|s| {
        if let Some(app) = s.as_mut() {
            let n = app.tabs.len() as isize;
            if n > 1 {
                app.active = (app.active as isize + dir).rem_euclid(n) as usize;
            }
        }
    });
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// Open a tab running the configured default shell (Ctrl+T, and the "+" button
/// when there's only one shell to choose from).
fn new_tab(hwnd: HWND) {
    let shell = STATE.with_borrow(|s| {
        s.as_ref().map(|a| {
            // Fall back to cmd if the configured shell is PowerShell but absent.
            let want_ps = a.cfg.shell.to_lowercase().contains("powershell");
            if want_ps && !a.ps_available {
                "cmd.exe".to_string()
            } else {
                a.cfg.shell.clone()
            }
        })
    });
    if let Some(shell) = shell {
        new_tab_with(hwnd, &shell);
    }
}

/// Open a tab running `shell` (a pick from the new-tab dropdown).
fn new_tab_with(hwnd: HWND, shell: &str) {
    let params = STATE.with_borrow(|s| {
        s.as_ref().map(|a| {
            let (cols, rows) = term_grid_size(hwnd, &a.fonts);
            (cols, rows, a.cfg.scrollback)
        })
    });
    let Some((cols, rows, scrollback)) = params else {
        return;
    };
    if let Some(sess) = spawn_session(hwnd, shell, cols, rows, scrollback) {
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
