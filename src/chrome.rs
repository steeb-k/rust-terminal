// SPDX-License-Identifier: GPL-3.0-or-later
//! Layout + hit-testing for the self-drawn dark chrome (titlebar/tab strip).
//! Shared by the renderer (drawing) and the window proc (clicks, NCHITTEST).

/// Height of the tab strip / titlebar, in pixels.
pub const CHROME_H: i32 = 36;
/// Fixed tab width.
pub const TAB_W: i32 = 200;
/// New-tab ("+") button width.
pub const NEWTAB_W: i32 = 30;
/// Resize-grab border thickness.
pub const BORDER: i32 = 6;
/// Caption button (min/max/close) width.
pub const BTN_W: i32 = 46;
/// Terminal content inset (padding) from the window edges, in pixels.
pub const PAD_X: i32 = 8;
pub const PAD_Y: i32 = 4;

#[derive(Clone, Copy, PartialEq)]
pub enum CaptionBtn {
    Min,
    Max,
    Close,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Hit {
    Tab(usize),
    Close(usize),
    NewTab,
    Minimize,
    Maximize,
    WindowClose,
    /// Empty strip area — drag the window.
    Drag,
    /// Below the chrome — terminal content.
    Terminal,
}

/// Left x of the three right-aligned caption buttons (min, max, close).
pub fn caption_xs(width: i32) -> (i32, i32, i32) {
    (width - 3 * BTN_W, width - 2 * BTN_W, width - BTN_W)
}

pub fn tab_x(i: usize) -> i32 {
    i as i32 * TAB_W
}

pub fn newtab_x(ntabs: usize) -> i32 {
    ntabs as i32 * TAB_W + 4
}

/// Close-box rect (x0, y0, x1, y1) inside tab `i`, vertically centered.
/// A roomy square so it's an easy click target.
pub fn close_box(i: usize) -> (i32, i32, i32, i32) {
    let x1 = tab_x(i) + TAB_W;
    let cy = CHROME_H / 2;
    (x1 - 32, cy - 12, x1 - 8, cy + 12)
}

/// Classify a client-space point within the chrome region.
pub fn hit(ntabs: usize, width: i32, x: i32, y: i32) -> Hit {
    if y >= CHROME_H {
        return Hit::Terminal;
    }
    let (min_x, max_x, close_x) = caption_xs(width);
    if x >= close_x {
        return Hit::WindowClose;
    }
    if x >= max_x {
        return Hit::Maximize;
    }
    if x >= min_x {
        return Hit::Minimize;
    }
    let nx = newtab_x(ntabs);
    if x >= nx && x < nx + NEWTAB_W {
        return Hit::NewTab;
    }
    if x >= 0 && x < ntabs as i32 * TAB_W {
        let i = (x / TAB_W) as usize;
        let (cx0, cy0, cx1, cy1) = close_box(i);
        if x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
            return Hit::Close(i);
        }
        return Hit::Tab(i);
    }
    Hit::Drag
}
