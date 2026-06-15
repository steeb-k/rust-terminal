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

- **Always bump the PEBakery scripts, not just `Cargo.toml`.** The download URLs auto-track the latest release so the *binary* will be correct, but the `Version=`/`Date=` metadata shown to users goes stale if you forget.
- **The two PEBakery scripts have diverged** and are not byte-identical:
  - The in-repo `pebakery/RustTerminal.script` pulls `releases/latest/download/<binary>`.
  - The winrx-creator copy queries the GitHub API for the latest tag, then downloads `releases/download/<tag>/<binary>`.
  Keep their `Version=`/`Date=` in sync; don't try to merge their download logic.
- The winrx-creator script lives in a **different repository**, so its bump is a separate commit there.
- `build.yml` builds sequentially (`max-parallel: 1`) so matrix jobs don't race to create the same release. ARM64 is stubbed out (TODO) — only `x86_64-pc-windows-msvc` ships today.
- Versioning is informal semver: patch for fixes, minor for features (e.g. the 1px window border shipped as `v0.2.5`).
