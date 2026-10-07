//! Lodestone: one window for every Ferrite app.
//!
//! Lists the GitHub repos tagged `ferrite-app`, installs an app from its
//! latest release (no compiling), shows which are installed and which have
//! updates, starts them, and keeps one shared look — scheme, appearance,
//! refresh rate, density — that every app it starts inherits through the
//! `FERRITE_*` variables.
//!
//!     cargo run

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod registry;

use std::collections::HashMap;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ferrite_design::prelude::*;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, KeyBinding, Render, SharedString,
    Subscription, Window, div, px, size,
};

use registry::{Installed, Shared};

const APPEARANCES: [(&str, &str); 3] = [("dark", "Dark"), ("light", "Light"), ("system", "System")];
const FPS: [u32; 4] = [25, 60, 120, 240];
const DENSITIES: [(&str, &str); 3] = [("compact", "Compact"), ("cozy", "Cozy"), ("roomy", "Roomy")];

/// What an app is doing, or what happened the last time.
enum Run {
    /// Downloading and unpacking; `done` counts the archive's bytes.
    Installing { done: Arc<AtomicU64>, total: u64 },
    Running { child: Child, since: Instant },
    Exited(Option<i32>),
    /// Stopped from Lodestone (a kill, so its exit code means nothing).
    Stopped,
    Failed(String),
}

/// The last check with GitHub.
enum Sync {
    Checking,
    Done(i64),
    Offline(String),
}

struct Lodestone {
    apps: Vec<registry::App>,
    installed: HashMap<SharedString, Installed>,
    runs: HashMap<SharedString, Run>,
    sync: Sync,
    nav: SharedString,
    selected: Option<SharedString>,
    drawer: bool,
    shared: Shared,
    /// Bumped on every sync so the tile grid cascades in again.
    scans: u32,
    ticks: u32,
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

        // Child processes and downloads: notice exits, move progress bars,
        // tick uptimes.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let mut hub = Self {
            // The last catalog opens at once (and offline); a fresh one follows.
            apps: registry::load_catalog(),
            installed: HashMap::new(),
            runs: HashMap::new(),
            sync: Sync::Checking,
            nav: "apps".into(),
            selected: None,
            drawer: false,
            shared: Shared::load(),
            scans: 0,
            ticks: 0,
            search,
            palette: cx.new(|cx| CommandPalette::new(window, cx)),
            toaster: cx.new(|_| Toaster::new()),
            _search,
            _appearance: theme::follow_system(window),
        };
        hub.apply_shared(window, cx);
        hub.refresh_installed();
        hub.set_commands(cx);
        hub.sync(cx);
        hub
    }

    // ── Catalog ──────────────────────────────────────────────────────────

    fn sync(&mut self, cx: &mut Context<Self>) {
        if matches!(self.sync, Sync::Checking) && self.scans > 0 {
            return;
        }
        self.sync = Sync::Checking;
        self.scans += 1;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched = cx.background_executor().spawn(async move { registry::fetch_catalog() }).await;
            let _ = this.update(cx, |this, cx| {
                match fetched {
                    Ok(apps) => {
                        registry::save_catalog(&apps);
                        this.apps = apps;
                        this.sync = Sync::Done(registry::now());
                        this.scans += 1;
                    }
                    Err(err) => {
                        let note = toast("Couldn't check GitHub").warning().message(format!("{err}. Showing the last list."));
                        this.toaster.update(cx, |t, cx| t.push(note, cx));
                        this.sync = Sync::Offline(err);
                    }
                }
                this.refresh_installed();
                this.set_commands(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_installed(&mut self) {
        self.installed = self.apps.iter().filter_map(|a| Some((a.key.clone(), registry::installed(&a.key)?))).collect();
    }

    fn app(&self, key: &SharedString) -> Option<&registry::App> {
        self.apps.iter().find(|a| &a.key == key)
    }

    fn is_running(&self, key: &SharedString) -> bool {
        matches!(self.runs.get(key), Some(Run::Running { .. }))
    }

    fn is_busy(&self, key: &SharedString) -> bool {
        matches!(self.runs.get(key), Some(Run::Installing { .. } | Run::Running { .. }))
    }

    /// The newer release, when one is out past the installed version.
    fn update_for(&self, key: &SharedString) -> Option<&registry::Release> {
        let installed = self.installed.get(key)?;
        self.app(key)?.release.as_ref().filter(|r| registry::is_newer(&r.tag, &installed.tag))
    }

    /// The tiles on the current page, after the search filter.
    fn visible(&self, cx: &App) -> Vec<&registry::App> {
        let query = self.search.read(cx).value();
        self.apps
            .iter()
            .filter(|a| match self.nav.as_ref() {
                "apps" => true,
                "installed" => self.installed.contains_key(&a.key),
                "running" => self.is_running(&a.key),
                _ => false,
            })
            .filter(|a| a.matches(&query))
            .collect()
    }

    // ── Installing and launching ─────────────────────────────────────────

    /// Install, or update to, the latest release.
    fn install(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if self.is_busy(&key) {
            return;
        }
        let Some(app) = self.app(&key).cloned() else { return };
        let Some(release) = app.release.clone() else { return };
        let done = Arc::new(AtomicU64::new(0));
        self.runs.insert(key.clone(), Run::Installing { done: done.clone(), total: release.size });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let job = app.clone();
            let result = cx.background_executor().spawn(async move { registry::install(&job, &done) }).await;
            let _ = this.update(cx, |this, cx| {
                let note = match &result {
                    Ok(i) => toast(format!("{} {} installed", app.name, i.tag)).success(),
                    Err(err) => toast(format!("Couldn't install {}", app.name)).danger().message(err.clone()),
                };
                this.toaster.update(cx, |t, cx| t.push(note, cx));
                match result {
                    Ok(installed) => {
                        this.installed.insert(key.clone(), installed);
                        this.runs.remove(&key);
                    }
                    Err(err) => {
                        this.runs.insert(key, Run::Failed(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn launch(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if self.is_busy(&key) {
            return;
        }
        let Some(installed) = self.installed.get(&key) else { return self.install(key, cx) };
        let name = self.app(&key).map(|a| a.name.clone()).unwrap_or_else(|| key.clone());
        let run = registry::start(&installed.exe, &self.shared);
        let note = match &run {
            Ok(_) => toast(format!("{name} started")).success().message(self.shared.scheme.to_uppercase()),
            Err(err) => toast(format!("{name} failed")).danger().message(err.clone()),
        };
        self.toaster.update(cx, |t, cx| t.push(note, cx));
        self.runs.insert(key, match run {
            Ok(child) => Run::Running { child, since: Instant::now() },
            Err(err) => Run::Failed(err),
        });
        cx.notify();
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
        self.ticks = self.ticks.wrapping_add(1);
        let mut changed = false;
        for run in self.runs.values_mut() {
            match run {
                Run::Installing { .. } => changed = true,
                Run::Running { child, .. } => {
                    // Uptimes tick once a second.
                    changed |= self.ticks.is_multiple_of(5);
                    if let Ok(Some(status)) = child.try_wait() {
                        *run = Run::Exited(status.code());
                        changed = true;
                    }
                }
                _ => {}
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
            command("Go to installed").group("Navigate").icon(Icon::Down).on_run(go("installed")),
            command("Go to running").group("Navigate").icon(Icon::Play).on_run(go("running")),
            command("Go to shared look").group("Navigate").icon(Icon::Sliders).on_run(go("look")),
            command("Check GitHub for apps").group("Lodestone").icon(Icon::Refresh).on_run({
                let weak = weak.clone();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.sync(cx));
                }
            }),
            command("Stop all apps").group("Lodestone").icon(Icon::Stop).on_run({
                let weak = weak.clone();
                move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.stop_all(cx));
                }
            }),
        ];
        for a in &self.apps {
            let (weak, key) = (weak.clone(), a.key.clone());
            let verb = if self.installed.contains_key(&a.key) { "Launch" } else { "Install" };
            commands.push(command(format!("{verb} {}", a.name)).group(verb).icon(Icon::Play).on_run(move |_, cx| {
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

    fn status(&self, a: &registry::App, window: &mut Window, cx: &App) -> AnyElement {
        let p = palette(cx);
        match self.runs.get(&a.key) {
            Some(Run::Installing { .. }) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(spinner(eid("spin", &a.key)))
                .child(div().display(Scale::X1, window).text_color(hsla(p.fg_dim)).child("INSTALLING"))
                .into_any_element(),
            Some(Run::Running { .. }) => tag("running").accent().into_any_element(),
            Some(Run::Exited(Some(0))) => tag("exited").outline().into_any_element(),
            Some(Run::Stopped) => tag("stopped").outline().into_any_element(),
            Some(Run::Exited(code)) => tag(format!("exit {}", code.map_or("?".into(), |c| c.to_string()))).warning().into_any_element(),
            Some(Run::Failed(_)) => tag("failed").danger().into_any_element(),
            None if self.update_for(&a.key).is_some() => tag("update").warning().into_any_element(),
            None if self.installed.contains_key(&a.key) => tag("installed").outline().into_any_element(),
            None if a.release.is_none() => tag("no release").outline().into_any_element(),
            None => tag("not installed").outline().into_any_element(),
        }
    }

    /// `v0.1.1 · 8 hours ago`: the installed version (or the latest, before
    /// an install) and when that release came out.
    fn version_line(&self, a: &registry::App) -> String {
        let latest = a.release.as_ref();
        let released = |tag: &str| latest.filter(|r| r.tag == tag).map(|r| registry::ago(registry::now() - r.published));
        match (self.installed.get(&a.key), latest) {
            (Some(i), Some(r)) if registry::is_newer(&r.tag, &i.tag) => format!("{} · {} is out", i.tag, r.tag),
            (Some(i), _) => released(&i.tag).map_or(i.tag.clone(), |ago| format!("{} · {ago}", i.tag)),
            (None, Some(r)) => format!("{} · {}", r.tag, registry::ago(registry::now() - r.published)),
            (None, None) => "no release yet".into(),
        }
    }

    /// The live line under a tile.
    fn readout(&self, a: &registry::App) -> String {
        match self.runs.get(&a.key) {
            Some(Run::Running { child, since }) => format!("PID {} · UP {}", child.id(), uptime(since.elapsed())),
            Some(Run::Installing { done, total }) => {
                format!("{} / {}", registry::megabytes(done.load(Ordering::Relaxed)), registry::megabytes(*total))
            }
            Some(Run::Failed(err)) => err.lines().last().unwrap_or("").to_string(),
            _ => self.version_line(a),
        }
    }

    fn progress(&self, key: &SharedString) -> Option<f32> {
        match self.runs.get(key) {
            Some(Run::Installing { done, total }) if *total > 0 => Some(done.load(Ordering::Relaxed) as f32 / *total as f32),
            Some(Run::Installing { .. }) => Some(0.),
            _ => None,
        }
    }

    /// The tile's buttons: Install, Run (plus Update), or Stop.
    fn actions(&self, a: &registry::App, cx: &mut Context<Self>) -> AnyElement {
        let key = a.key.clone();
        let row = div().flex().flex_row().gap_2();
        if self.is_running(&key) {
            return row
                .child(Button::new(eid("stop", &key)).label("Stop").icon(Icon::Stop).ghost().small().on_click(listen(cx, &key, |this, key, cx| this.stop(&key, cx))))
                .into_any_element();
        }
        let installing = matches!(self.runs.get(&key), Some(Run::Installing { .. }));
        if !self.installed.contains_key(&key) {
            return row
                .child(
                    Button::new(eid("install", &key))
                        .label("Install")
                        .icon(Icon::Down)
                        .secondary()
                        .small()
                        .loading(installing)
                        .disabled(a.release.is_none())
                        .on_click(listen(cx, &key, Self::install)),
                )
                .into_any_element();
        }
        row.when(self.update_for(&key).is_some() || installing, |row| {
            row.child(Button::new(eid("update", &key)).label("Update").icon(Icon::Down).ghost().small().loading(installing).on_click(listen(cx, &key, Self::install)))
        })
        .child(Button::new(eid("run", &key)).label("Run").icon(Icon::Play).secondary().small().disabled(installing).on_click(listen(cx, &key, Self::launch)))
        .into_any_element()
    }

    fn tile(&self, a: &registry::App, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let key = a.key.clone();
        let selected = self.selected.as_ref() == Some(&key);
        let running = self.is_running(&key);
        let about = if a.about.is_empty() { SharedString::from("—") } else { a.about.clone() };
        let actions = self.actions(a, cx);

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
                panel(a.name.clone())
                    .meta("APP")
                    .h(px(176.))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .items_start()
                            .child(avatar(a.name.to_string()).size(px(32.)).when(running, |av| av.presence(Presence::Online)))
                            .child(div().flex_1().min_w_0().h(px(40.)).overflow_hidden().body(text::SM).text_color(hsla(p.fg_dim)).child(about)),
                    )
                    .child(div().flex_1())
                    .when_some(self.progress(&a.key), |d, v| d.child(progress_bar(v, px(6.), cx)))
                    .child(div().body(text::SM).text_color(hsla(p.fg_faint)).truncate().child(self.readout(a)))
                    .child(div().flex().flex_row().items_center().child(self.status(a, window, cx)).child(div().flex_1()).child(actions)),
            )
    }

    fn library(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let visible: Vec<registry::App> = self.visible(cx).into_iter().cloned().collect();
        if visible.is_empty() {
            let (title, hint) = match self.nav.as_ref() {
                "running" => ("Nothing running", "Start an installed app."),
                "installed" => ("Nothing installed", "Install an app from the Apps page."),
                _ if !self.search.read(cx).value().is_empty() => ("No matches", "Try a shorter filter."),
                _ if matches!(self.sync, Sync::Checking) => ("Checking GitHub", "Looking for repos tagged ferrite-app."),
                _ => ("No Ferrite apps found", "Tag a GitHub repo with the ferrite-app topic, then refresh."),
            };
            return div().flex_1().flex().items_center().justify_center().child(empty_state(title, hint, window, cx)).into_any_element();
        }
        let tiles: Vec<AnyElement> = visible.iter().map(|a| self.tile(a, window, cx).into_any_element()).collect();
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
        let app = self.selected.as_ref().and_then(|k| self.app(k)).cloned();
        let close = {
            let weak = cx.weak_entity();
            move |_: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    this.drawer = false;
                    cx.notify();
                });
            }
        };
        let mut d = drawer("details").open(self.drawer && app.is_some()).width(px(400.)).on_close(close);
        let Some(a) = app else { return d };
        let key = a.key.clone();
        let running = self.is_running(&key);
        let installing = matches!(self.runs.get(&key), Some(Run::Installing { .. }));
        let installed = self.installed.get(&key).cloned();

        let mut props = property_list()
            .row("installed", installed.as_ref().map_or("no".into(), |i| i.tag.clone()))
            .row(
                "latest",
                a.release.as_ref().map_or("no release yet".into(), |r| format!("{} · {}", r.tag, registry::ago(registry::now() - r.published))),
            )
            .when_some(a.release.as_ref(), |l, r| l.row("download", registry::megabytes(r.size)))
            .when_some(installed.as_ref(), |l, i| l.row("folder", i.exe.parent().map(|d| d.display().to_string()).unwrap_or_default()))
            .row_with("state", self.status(&a, window, cx));
        if let Some(Run::Running { child, since }) = self.runs.get(&key) {
            props = props.row("pid", child.id().to_string()).row("uptime", uptime(since.elapsed()));
        }

        let primary = if running {
            Button::new("drawer-stop").label("Stop").icon(Icon::Stop).danger().on_click(cx.listener({
                let key = key.clone();
                move |this, _: &ClickEvent, _, cx| this.stop(&key, cx)
            }))
        } else if installed.is_some() {
            Button::new("drawer-launch").label("Launch").icon(Icon::Play).primary().disabled(installing).on_click(cx.listener({
                let key = key.clone();
                move |this, _: &ClickEvent, _, cx| this.launch(key.clone(), cx)
            }))
        } else {
            Button::new("drawer-install").label("Install").icon(Icon::Down).primary().loading(installing).disabled(a.release.is_none()).on_click(
                cx.listener({
                    let key = key.clone();
                    move |this, _: &ClickEvent, _, cx| this.install(key.clone(), cx)
                }),
            )
        };
        let update = (installed.is_some() && (self.update_for(&key).is_some() || installing)).then(|| {
            let label = self.update_for(&key).map_or("Update".to_string(), |r| format!("Update to {}", r.tag));
            Button::new("drawer-update").label(label).icon(Icon::Down).secondary().loading(installing).on_click(cx.listener({
                let key = key.clone();
                move |this, _: &ClickEvent, _, cx| this.install(key.clone(), cx)
            }))
        });
        let folder = installed.as_ref().and_then(|i| i.exe.parent().map(|d| d.to_path_buf()));
        let page = a.release.as_ref().map(|r| r.page.clone()).filter(|p| !p.is_empty());

        d = d
            .title(a.name.clone())
            .child(div().body(text::BASE).child(if a.about.is_empty() { SharedString::from("No description on GitHub.") } else { a.about.clone() }))
            .when_some(self.progress(&key), |d, v| d.child(progress_bar(v, px(8.), cx)))
            .child(props)
            .when_some(
                match self.runs.get(&key) {
                    Some(Run::Failed(err)) => Some(err.clone()),
                    _ => None,
                },
                |d, err| d.child(alert("failed", "Something went wrong").danger().message(err)),
            )
            .footer(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .child(primary)
                    .children(update)
                    .when_some(folder, |row, dir| {
                        row.child(Button::new("reveal").label("Open folder").icon(Icon::Folder).secondary().on_click(move |_, _, _| registry::open(dir.as_os_str())))
                    })
                    .when_some(page, |row, url| {
                        row.child(Button::new("release-page").label("Release").icon(Icon::File).ghost().on_click(move |_, _, _| registry::open(url.as_ref())))
                    }),
            );
        d
    }
}

/// A tile button's click: the tile under it doesn't also open the drawer.
fn listen(
    cx: &mut Context<Lodestone>,
    key: &SharedString,
    f: fn(&mut Lodestone, SharedString, &mut Context<Lodestone>),
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let key = key.clone();
    cx.listener(move |this, _: &ClickEvent, _, cx| {
        cx.stop_propagation();
        f(this, key.clone(), cx);
    })
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
        let apps = self.apps.len();
        let installed = self.installed.len();
        let running = self.runs.values().filter(|r| matches!(r, Run::Running { .. })).count();
        let sync = match &self.sync {
            Sync::Checking => "CHECKING GITHUB".to_string(),
            Sync::Done(at) => format!("CHECKED {}", registry::ago(registry::now() - at).to_uppercase()),
            Sync::Offline(_) => "OFFLINE".to_string(),
        };

        let nav = sidebar("nav")
            .section("Library")
            .item_with_meta("apps", "Apps", Icon::Home, apps.to_string())
            .item_with_meta("installed", "Installed", Icon::Down, installed.to_string())
            .item_with_meta("running", "Running", Icon::Play, running.to_string())
            .section("Ecosystem")
            .item("look", "Shared look", Icon::Sliders)
            .selected(self.nav.clone())
            .footer(div().body(text::SM).text_color(hsla(p.fg_faint)).truncate().child(match &self.sync {
                Sync::Offline(err) => format!("offline: {err}"),
                _ => format!("github.com/{} · {}", registry::OWNER, registry::TOPIC),
            }))
            .on_select(cx.listener(|this, key: &SharedString, _, cx| {
                this.nav = key.clone();
                cx.notify();
            }));

        let checking = matches!(self.sync, Sync::Checking);
        let bar = toolbar()
            .child(div().w(px(260.)).child(self.search.clone()))
            .spacer()
            .child(
                Button::new("refresh")
                    .icon(Icon::Refresh)
                    .small()
                    .ghost()
                    .loading(checking)
                    .tooltip("Check GitHub for apps and updates")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.sync(cx))),
            )
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
                    .left(format!("{apps} APPS · {installed} INSTALLED"))
                    .left_live(format!("{running} RUNNING"))
                    .right(sync)
                    .right(theme::scheme(cx).name.to_uppercase())
                    .right_live(format!("{}FPS", motion::fps())),
            );

        let boot = boot_screen("boot", root)
            .title("Lodestone")
            .line("APPS", format!("{apps} KNOWN"))
            .line("INSTALLED", format!("{installed}"))
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

/// A per-app element id: `tile:ferrite-almanac`.
fn eid(prefix: &str, key: &str) -> gpui::ElementId {
    gpui::ElementId::Name(format!("{prefix}:{key}").into())
}
