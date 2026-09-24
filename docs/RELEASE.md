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

## Cross-compiling from Linux (verification only)

```sh
rustup target add x86_64-pc-windows-gnu && sudo apt install mingw-w64 nsis
npx tauri build --target x86_64-pc-windows-gnu --bundles nsis
```

This proves the Windows build and the installer packaging. Release builds should come from the
Windows (MSVC) CI job.
