//! Lodestone: one window for every Ferrite app in the workspace.
//!
//! Finds sibling crates that depend on ferrite-design (and ferrite-design's
//! own examples), shows each as a live tile, builds and starts them, and
//! keeps one shared look — scheme, appearance, refresh rate, density — that
//! every app it starts inherits through the `FERRITE_*` variables.
//!
//!     cargo run      # scans the folder this checkout sits in
//!     ./lodestone    # a standalone binary scans the folder it sits in

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod registry;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use ferrite_design::prelude::*;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, KeyBinding, Render, SharedString,
    Subscription, Window, div, px, size,
};

use registry::{Entry, Kind, Shared};

const APPEARANCES: [(&str, &str); 3] = [("dark", "Dark"), ("light", "Light"), ("system", "System")];
const FPS: [u32; 4] = [25, 60, 120, 240];
const DENSITIES: [(&str, &str); 3] = [("compact", "Compact"), ("cozy", "Cozy"), ("roomy", "Roomy")];

/// What happened the last time an entry was launched.
enum Run {
    Building,
    Running { child: Child, since: Instant },
    Exited(Option<i32>),
    /// Stopped from Lodestone (a kill, so its exit code means nothing).
    Stopped,
    Failed(String),
}

struct Lodestone {
    root: PathBuf,
    entries: Vec<Entry>,
    commits: HashMap<SharedString, String>,
    runs: HashMap<SharedString, Run>,
    nav: SharedString,
    selected: Option<SharedString>,
    drawer: bool,
    shared: Shared,
    /// Bumped on every rescan so the tile grid cascades in again.
    scans: u32,
    search: Entity<TextInput>,
    palette: Entity<CommandPalette>,
    toaster: Entity<Toaster>,
    _search: Subscription,
    _appearance: Subscription,
}

impl Lodestone {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new(window, cx).placeholder("Filter apps…").prompt(">"));
        let _search = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());

        // Child processes: notice exits, and tick uptimes while anything runs.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let mut hub = Self {
            root: registry::default_root(),
            entries: Vec::new(),
            commits: HashMap::new(),
            runs: HashMap::new(),
            nav: "apps".into(),
            selected: None,
            drawer: false,
            shared: Shared::load(),
            scans: 0,
            search,
            palette: cx.new(|cx| CommandPalette::new(window, cx)),
            toaster: cx.new(|_| Toaster::new()),
            _search,
            _appearance: theme::follow_system(window),
        };
        hub.apply_shared(window, cx);
        hub.rescan(cx);
        hub
    }

    // ── Workspace ────────────────────────────────────────────────────────

    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.entries = registry::scan(&self.root);
        self.scans += 1;
        // A workspace with no apps yet opens on the gallery, not an empty page.
        if self.scans == 1 && !self.entries.iter().any(|e| e.kind == Kind::App) {
            self.nav = "gallery".into();
        }
        self.set_commands(cx);
        let dirs: Vec<(SharedString, PathBuf)> = self.entries.iter().map(|e| (e.key.clone(), e.dir.clone())).collect();
        cx.spawn(async move |this, cx| {
            let commits = cx
                .background_executor()
                .spawn(async move {
                    let mut by_dir: HashMap<PathBuf, Option<String>> = HashMap::new();
                    dirs.into_iter()
                        .filter_map(|(key, dir)| {
                            let c = by_dir.entry(dir.clone()).or_insert_with(|| registry::last_commit(&dir)).clone();
                            c.map(|c| (key, c))
                        })
                        .collect::<HashMap<_, _>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.commits = commits;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn entry(&self, key: &SharedString) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.key == key)
    }

    fn is_running(&self, key: &SharedString) -> bool {
        matches!(self.runs.get(key), Some(Run::Running { .. }))
    }

    /// The tiles on the current page, after the search filter.
    fn visible(&self, cx: &App) -> Vec<&Entry> {
        let query = self.search.read(cx).value();
        self.entries
            .iter()
            .filter(|e| match self.nav.as_ref() {
                "apps" => e.kind == Kind::App,
                "gallery" => e.kind == Kind::Example,
                "running" => self.is_running(&e.key),
                _ => false,
            })
            .filter(|e| e.matches(&query))
            .collect()
    }

    // ── Launching ────────────────────────────────────────────────────────

    fn launch(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if matches!(self.runs.get(&key), Some(Run::Building | Run::Running { .. })) {
            return;
        }
        let Some(entry) = self.entry(&key).cloned() else { return };
        self.runs.insert(key.clone(), Run::Building);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let build_entry = entry.clone();
            let built = cx.background_executor().spawn(async move { registry::build(&build_entry) }).await;
            let _ = this.update(cx, |this, cx| {
                let run = built.and_then(|bin| registry::start(&bin, &entry.dir, &this.shared));
                let note = match &run {
                    Ok(_) => toast(format!("{} started", entry.name)).success().message(this.shared.scheme.to_uppercase()),
                    Err(err) => toast(format!("{} failed", entry.name)).danger().message(err.clone()),
                };
                this.toaster.update(cx, |t, cx| t.push(note, cx));
                this.runs.insert(key, match run {
                    Ok(child) => Run::Running { child, since: Instant::now() },
                    Err(err) => Run::Failed(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn stop(&mut self, key: &SharedString, cx: &mut Context<Self>) {
        if let Some(Run::Running { child, .. }) = self.runs.get_mut(key) {
            let _ = child.kill();
            let _ = child.wait();
            self.runs.insert(key.clone(), Run::Stopped);
            cx.notify();
        }
    }

    fn stop_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<SharedString> = self.runs.keys().filter(|k| self.is_running(k)).cloned().collect();
        for key in keys {
            self.stop(&key, cx);
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for run in self.runs.values_mut() {
            if let Run::Running { child, .. } = run {
                changed = true; // uptimes tick
                if let Ok(Some(status)) = child.try_wait() {
                    *run = Run::Exited(status.code());
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    // ── The shared look ──────────────────────────────────────────────────

    fn apply_shared(&self, window: &mut Window, cx: &mut App) {
        let s = &self.shared;
        if let Some(scheme) = schemes::by_key(&s.scheme) {
            theme::set_scheme(scheme, cx);
        }
        let appearance = match s.appearance.as_str() {
            "light" => Appearance::Light,
            "system" => Appearance::System,
            _ => Appearance::Dark,
        };
        theme::set_appearance(appearance, window, cx);
        motion::set_fps(s.fps);
        theme::set_density(
            match s.density.as_str() {
                "compact" => Density::Compact,
                "roomy" => Density::Roomy,
                _ => Density::Cozy,
            },
            cx,
        );
    }

    fn change_shared(&mut self, f: impl FnOnce(&mut Shared), window: &mut Window, cx: &mut Context<Self>) {
        f(&mut self.shared);
        self.apply_shared(window, cx);
        if let Err(err) = self.shared.save() {
            self.toaster.update(cx, |t, cx| t.push(toast("Couldn't save the shared look").danger().message(err), cx));
        }
        cx.notify();
    }

    // ── Commands ─────────────────────────────────────────────────────────

    fn set_commands(&self, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        let go = |key: &'static str| {
            let weak = weak.clone();
            move |_: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    this.nav = key.into();
                    cx.notify();
                });
            }
        };
        let mut commands = vec![
            command("Go to apps").group("Navigate").icon(Icon::Home).on_run(go("apps")),
            command("Go to running").group("Navigate").icon(Icon::Play).on_run(go("running")),
            command("Go to gallery").group("Navigate").icon(Icon::Chart).on_run(go("gallery")),
            command("Go to shared look").group("Navigate").icon(Icon::Sliders).on_run(go("look")),
            command("Rescan workspace").group("Lodestone").icon(Icon::Refresh).on_run({
                let weak = weak.clone();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.rescan(cx));
                }
            }),
            command("Stop all apps").group("Lodestone").icon(Icon::Stop).on_run({
                let weak = weak.clone();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.stop_all(cx));
                }
            }),
        ];
        for e in &self.entries {
            let (weak, key) = (weak.clone(), e.key.clone());
            commands.push(command(format!("Launch {}", e.name)).group("Launch").icon(Icon::Play).on_run(move |_, cx| {
                let _ = weak.update(cx, |this, cx| this.launch(key.clone(), cx));
            }));
        }
        for scheme in SCHEMES {
            let weak = weak.clone();
            commands.push(command(format!("Scheme: {}", scheme.name)).group("Shared look").on_run(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.change_shared(|s| s.scheme = scheme.key.into(), window, cx));
            }));
        }
        self.palette.update(cx, |p, cx| p.set_commands(commands, cx));
    }

    // ── Views ────────────────────────────────────────────────────────────

    fn status(&self, e: &Entry, window: &mut Window, cx: &App) -> AnyElement {
        let p = palette(cx);
        match self.runs.get(&e.key) {
            Some(Run::Building) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(spinner(eid("spin", &e.key)))
                .child(div().display(Scale::X1, window).text_color(hsla(p.fg_dim)).child("BUILDING"))
                .into_any_element(),
            Some(Run::Running { .. }) => tag("running").accent().into_any_element(),
            Some(Run::Exited(Some(0))) => tag("exited").outline().into_any_element(),
            Some(Run::Stopped) => tag("stopped").outline().into_any_element(),
            Some(Run::Exited(code)) => tag(format!("exit {}", code.map_or("?".into(), |c| c.to_string()))).warning().into_any_element(),
            Some(Run::Failed(_)) => tag("failed").danger().into_any_element(),
            None => tag("idle").outline().into_any_element(),
        }
    }

    /// The live line under a tile: uptime while running, else the last commit.
    fn readout(&self, e: &Entry) -> String {
        match self.runs.get(&e.key) {
            Some(Run::Running { child, since }) => format!("PID {} · UP {}", child.id(), uptime(since.elapsed())),
            Some(Run::Failed(err)) => err.lines().last().unwrap_or("").to_string(),
            // Examples share one repo, so its last commit says nothing about them.
            _ if e.kind == Kind::Example => format!("cargo run --example {}", e.target),
            _ => self.commits.get(&e.key).cloned().unwrap_or_else(|| "no commits".into()),
        }
    }

    fn tile(&self, e: &Entry, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let key = e.key.clone();
        let selected = self.selected.as_ref() == Some(&key);
        let running = self.is_running(&key);
        let building = matches!(self.runs.get(&key), Some(Run::Building));

        let action = if running {
            Button::new(eid("stop", &key)).label("Stop").icon(Icon::Stop).ghost().small().on_click(cx.listener({
                let key = key.clone();
                move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.stop(&key, cx);
                }
            }))
        } else {
            Button::new(eid("run", &key)).label("Run").icon(Icon::Play).secondary().small().loading(building).on_click(cx.listener({
                let key = key.clone();
                move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.launch(key.clone(), cx);
                }
            }))
        };

        let about = if e.about.is_empty() { SharedString::from("—") } else { e.about.clone() };
        div()
            .id(eid("tile", &key))
            .w(px(300.))
            .p(px(1.))
            .when(selected, |d| d.bg(hsla(p.accent)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = Some(key.clone());
                this.drawer = true;
                cx.notify();
            }))
            .child(
                panel(e.name.clone())
                    .meta(if e.kind == Kind::App { "APP" } else { "EXAMPLE" })
                    .h(px(176.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .items_start()
                            .child(avatar(e.name.to_string()).size(px(32.)).when(running, |a| a.presence(Presence::Online)))
                            .child(div().flex_1().min_w_0().h(px(40.)).overflow_hidden().body(text::SM).text_color(hsla(p.fg_dim)).child(about)),
                    )
                    .child(div().flex_1())
                    .child(div().body(text::SM).text_color(hsla(p.fg_faint)).truncate().child(self.readout(e)))
                    .child(div().flex().flex_row().items_center().child(self.status(e, window, cx)).child(div().flex_1()).child(action)),
            )
    }

    fn library(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let visible: Vec<Entry> = self.visible(cx).into_iter().cloned().collect();
        if visible.is_empty() {
            let (title, hint) = match self.nav.as_ref() {
                "running" => ("Nothing running", "Start an app from the library or the gallery."),
                "apps" if self.search.read(cx).value().is_empty() => ("No Ferrite apps found", "Scaffold one with scripts/new-app.sh, then rescan."),
                _ => ("No matches", "Try a shorter filter."),
            };
            return div().flex_1().flex().items_center().justify_center().child(empty_state(title, hint, window, cx)).into_any_element();
        }
        let tiles: Vec<AnyElement> = visible.iter().map(|e| self.tile(e, window, cx).into_any_element()).collect();
        scroll_area("library")
            .flex_1()
            .min_h_0()
            .child(cascade_in(eid("grid", &self.nav), self.scans).flex().flex_row().flex_wrap().gap(space::ROW).p(space::ROW).children(tiles))
            .into_any_element()
    }

    fn look(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let s = &self.shared;
        let schemes = panel("Scheme").meta(theme::scheme(cx).name.to_uppercase()).w(px(380.)).children(SCHEMES.iter().map(|scheme| {
            list_item(eid("scheme", scheme.key), scheme.name)
                .meta(match scheme.kind {
                    SchemeKind::Signature => "signature",
                    SchemeKind::Neutral => "neutral",
                    SchemeKind::Wild => "wild",
                })
                .selected(s.scheme == scheme.key)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.change_shared(|s| s.scheme = scheme.key.into(), window, cx)))
        }));

        let about = schemes::by_key(&s.scheme).map(|sc| sc.about).unwrap_or("");
        let look = panel("Look")
            .child(div().body(text::SM).text_color(hsla(p.fg_dim)).child(about))
            .child(
                field("appearance", "Appearance").child(
                    APPEARANCES.iter().fold(segmented("appearance-seg"), |seg, (_, label)| seg.option(*label))
                        .selected(APPEARANCES.iter().position(|(k, _)| *k == s.appearance).unwrap_or(0))
                        .on_select(cx.listener(|this, i: &usize, window, cx| this.change_shared(|s| s.appearance = APPEARANCES[*i].0.into(), window, cx))),
                ),
            )
            .child(
                field("fps", "Refresh rate").hint("25 is the classic stepped look").child(
                    FPS.iter().fold(segmented("fps-seg"), |seg, f| seg.option(format!("{f}")))
                        .selected(FPS.iter().position(|f| *f == s.fps).unwrap_or(FPS.len() - 1))
                        .on_select(cx.listener(|this, i: &usize, window, cx| this.change_shared(|s| s.fps = FPS[*i], window, cx))),
                ),
            )
            .child(
                field("density", "Density").child(
                    DENSITIES.iter().fold(segmented("density-seg"), |seg, (_, label)| seg.option(*label))
                        .selected(DENSITIES.iter().position(|(k, _)| *k == s.density).unwrap_or(1))
                        .on_select(cx.listener(|this, i: &usize, window, cx| this.change_shared(|s| s.density = DENSITIES[*i].0.into(), window, cx))),
                ),
            );

        let path = Shared::path().map(|p| p.display().to_string()).unwrap_or_else(|| "unsaved".into());
        let handoff = panel("Handed to every launch").child(
            s.env().into_iter().fold(property_list().key_width(px(180.)), |l, (k, v)| l.row(k, v)).row("saved to", path),
        );

        scroll_area("look")
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(space::ROW)
                    .p(space::ROW)
                    .child(schemes)
                    .child(div().flex().flex_col().flex_1().min_w_0().gap(space::ROW).child(look).child(handoff)),
            )
            .into_any_element()
    }

    fn details(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entry = self.selected.as_ref().and_then(|k| self.entry(k)).cloned();
        let close = {
            let weak = cx.weak_entity();
            move |_: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    this.drawer = false;
                    cx.notify();
                });
            }
        };
        let mut d = drawer("details").open(self.drawer && entry.is_some()).width(px(400.)).on_close(close);
        let Some(e) = entry else { return d };
        let running = self.is_running(&e.key);
        let built = registry::binary(&e).exists();
        let key = e.key.clone();

        let mut props = property_list()
            .row("kind", if e.kind == Kind::App { "app" } else { "ferrite-design example" })
            .row("folder", e.dir.display().to_string())
            .row("target", e.target.clone())
            .row("built", if built { "yes" } else { "not yet — first run builds it" })
            .when(e.kind == Kind::App, |l| l.row("last commit", self.commits.get(&e.key).cloned().unwrap_or_else(|| "—".into())))
            .row_with("state", self.status(&e, window, cx));
        if let Some(Run::Running { child, since }) = self.runs.get(&e.key) {
            props = props.row("pid", child.id().to_string()).row("uptime", uptime(since.elapsed()));
        }

        let primary = if running {
            Button::new("drawer-stop").label("Stop").icon(Icon::Stop).danger().on_click(cx.listener({
                let key = key.clone();
                move |this, _: &ClickEvent, _, cx| this.stop(&key, cx)
            }))
        } else {
            Button::new("drawer-launch")
                .label("Launch")
                .icon(Icon::Play)
                .primary()
                .loading(matches!(self.runs.get(&key), Some(Run::Building)))
                .on_click(cx.listener({
                    let key = key.clone();
                    move |this, _: &ClickEvent, _, cx| this.launch(key.clone(), cx)
                }))
        };
        let dir = e.dir.clone();
        d = d
            .title(e.name.clone())
            .child(div().body(text::BASE).child(if e.about.is_empty() { SharedString::from("No description in Cargo.toml.") } else { e.about.clone() }))
            .child(props)
            .when_some(
                match self.runs.get(&e.key) {
                    Some(Run::Failed(err)) => Some(err.clone()),
                    _ => None,
                },
                |d, err| d.child(alert("build-failed", "Build failed").danger().message(err)),
            )
            .footer(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(primary)
                    .child(Button::new("reveal").label("Open folder").icon(Icon::Folder).secondary().on_click(move |_, _, _| registry::reveal(&dir))),
            );
        d
    }
}

fn uptime(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}S"),
        60..3600 => format!("{}M {:02}S", s / 60, s % 60),
        _ => format!("{}H {:02}M", s / 3600, (s % 3600) / 60),
    }
}

impl Render for Lodestone {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let apps = self.entries.iter().filter(|e| e.kind == Kind::App).count();
        let examples = self.entries.len() - apps;
        let running = self.runs.values().filter(|r| matches!(r, Run::Running { .. })).count();

        let nav = sidebar("nav")
            .section("Library")
            .item_with_meta("apps", "Apps", Icon::Home, apps.to_string())
            .item_with_meta("running", "Running", Icon::Play, running.to_string())
            .item_with_meta("gallery", "Gallery", Icon::Chart, examples.to_string())
            .section("Ecosystem")
            .item("look", "Shared look", Icon::Sliders)
            .selected(self.nav.clone())
            .footer(div().body(text::SM).text_color(hsla(p.fg_faint)).truncate().child(self.root.display().to_string()))
            .on_select(cx.listener(|this, key: &SharedString, _, cx| {
                this.nav = key.clone();
                cx.notify();
            }));

        let bar = toolbar()
            .child(div().w(px(260.)).child(self.search.clone()))
            .spacer()
            .child(Button::new("rescan").icon(Icon::Refresh).small().ghost().tooltip("Rescan workspace").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.rescan(cx))))
            .separator()
            .child(Button::new("cmd").icon(Icon::Search).small().tooltip("Commands · Ctrl+Shift+P").on_click({
                let palette = self.palette.clone();
                move |_, window, cx| palette.update(cx, |p, cx| p.open(window, cx))
            }));

        let page = if self.nav.as_ref() == "look" { self.look(window, cx) } else { self.library(window, cx) };
        let details = self.details(window, cx);

        let root = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(hsla(p.bg))
            .text_color(hsla(p.fg))
            .body(text::BASE)
            .on_action(cx.listener(|this, _: &TogglePalette, window, cx| this.palette.update(cx, |p, cx| p.toggle(window, cx))))
            .child(self.palette.clone())
            .child(self.toaster.clone())
            .child(title_bar("Lodestone"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(nav)
                    .child(div().flex().flex_col().flex_1().min_w_0().when(self.nav.as_ref() != "look", |d| d.child(bar)).child(page)),
            )
            .child(details)
            .child(
                status_bar()
                    .left(format!("{apps} APPS · {examples} EXAMPLES"))
                    .left_live(format!("{running} RUNNING"))
                    .right(theme::scheme(cx).name.to_uppercase())
                    .right_live(format!("{}FPS", motion::fps())),
            );

        let boot = boot_screen("boot", root)
            .title("Lodestone")
            .line("WORKSPACE", "OK")
            .line("APPS", format!("{apps} FOUND"))
            .line("GALLERY", format!("{examples} FOUND"))
            .line("SCHEME", self.shared.scheme.to_uppercase())
            .line("SHARED LOOK", if Shared::path().is_some_and(|p| p.exists()) { "LOADED" } else { "DEFAULT" });

        window_frame().child(power_on_in("power", boot))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        ferrite_design::init(Appearance::Dark, cx);
        cx.bind_keys([KeyBinding::new("ctrl-shift-p", TogglePalette, None)]);
        let options = chrome::window_options("Lodestone", size(px(1280.), px(820.)), cx);
        cx.open_window(options, |window, cx| {
            chrome::square_corners(window);
            chrome::power_off_on_close(window, cx);
            cx.new(|cx| Lodestone::new(window, cx))
        })
        .expect("failed to open the window");
        cx.activate(true);
    });
}

/// A per-entry element id: `tile:ferrite-pulse`.
fn eid(prefix: &str, key: &str) -> gpui::ElementId {
    gpui::ElementId::Name(format!("{prefix}:{key}").into())
}
