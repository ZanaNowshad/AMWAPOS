# Release

1. Bump `version` in `package.json`, `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml`.
2. Tag `vX.Y.Z` and push. `.github/workflows/release.yml` then:
   - runs the Rust tests on Windows;
   - builds the NSIS installer (the WebView2 bootstrapper is embedded, for offline tills);
   - signs it with Authenticode **if** the `WINDOWS_CERT_PFX` (base64) and
     `WINDOWS_CERT_PASSWORD` secrets are set;
   - generates CycloneDX SBOMs (Rust crates and npm production dependencies);
   - writes `SHA256SUMS.txt` and publishes a **draft** GitHub release.
3. Check the draft, install it on a Windows test machine (see OPERATIONS acceptance), then publish.

Unsigned installers trigger SmartScreen warnings. Do not roll them out to stores without an
explicit decision.

## Soak builds

Every push to the working branch runs CI. The **Windows build + tests + installer** job uploads
`amwapos-windows-unsigned`, which contains `AMWAPOS_<ver>_x64-setup.exe`, and prints its SHA-256
in the "Installer hashes" step. The installer used for a soak is recorded in
[STATUS.md](STATUS.md) with the run, the commit and the hash.

A tag such as `v0.1.0-soak.1` runs the release workflow. The result is a **draft** release with
the installer, SBOMs and `SHA256SUMS.txt`, unsigned unless the certificate secrets exist.

## Cross-compiling from Linux (verification only)

```sh
rustup target add x86_64-pc-windows-gnu && sudo apt install mingw-w64 nsis
npx tauri build --target x86_64-pc-windows-gnu --bundles nsis
```

This proves the Windows build and the installer packaging. Release builds should come from the
Windows (MSVC) CI job.
