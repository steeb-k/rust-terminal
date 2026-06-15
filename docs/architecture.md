# Architecture

`rust-terminal` is a single-window, single-UI-thread Win32 application. All
terminal and rendering state lives on the UI thread; the only other threads are
one ConPTY reader per tab, which never touch UI state directly — they buffer
bytes and post a message.

## The defining constraint

The target is **bare Windows PE**: no DWM, no Direct3D, no XAML, often no
PowerShell. Consequences that shape the whole design:

- Rendering is **pure GDI** into a single window we draw entirely ourselves.
- There is **no OS-drawn chrome** — title bar, buttons, borders, and tabs are all hand-drawn (see [rendering.md](rendering.md)).
- First-launch shell selection falls back to `cmd.exe` when PowerShell is absent.
- Config and the shared accent color come from the **registry**, written by the PEBakery install step.

## Threading model

```
                         UI thread (message loop)
                        ┌──────────────────────────┐
  ConPTY reader (tab 0) │  wndproc                  │
   ─── bytes ──► buffer ─┼─ WM_PTY_DATA ─► parser ─► grid ─► render (WM_PAINT)
  ConPTY reader (tab 1) │                           │
   ─── bytes ──► buffer ─┘  WM_CHAR/WM_KEYDOWN ─► input ─► pty.write
                        └──────────────────────────┘
```

- Each `conpty::Pty` owns a background thread reading the pseudoconsole output pipe into a shared `Vec<u8>` behind a mutex, then posts `WM_PTY_DATA` to wake the UI thread.
- The UI thread drains every tab's pending bytes in the `WM_PTY_DATA` handler, feeds them to that tab's parser, and writes back any device-query replies (`responses`).
- When a shell exits, the reader posts `WM_PTY_EXIT` with the session id; the UI thread closes that tab (and the window if it was the last).

All shared application state is a single `thread_local! STATE: RefCell<Option<App>>`
in `main.rs`. Because everything that touches `App` runs on the UI thread, the
`RefCell` is sufficient — no locking.

## Per-tab session

```rust
struct Session { pty: Pty, term: Term, label: String }
```

- `pty` — the ConPTY wrapper (see [input-and-pty.md](input-and-pty.md)).
- `term` — the VT parser (`parser::Term`) wrapping the screen buffer (`grid::Grid`).
- `label` — default tab name derived from the shell, used until the program sets a usable title.

`App` holds the `Vec<Session>`, the active index, the resolved `accent`, focus
state, hover/selection UI flags, the shared `Fonts`, and the loaded `Config`.

## Message-loop responsibilities (`wndproc`)

| Concern | Messages |
|---------|----------|
| Borderless frame | `WM_NCCALCSIZE` (client = whole window), `WM_NCPAINT`, `WM_NCACTIVATE` |
| Focus tracking | `WM_NCACTIVATE` sets `App.focused` (drives border color) |
| Hit-testing / drag / resize | `WM_NCHITTEST` (border edges + `chrome::hit`), `WM_GETMINMAXINFO` |
| Painting | `WM_PAINT`, `WM_PRINTCLIENT`, `WM_ERASEBKGND` |
| Sizing | `WM_SIZE` resizes every tab's grid + PTY, updates the rounded region |
| Input | `WM_CHAR`, `WM_KEYDOWN` (app shortcuts vs. shell passthrough) |
| Mouse / selection | `WM_LBUTTONDOWN/UP`, `WM_MOUSEMOVE`, `WM_MOUSEWHEEL`, leave/hover |
| PTY | `WM_PTY_DATA`, `WM_PTY_EXIT` (custom messages from `conpty`) |
| Lifecycle | `WM_CREATE`, `WM_DESTROY` |

## Startup sequence

1. `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`.
2. Register the window class, create the `WS_POPUP | WS_THICKFRAME | …` window (borderless but still resizable/min/max-capable).
3. `DwmSetWindowAttribute(DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE)` — removes the Win11 focus-tinted edge on the desktop; harmless no-op under WinPE.
4. `WM_CREATE`: load `Config`, build `Fonts`, spawn the initial tab(s) (PowerShell + cmd if available, else cmd), install `App` into `STATE`.
5. Standard `GetMessage`/`Translate`/`Dispatch` loop until `WM_QUIT`.

## Module map

See the table in [CLAUDE.md](../CLAUDE.md#module-map). In short:
`main.rs` orchestrates; `chrome.rs` + `render.rs` handle the UI;
`parser.rs` + `grid.rs` are the terminal core; `conpty.rs` + `input.rs` bridge to
the shell; `config.rs`, `colors.rs`, `clipboard.rs` are support.
