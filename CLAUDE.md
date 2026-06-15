# CLAUDE.md

Guidance for Claude Code (and humans) working in this repository.

## What this is

`rust-terminal` is a GDI-rendered terminal emulator for **Windows PE**. It is a
companion to the sibling `StartPE` app and ships inside winrx-creator / PhoenixPE
images. The defining constraint: it must run on bare WinPE, which has **no DWM,
no Direct3D, and no XAML**. Everything is drawn with plain GDI into a single
self-owned window; there is no OS-drawn chrome.

## Build / run

```sh
cargo build --bin rust-terminal              # debug
cargo build --release --bin rust-terminal    # release (size-tuned: opt-level=z, LTO, strip)
cargo run --bin rust-terminal                # run locally (works on normal Windows too)
```

- Targets Windows only (`#![windows_subsystem = "windows"]` — no console window).
- `src/bin/conpty_spike.rs` is a throwaway feasibility spike (`--bin conpty-spike`); ignore it for product work.
- `build.rs` embeds `assets/rust-terminal.ico` as resource id 101 (best-effort; build still succeeds without a resource compiler).

## Architecture

Single-threaded Win32 message loop in `main.rs` drives everything. Per-tab ConPTY
sessions run background reader threads that post `WM_PTY_DATA` to the UI thread;
the UI thread owns all terminal/render state in a `thread_local! STATE: App`.

Data flow per tab: ConPTY output bytes → `conpty` reader thread buffer → `WM_PTY_DATA`
→ `parser` (vte) mutates `grid` → `render` paints. Keyboard: `WM_CHAR`/`WM_KEYDOWN`
→ `input` encodes → `pty.write`.

### Module map

| File | Responsibility |
|------|----------------|
| `main.rs` | Window class, message loop, `wndproc`, tabs/sessions, focus, hit-testing wiring |
| `chrome.rs` | Layout + hit-testing constants for the self-drawn tab strip / titlebar (shared by render + wndproc) |
| `render.rs` | GDI renderer: double-buffered chrome + terminal grid + cursor + window border |
| `parser.rs` | VT/ANSI parsing via `vte`; mutates `grid`; queues replies in `responses` |
| `grid.rs` | Screen buffer: primary/alt screens, scrollback ring, scroll region, selection |
| `conpty.rs` | ConPTY session wrapper: pseudoconsole, input pipe, child process, reader thread |
| `input.rs` | Keyboard → PTY byte encoding |
| `clipboard.rs` | Win32 Unicode clipboard get/set |
| `colors.rs` | ANSI/xterm-256 palette as GDI `COLORREF` |
| `config.rs` | Registry-backed config + shared StartPE accent resolution |

## Conventions & gotchas

- **Colors are GDI `COLORREF` (`0x00BBGGRR`)**, not web RGB. Use `colors::rgb(r,g,b)`. The Windows accent / StartPE registry values are `0xAABBGGRR`; mask `& 0x00FF_FFFF` to convert.
- **Borderless window**: `WM_NCCALCSIZE` returns 0 so the client area is the whole window; `WM_NCPAINT`/`WM_NCACTIVATE` suppress any classic frame. The whole window — tab strip, terminal, 1px border — is painted in `render::render_to`. Don't reach for OS title bars or buttons.
- **Rounded corners** via `SetWindowRgn` (`chrome::RADIUS`, square when maximized). The 1px window border is stroked with `FrameRgn` over a matching region so it follows the corners.
- **Accent color** (`config::Config.accent`) drives the active-tab bar, cursor, and focused window border. Resolution order: StartPE `HKLM/HKCU\Software\StartPE\StartButtonColor` → standard Windows accent (`HKCU\Software\Microsoft\Windows\DWM\AccentColor`) → purple default (`ACCENT_DEFAULT`).
- **Config is read once** at `WM_CREATE` from the registry: HKLM first, then HKCU overlays (PE runs the shell as SYSTEM, so machine-wide writes win there).
- All rendering is double-buffered into a memory DC, then `BitBlt`'d; `WM_ERASEBKGND` returns 1 to avoid flicker.

## Release process

Releases are cut by pushing a `vX.Y.Z` tag; `.github/workflows/build.yml` builds
the release binary and publishes the GitHub release with `rust-terminal.exe`
attached (`releases/latest/download/` is what the PEBakery scripts pull).

When bumping the version, update **all** of these together:

1. `Cargo.toml` `version`
2. `pebakery/RustTerminal.script` — `Version=X.Y.Z.0` and `Date=`
3. `D:\winrx-creator\Projects\winrx-creator\Applications\System Tools\RustTerminal.script` (separate repo) — same fields

Both PEBakery scripts follow the shared winrx-creator/PhoenixPE convention
(modeled on `Diskoria.script`) and are kept **byte-identical**: License header,
`[Main]`/`[Variables]`, RunFromRam + AlwaysDownload + Shortcuts, standard icon
buttons, a `DownloadProgram` that resolves the latest tag via the GitHub API
(falling back to `%ProgramVersion%`), plus a RustTerminal-specific `[Config]`
that writes the shell choice to the registry. When you change one, copy it to the
other so they stay in sync (`Version=`, `Date=`, and the `%ProgramVersion%`
fallback tag all move together).

Then: commit → `git tag vX.Y.Z` → push `main` and the tag. Full checklist in
[`docs/release.md`](docs/release.md).

## Deeper docs

- [`docs/architecture.md`](docs/architecture.md) — threading, message loop, startup.
- [`docs/rendering.md`](docs/rendering.md) — borderless window, double buffering, chrome, border, colors.
- [`docs/input-and-pty.md`](docs/input-and-pty.md) — ConPTY, keyboard encoding, selection/clipboard.
- [`docs/release.md`](docs/release.md) — release checklist.
