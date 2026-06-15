# Release process

Releases are automated by GitHub Actions: pushing a `vX.Y.Z` tag triggers
`.github/workflows/build.yml`, which builds the release binary on
`windows-latest` and publishes a GitHub release with `rust-terminal.exe`
attached. The PEBakery scripts download from `releases/latest/download/`, so the
release **is** the distribution channel.

## Checklist

1. **Bump the version in all three places** (they drift apart otherwise):
   - `Cargo.toml` → `version = "X.Y.Z"`
   - `pebakery/RustTerminal.script` → `Version=X.Y.Z.0` and `Date=YYYY-MM-DD`
   - `D:\winrx-creator\Projects\winrx-creator\Applications\System Tools\RustTerminal.script`
     (a **separate git repo**) → same `Version=`/`Date=` fields
2. Build to refresh `Cargo.lock` and sanity-check:
   ```sh
   cargo build --release --bin rust-terminal
   ```
3. Commit, tag, and push:
   ```sh
   git commit -am "vX.Y.Z: <summary>"
   git tag vX.Y.Z
   git push origin main
   git push origin vX.Y.Z
   ```
4. Watch the workflow and confirm the release published:
   ```sh
   gh run watch <run-id> --exit-status
   gh release view vX.Y.Z
   ```

## Notes

- **Always bump the PEBakery scripts, not just `Cargo.toml`.** The download logic auto-tracks the latest release so the *binary* will be correct, but the `Version=`/`Date=` metadata and the `%ProgramVersion%` fallback tag go stale if you forget.
- **The two PEBakery scripts are kept byte-identical**, both following the shared winrx-creator/PhoenixPE convention (modeled on `Diskoria.script`): they resolve the latest tag via the GitHub API and fall back to `%ProgramVersion%`. When you bump one, copy it to the other:
  - in-repo: `pebakery/RustTerminal.script`
  - winrx-creator: `…\System Tools\RustTerminal.script` (a **different repository** — its bump is a separate commit there)
  Bump `Version=`, `Date=`, **and** `%ProgramVersion%="vX.Y.Z"` (the fallback download tag) in both.
- `build.yml` builds sequentially (`max-parallel: 1`) so matrix jobs don't race to create the same release. ARM64 is stubbed out (TODO) — only `x86_64-pc-windows-msvc` ships today.
- Versioning is informal semver: patch for fixes, minor for features (e.g. the 1px window border shipped as `v0.2.5`).
