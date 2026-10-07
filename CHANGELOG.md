# Changelog

All notable changes to Lodestone are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.1] - 2026-10-07

### Fixed

- Windows: the executable, window and taskbar now show the Lodestone logo
  instead of the generic application icon, and Task Manager lists it as
  "Lodestone".
- README: a downloaded Lodestone needs a folder (`LODESTONE_ROOT` or an
  argument) to find apps, and apps are discovered from source checkouts.

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

[0.1.1]: https://github.com/rwetz/ferrite-lodestone/releases/tag/v0.1.1
[0.1.0]: https://github.com/rwetz/ferrite-lodestone/releases/tag/v0.1.0
