# rust-terminal

A free, open-source (GPLv3) **GDI-rendered terminal emulator for Windows PE**.
ConPTY-backed, with no dependency on DWM, Direct3D, or XAML — so it runs on bare
WinPE where modern terminals can't. A companion to [StartPE](#), it matches the
shared StartPE accent color automatically.

![version](https://img.shields.io/github/v/release/steeb-k/rust-terminal)

## Features

- **Runs on bare WinPE** — pure GDI rendering, no DWM/D3D/XAML required.
- **Real ConPTY sessions** — full PowerShell / Command Prompt support, including Tab completion and ANSI/VT sequences (parsed with Alacritty's `vte`).
- **Tabs** — multiple shells in one window; `Ctrl+T` new, `Ctrl+W` close, `Ctrl+Tab` / `Ctrl+Shift+Tab` to switch.
- **Self-drawn dark chrome** — borderless window with a custom tab strip, drag, resize, minimize/maximize/close, and rounded corners.
- **Accent-aware 1px border** — tinted with the StartPE / Windows accent color when focused, gray when not.
- **Selection & clipboard** — mouse selection, `Ctrl+Shift+C` copy, `Ctrl+Shift+V` paste (with bracketed-paste support).
- **Scrollback** — configurable ring buffer, mouse-wheel scrolling.
- **Tiny binary** — size-tuned release profile to sit alongside other PE tools.

## Keyboard shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+T` | New tab |
| `Ctrl+W` | Close tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+Shift+C` | Copy selection |
| `Ctrl+Shift+V` | Paste |
| Mouse wheel | Scroll back through history |

`Ctrl+C` and other control codes pass through to the shell as normal.

## Build

Requires a Rust toolchain with the MSVC Windows target.

```sh
cargo build --release --bin rust-terminal
```

The optimized binary lands at `target/release/rust-terminal.exe`.

## Install (Windows PE)

Use the PEBakery script in [`pebakery/RustTerminal.script`](pebakery/RustTerminal.script)
with PhoenixPE / winrx-creator. It queries GitHub for the latest release,
downloads `rust-terminal.exe`, caches it under Programs Cache for future builds,
and writes the shell choice to the registry. The accent color is **not** set
here — it's read from `HKLM\Software\StartPE\StartButtonColor` so it matches
StartPE.

## Configuration

Settings live in the registry under `Software\RustTerminal` (HKLM is read first,
then HKCU overlays it):

| Value | Type | Default | Meaning |
|-------|------|---------|---------|
| `Shell` | `REG_SZ` | `powershell.exe` | Shell command line to launch |
| `FontFace` | `REG_SZ` | `Consolas` | Monospace font (falls back if absent) |
| `FontSize` | `REG_DWORD` | `16` | Font cell height in pixels (8–48) |
| `Scrollback` | `REG_DWORD` | `5000` | Scrollback capacity in lines (0–100000) |

The accent color is shared with StartPE via `Software\StartPE\StartButtonColor`
(`REG_DWORD`, COLORREF `0x00BBGGRR`). If unset, rust-terminal falls back to the
standard Windows accent color, then to its purple default.

## Development

- [`CLAUDE.md`](CLAUDE.md) — orientation, build/run, conventions, release process.
- [`docs/architecture.md`](docs/architecture.md) — threading model, message loop, startup.
- [`docs/rendering.md`](docs/rendering.md) — borderless window, GDI double buffering, chrome, border, colors.
- [`docs/input-and-pty.md`](docs/input-and-pty.md) — ConPTY sessions, keyboard encoding, selection/clipboard.
- [`docs/release.md`](docs/release.md) — versioning and the release checklist.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).
