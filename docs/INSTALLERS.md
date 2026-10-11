# Ferrite installation

Windows releases ship a `*-setup.exe` alongside the portable archive and
`SHA256SUMS.txt`. Setup installs for the current user under
`%LOCALAPPDATA%\Programs\Ferrite`, creates a Start menu shortcut, and registers
with Windows Installed apps. No administrator access is required.

Lodestone manages its own downloaded copies under `%LOCALAPPDATA%\ferrite\apps`.
These are separate from standalone setup installations. Install, update,
update all, launch and stop use the existing release workflow. Uninstall is
available in the app details drawer after closing the app and confirming.
It removes downloaded binaries and keeps application data and shared settings.
All downloads require the release's checksum file.

The setup uninstaller removes packaged files and shortcuts, keeping settings
and all apps downloaded through Lodestone. Close the app before replacing or
uninstalling its executable.

## Build setup without rebuilding Rust

Install NSIS 3.13. Supply the folder already staged for a portable Windows release:

```powershell
python .github/scripts/package_windows.py --source lodestone-v0.3.2-x86_64-pc-windows-msvc --tag v0.3.2
```

For a custom output directory, add `--output-dir dist`. The packager reads the
binary name from Cargo.toml and uses `assets/logo.ico`. All 14 app repositories
carry the same packaging scripts; release workflows package the existing CI
binary, retain portable assets, and checksum both formats.

Setup binaries currently have no Authenticode signature. macOS DMG and Linux
system packages remain follow-up work; existing platform archives are retained.
Lodestone does not manage apps installed separately by setup, or update the
Windows DisplayVersion registry value when its existing self-update replaces
the executable. Rerunning setup refreshes that metadata.

## Local validation, 2026-10-11

- Updated Lodestone: `cargo build --locked -j 2`, strict Clippy on all targets,
  and all 10 tests passed, including bounded uninstall and other-app preservation.
- All 14 app workflows parsed as YAML and their release version tests passed.
- All 14 latest published Windows archives were downloaded and SHA-256 checked;
  setup executables were compiled locally from those release binaries.
- Lodestone v0.3.2 setup passed an isolated silent install/uninstall test:
  executable, Start menu shortcut and Windows uninstall registration were
  created and removed; an added user file survived uninstall.
- Generated setup files live under `target/installer-proof`. They contain the
  previously published app versions. The new GPUI uninstall code is in the
  local development build and will ship after publishing a new release.
- The development process started and stayed alive, but no rendered UI inspection
  or live clicking of the new uninstall dialog has been performed. GitHub CI has
  not run for these changes, and no setup assets have been published.
