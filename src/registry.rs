//! What Lodestone knows about the workspace: which crates are Ferrite apps,
//! how to build and start them, and the shared look every launch inherits.
//!
//! Pure parsing is separated from I/O so it can be unit-tested.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use gpui::SharedString;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A crate that depends on ferrite-design.
    App,
    /// An example inside ferrite-design itself (the gallery, the templates).
    Example,
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// Stable id: the package name, or `ferrite-design:<example>`.
    pub key: SharedString,
    pub name: SharedString,
    pub about: SharedString,
    pub dir: PathBuf,
    pub kind: Kind,
    /// The cargo target: the package's binary, or the example's name.
    pub target: String,
}

impl Entry {
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty() || self.name.to_lowercase().contains(&q) || self.about.to_lowercase().contains(&q)
    }
}

/// Where to look, first match wins: a folder given on the command line,
/// `LODESTONE_ROOT`, the folder this crate was built in (when run from a
/// checkout), else the current directory.
pub fn default_root() -> PathBuf {
    if let Some(arg) = std::env::args_os().nth(1) {
        return PathBuf::from(arg);
    }
    if let Some(root) = std::env::var_os("LODESTONE_ROOT") {
        return PathBuf::from(root);
    }
    // A release binary was built elsewhere; only trust this path if it exists here.
    if let Some(parent) = Path::new(env!("CARGO_MANIFEST_DIR")).parent().filter(|p| p.is_dir()) {
        return parent.to_path_buf();
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

// ── Cargo.toml (pure) ─────────────────────────────────────────────────────

#[derive(Default, Debug, PartialEq)]
pub struct Manifest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub uses_ferrite: bool,
}

/// Just enough TOML for `[package] name/description` and a ferrite-design
/// dependency; no need to pull in a parser for three keys.
pub fn parse_manifest(src: &str) -> Manifest {
    let mut m = Manifest::default();
    let mut section = String::new();
    for line in src.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let value = value.trim().trim_matches('"').to_string();
        match section.as_str() {
            "package" if key == "name" => m.name = Some(value),
            "package" if key == "description" => m.description = Some(value),
            s if s.ends_with("dependencies") && key == "ferrite-design" => m.uses_ferrite = true,
            _ => {}
        }
        if section.ends_with("dependencies.ferrite-design") {
            m.uses_ferrite = true;
        }
    }
    m
}

/// The first `//!` line of an example: its one-line summary.
pub fn example_about(src: &str) -> String {
    src.lines()
        .filter_map(|l| l.trim().strip_prefix("//!"))
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `app_dashboard` → `Dashboard`, `components` → `Components`.
pub fn example_name(stem: &str) -> String {
    let stem = stem.strip_prefix("app_").unwrap_or(stem);
    stem.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ferrite-pulse` → `Pulse`.
pub fn app_name(package: &str) -> String {
    example_name(&package.strip_prefix("ferrite-").unwrap_or(package).replace('-', "_"))
}

// ── Scanning (I/O) ────────────────────────────────────────────────────────

/// Every Ferrite app under `root` (one level deep), then ferrite-design's
/// examples. Lodestone itself is left out.
pub fn scan(root: &Path) -> Vec<Entry> {
    let mut apps = Vec::new();
    let mut examples = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else { return apps };
    for dir in dirs.flatten().map(|d| d.path()).filter(|p| p.is_dir()) {
        let Ok(src) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        let m = parse_manifest(&src);
        let Some(name) = m.name else { continue };
        if name == "ferrite-design" {
            examples = scan_examples(&dir);
        } else if m.uses_ferrite && name != env!("CARGO_PKG_NAME") {
            apps.push(Entry {
                key: name.clone().into(),
                name: app_name(&name).into(),
                about: m.description.unwrap_or_default().into(),
                dir,
                kind: Kind::App,
                target: name,
            });
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps.extend(examples);
    apps
}

fn scan_examples(dir: &Path) -> Vec<Entry> {
    let Ok(files) = std::fs::read_dir(dir.join("examples")) else { return Vec::new() };
    let mut out: Vec<Entry> = files
        .flatten()
        .map(|f| f.path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_string();
            // The cheat sheet is a compile check, not something to look at.
            if stem == "cheatsheet" {
                return None;
            }
            let about = std::fs::read_to_string(&p).map(|s| example_about(&s)).unwrap_or_default();
            Some(Entry {
                key: format!("ferrite-design:{stem}").into(),
                name: example_name(&stem).into(),
                about: about.into(),
                dir: dir.to_path_buf(),
                kind: Kind::Example,
                target: stem,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `2 days ago · Add the thing`, or `None` outside git.
pub fn last_commit(dir: &Path) -> Option<String> {
    let out = quiet(Command::new("git").args(["log", "-1", "--format=%cr · %s"]).current_dir(dir)).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

// ── Build and start ───────────────────────────────────────────────────────

fn target_dir(dir: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| dir.join("target"))
}

pub fn binary(e: &Entry) -> PathBuf {
    let exe = format!("{}{}", e.target, std::env::consts::EXE_SUFFIX);
    match e.kind {
        Kind::App => target_dir(&e.dir).join("debug").join(exe),
        Kind::Example => target_dir(&e.dir).join("debug").join("examples").join(exe),
    }
}

/// Build the entry (blocking; run it off the main thread). Builds first and
/// then starts the binary directly, so Lodestone holds the app's own process
/// rather than cargo's, and Stop really stops it.
pub fn build(e: &Entry) -> Result<PathBuf, String> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("--quiet").current_dir(&e.dir);
    if e.kind == Kind::Example {
        cmd.args(["--example", &e.target]);
    }
    let out = quiet(&mut cmd).output().map_err(|err| format!("cargo: {err}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        return Err(tail.iter().rev().take(3).rev().copied().collect::<Vec<_>>().join("\n"));
    }
    let bin = binary(e);
    if bin.exists() { Ok(bin) } else { Err(format!("built, but {} is missing", bin.display())) }
}

pub fn start(bin: &Path, dir: &Path, shared: &Shared) -> Result<Child, String> {
    quiet(Command::new(bin).current_dir(dir).envs(shared.env()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{}: {err}", bin.display()))
}

pub fn reveal(dir: &Path) {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener).arg(dir).spawn();
}

/// No console window flashing up on Windows for every child.
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

// ── The shared look ───────────────────────────────────────────────────────

/// One look for the whole ecosystem: saved to `<config>/ferrite/shared.conf`
/// and handed to every app Lodestone starts as `FERRITE_*` variables, which
/// `theme::apply_env` already reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Shared {
    pub scheme: String,
    /// `dark`, `light` or `system`.
    pub appearance: String,
    pub fps: u32,
    /// `compact`, `cozy` or `roomy`.
    pub density: String,
}

impl Default for Shared {
    fn default() -> Self {
        Self { scheme: "ferrite".into(), appearance: "dark".into(), fps: 240, density: "cozy".into() }
    }
}

impl Shared {
    pub fn parse(src: &str) -> Self {
        let mut s = Self::default();
        for line in src.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "scheme" => s.scheme = v.into(),
                "appearance" if matches!(v, "dark" | "light" | "system") => s.appearance = v.into(),
                "fps" => s.fps = v.parse().map(|f: u32| f.clamp(12, 240)).unwrap_or(s.fps),
                "density" if matches!(v, "compact" | "cozy" | "roomy") => s.density = v.into(),
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        format!(
            "# Shared Ferrite look, written by Lodestone.\nscheme = {}\nappearance = {}\nfps = {}\ndensity = {}\n",
            self.scheme, self.appearance, self.fps, self.density
        )
    }

    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("FERRITE_SCHEME", self.scheme.clone()),
            ("FERRITE_APPEARANCE", self.appearance.clone()),
            ("FERRITE_FPS", self.fps.to_string()),
            ("FERRITE_DENSITY", self.density.clone()),
        ]
    }

    pub fn path() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        };
        base.map(|b| b.join("ferrite").join("shared.conf"))
    }

    pub fn load() -> Self {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| Self::parse(&s)).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, self.serialize()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_finds_name_description_and_dependency() {
        let m = parse_manifest(
            "[package]\nname = \"ferrite-pulse\"\ndescription = \"Watch things\"\n\n[dependencies]\nferrite-design = { git = \"x\" }\ngpui = \"1\"\n",
        );
        assert_eq!(m, Manifest { name: Some("ferrite-pulse".into()), description: Some("Watch things".into()), uses_ferrite: true });
    }

    #[test]
    fn manifest_ignores_names_outside_package() {
        let m = parse_manifest("[[bin]]\nname = \"other\"\n[package]\nname = \"plain\"\n[dependencies]\nserde = \"1\"\n");
        assert_eq!(m.name.as_deref(), Some("plain"));
        assert!(!m.uses_ferrite);
    }

    #[test]
    fn manifest_sees_table_style_dependency() {
        assert!(parse_manifest("[package]\nname = \"a\"\n[dependencies.ferrite-design]\npath = \"../x\"\n").uses_ferrite);
    }

    #[test]
    fn names_read_well() {
        assert_eq!(example_name("app_dashboard"), "Dashboard");
        assert_eq!(example_name("components"), "Components");
        assert_eq!(app_name("ferrite-desk-clock"), "Desk Clock");
        assert_eq!(app_name("logscope"), "Logscope");
    }

    #[test]
    fn example_about_is_first_doc_line() {
        assert_eq!(example_about("//!\n//! Template: a monitoring dashboard.\n//! More.\nfn main() {}"), "Template: a monitoring dashboard.");
        assert_eq!(example_about("fn main() {}"), "");
    }

    #[test]
    fn shared_round_trips_and_rejects_junk() {
        let s = Shared { scheme: "harbor".into(), appearance: "light".into(), fps: 25, density: "roomy".into() };
        assert_eq!(Shared::parse(&s.serialize()), s);
        let junk = Shared::parse("appearance = purple\nfps = 9000\ndensity = huge\n");
        assert_eq!(junk, Shared { fps: 240, ..Shared::default() });
    }

    #[test]
    fn search_matches_name_or_about() {
        let e = Entry { key: "k".into(), name: "Dashboard".into(), about: "monitoring".into(), dir: ".".into(), kind: Kind::Example, target: "x".into() };
        assert!(e.matches("dash") && e.matches("MONITOR") && e.matches("  ") && !e.matches("wizard"));
    }
}
