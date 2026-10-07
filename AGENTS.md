# Lodestone

A native desktop app on [ferrite-design](https://github.com/rwetz/ferrite-design)
(GPUI, Rust), started from the `dashboard` template. It finds
every Ferrite app in a workspace, builds and launches them, and keeps one
shared look (scheme, appearance, refresh rate, density) that each launch
inherits through `FERRITE_*` variables. Discovery, build/launch and the
shared-look file live in `src/registry.rs` (pure parsing is unit-tested);
the views live in `src/main.rs`.

Before changing UI code, read ferrite-design's AGENTS.md — it is the
contract for how Ferrite apps are built (components, colors, type, motion,
and the mistakes that cost time). Short version:

- `use ferrite_design::prelude::*;` — every component comes from there.
- Colors only through `palette(cx)`; no hex values in views.
- Display type via `.display(Scale::X1, window)`, UPPERCASE; body via `.body(text::BASE)`.
- Controlled components: you own the value, the handler gets the new one.
- Check: `cargo build`, `cargo clippy`, and run it.
