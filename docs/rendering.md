# Rendering

Everything inside the window is drawn by hand with GDI. There is no OS chrome.
`render.rs` owns all of it; `chrome.rs` owns the layout math and hit-testing that
both the renderer and `wndproc` share.

## Borderless window

The window is created `WS_POPUP | WS_THICKFRAME` (resizable, but no caption).
Three message handlers strip the remaining OS frame:

- `WM_NCCALCSIZE` returns `0` → the **client area is the entire window**, so our paint covers every pixel.
- `WM_NCPAINT` returns `0` → suppresses the classic frame border (works even without DWM).
- `WM_NCACTIVATE` returns `DefWindowProcW(..., LPARAM(-1))` → stops the inactive-frame repaint while keeping the window logically active; it also records focus into `App.focused`.

On the desktop, `DwmSetWindowAttribute(DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE)`
removes the Win11 focus edge. Under WinPE there is no DWM, so this is a no-op and
the `WM_NCPAINT` suppression does the job.

## Double buffering

`render_to(target, w, h, …)` renders into an offscreen memory DC + bitmap, then
`BitBlt`s the whole thing to the target in one shot. `WM_ERASEBKGND` returns `1`
so the OS never clears the background — together this eliminates flicker. Paint
order into the memory DC:

1. Fill background (`DEFAULT_BG`).
2. `draw_chrome` — tab strip / titlebar.
3. `draw_cells` — the terminal grid.
4. `draw_cursor`.
5. `draw_menu` — the new-tab dropdown, when open; after the grid because it overlays it.
6. `draw_border` — the 1px window outline, **last**, so it sits on top.

`WM_PAINT` calls `render::paint` (which `BeginPaint`s and forwards to `render_to`);
`WM_PRINTCLIENT` forwards directly so thumbnails/snapshots render correctly.

## Chrome (`chrome.rs` + `draw_chrome`)

`chrome.rs` defines the layout constants (`CHROME_H`, `TAB_W`, `BTN_W`,
`RADIUS`, padding, etc.) and two pure functions:

- `hit(ntabs, width, x, y) -> Hit` — classifies a client point (tab, close box, new-tab `+`, caption button, drag region, or terminal).
- `menu_rect` / `menu_hit` — geometry and item hit-testing for the new-tab dropdown.
- `caption_xs`, `tab_x`, `close_box`, … — geometry helpers.

`wndproc` reuses `hit` in `WM_NCHITTEST` (mapping `Hit::Drag → HTCAPTION`,
caption buttons → `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE`) and in mouse handlers,
so drawing and input always agree on where things are. Caption/close glyphs come
from the **Segoe MDL2 Assets** font; tab labels use **Segoe UI**.

## New-tab dropdown

Clicking `+` opens a list of the shells this image actually has (`main::available_shells`
probes for them at startup). It is **not** a popup window or a `TrackPopupMenu` —
it's painted into the same window as an overlay over the terminal area, so it
inherits the dark chrome instead of the OS's light menu styling, and needs
nothing from a compositor.

Because it lives inside the window, dismissal is explicit: `close_menu` runs on a
click anywhere off an item, any keystroke (Escape does *only* that), `WM_SIZE`
(the geometry hangs off the chrome, which moves), and focus loss via
`WM_NCACTIVATE`. While it's open, `WM_LBUTTONDOWN` swallows the click before the
normal `chrome::hit` dispatch — otherwise a click meant to dismiss the menu would
also start a selection in the terminal underneath.

With only one shell available there's nothing to choose, so `+` opens a tab directly.

## Rounded corners and the border

Corners are clipped by a window region set in `WM_SIZE`:
`CreateRoundRectRgn(0, 0, w+1, h+1, RADIUS, RADIUS)` when normal, no region (square)
when maximized.

`draw_border` strokes a 1px outline with `FrameRgn` over a region built the same
way (`radius = 0` when maximized), so the border **follows the rounded corners**
instead of leaving gaps. Color:

- **Focused** → `chrome.accent` (the shared accent color).
- **Not focused** → `BORDER_INACTIVE` (gray).

Focus comes from `App.focused`, updated in `WM_NCACTIVATE`.

## Colors

`colors.rs` stores everything as GDI **`COLORREF` (`0x00BBGGRR`)** — note the
byte order is the reverse of web RGB. Build values with `colors::rgb(r, g, b)`.
It provides the 16-color Campbell ANSI palette and full xterm-256 resolution
(`xterm256`). Registry accent values are `0xAABBGGRR`; mask `& 0x00FF_FFFF` to
drop the alpha and get a `COLORREF`.

## Cells (`draw_cells`)

Cells are drawn in **runs of identical attributes** (same fg/bg/flags) via
`ExtTextOutW` with `ETO_OPAQUE`, which fills the cell background and draws glyphs
in one call. Bold selects the bold font; reverse swaps fg/bg; selection overrides
to `SEL_FG`/`SEL_BG`; underline is a 1px fill on the bottom row. A per-cell
advance array (`dx`) keeps wide (CJK) layout correct. The cursor is drawn as a
block (`DSTINVERT`), underline, or bar (accent-colored) depending on the
program-selected style.

## Fonts

`Fonts::new(px, face)` picks the first available monospace face from a PE-safe
chain (`Consolas → Cascadia Mono → Lucida Console → Courier New`) and measures
the cell metrics with `GetTextMetricsW`. A small `LINE_GAP` is added per row for
readability; `grid_size` converts a pixel area into `(cols, rows)`.
