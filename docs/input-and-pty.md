# Input & PTY

This covers how keystrokes reach the shell and how shell output gets back, plus
selection and clipboard.

## ConPTY session (`conpty.rs`)

Each tab owns a `Pty`:

- `Pty::spawn(id, shell, cols, rows, hwnd)` creates a pseudoconsole (`CreatePseudoConsole`), an input pipe, and the child process (`CreateProcessW` with a `STARTUPINFOEXW` carrying the `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`).
- The child starts in the user's home profile (`USERPROFILE`), not wherever the app was launched from. `conpty::home_dir` returns `None` when that variable is missing or isn't a real directory, which means "inherit" — a bogus path would make `CreateProcessW` fail and lose the tab outright.
- A **background reader thread** loops on `ReadFile` over the output pipe, appends bytes to a mutex-guarded buffer, and posts `WM_PTY_DATA` to `hwnd` to wake the UI thread. On EOF/child exit it posts `WM_PTY_EXIT` with the session `id`.
- `pty.write(bytes)` writes to the input pipe; `pty.resize(cols, rows)` calls `ResizePseudoConsole`.
- `take_pending()` swaps out the accumulated bytes for the UI thread to feed to the parser.
- Drop tears the session down (close handles, `ClosePseudoConsole`).

`WM_PTY_DATA` and `WM_PTY_EXIT` are custom `WM_APP`-range messages defined in
`conpty.rs` and handled in `main.rs`.

## Keyboard encoding (`input.rs`)

Two Win32 messages feed the shell:

- **`WM_CHAR`** delivers already-translated text plus Enter/Tab/Backspace and `Ctrl+letter` control codes. Letting the shell receive these directly is what makes its own Tab completion and line editing work. `input::char_bytes` encodes the UTF-16 unit to UTF-8 bytes.
- **`WM_KEYDOWN`** handles keys `WM_CHAR` doesn't produce — arrows, Home/End, PageUp/Down, function keys, Backspace/Delete — encoded by `input::key_bytes(vk, app_cursor)`. The `app_cursor` flag switches arrow keys between normal (`CSI A`) and application (`SS3 A`) mode per the terminal's DECCKM state.

### App shortcuts vs. shell passthrough

`wndproc` intercepts a small set before they reach the shell:

| Keys | Action |
|------|--------|
| `Ctrl+T` / `Ctrl+W` | New / close tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Switch tab |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy / paste |

These are swallowed in both `WM_KEYDOWN` (to act) and `WM_CHAR` (so the control
char never reaches the shell). **Plain `Ctrl+C` (0x03) passes through** so it can
interrupt the running program.

## Selection & clipboard

- Mouse selection: `WM_LBUTTONDOWN` in the terminal region begins a selection (`grid.sel_begin`), `WM_MOUSEMOVE` while dragging extends it (`grid.sel_update`), `WM_LBUTTONUP` ends it. Coordinates are mapped through the chrome/padding offset and current scroll position by `mouse_cell`.
- The focusing click on an inactive window is **eaten** for the terminal area (`WM_MOUSEACTIVATE → MA_ACTIVATEANDEAT`) so it doesn't start a stray selection; chrome clicks still work (`MA_ACTIVATE`).
- Copy/paste go through `clipboard.rs` (`CF_UNICODETEXT`). Paste normalizes `\r\n`/`\n` to `\r` and honors **bracketed paste** (`ESC [200~ … ESC [201~`) when the program enabled it.
- Mouse wheel scrolls the scrollback view (`grid.scroll_view`); typing snaps back to the bottom.

## Resize

`WM_SIZE` recomputes `(cols, rows)` from the client area minus chrome and
padding, then for every tab resizes both the `grid` and the `pty` so the shell
and our buffer stay in agreement.
