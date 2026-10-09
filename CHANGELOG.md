# Changelog

All notable changes to Lodestone are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.3.0] - 2026-10-09

### Added

- **Update all** in the toolbar (and the command palette) updates every
  installed app with a newer release. A running app is skipped with a note
  to stop it first.
- Lodestone checks its own releases at start and on every refresh. A banner
  offers **Update Lodestone**, which downloads and verifies the new build,
  swaps it in place of the running binary, and offers **Restart now**.
- The status bar shows Lodestone's version.
- Tiles and the details drawer show each app's own logo (its repo's
  `assets/logo.svg`, cached); an app without one keeps its identicon.

### Fixed

- A tile with an update no longer overflows: **Update** takes the state
  tag's place on the left, and **Run** or **Stop** stays on the right.
  Installing shows a compact tag instead of large text and a second
  spinning button.

## [0.2.0] - 2026-10-07

### Changed

- Lodestone installs apps instead of building them. It lists the GitHub
  repos tagged `ferrite-app`, downloads an app's release archive for this
  platform, checks it against `SHA256SUMS.txt`, and unpacks it under
  `<data>/ferrite/apps`. **Run** starts that binary at once; nothing is
  compiled, and no Rust toolchain is needed.
- A tile shows the version and the release's age (`v0.1.1 · 8 hours ago`)
  instead of the last commit.
- The Gallery page and folder scanning are gone; there is an Installed page.

### Added

- **Install** and **Update**, with a download progress bar.
- An update badge when a newer release is out than the one installed.
- The catalog is cached, so Lodestone opens with its apps offline.
- `GITHUB_TOKEN` is used when set, for a higher API rate limit.

## [0.1.1] - 2026-10-07

### Changed

- Lodestone scans only the folder it lives in: the folder holding the
  checkout when run from source, or the binary's own folder. The
  command-line folder, `LODESTONE_ROOT` and the current-directory fallback
  are gone.

### Fixed

- Windows: the executable, window and taskbar now show the Lodestone logo
  instead of the generic application icon, and Task Manager lists it as
  "Lodestone".

## [0.1.0] - 2026-10-06

The first release.

### Added

- Discovery of Ferrite apps (any crate that depends on `ferrite-design`) one
  level under a workspace folder, plus ferrite-design's own examples on a
  Gallery page.
- Live tiles showing each app's state, with its process ID and uptime while
  it runs and the last commit otherwise.
- One-click launch: a background `cargo build`, then the app's own binary,
  so Stop really stops it. Build errors show in the details drawer.
- A shared look (scheme, appearance, refresh rate, density) saved to
  `ferrite/shared.conf` in the platform config folder and handed to every
  launch as `FERRITE_*` variables.
- A boot screen, a command palette with launch and scheme commands, a search
  filter, a rescan button, and a pixel-art logo.
- The workspace folder comes from the command line, `LODESTONE_ROOT`, the
  checkout's parent folder, or the current directory, in that order.

[0.3.0]: https://github.com/rwetz/ferrite-lodestone/releases/tag/v0.3.0
[0.2.0]: https://github.com/rwetz/ferrite-lodestone/releases/tag/v0.2.0
[0.1.1]: https://github.com/rwetz/ferrite-lodestone/releases/tag/v0.1.1
[0.1.0]: https://github.com/rwetz/ferrite-lodestone/releases/tag/v0.1.0
