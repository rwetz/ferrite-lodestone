//! What Lodestone knows: which Ferrite apps exist (GitHub repos tagged
//! `ferrite-app`), their latest releases, which are installed here, how to
//! install and start them, and the shared look every launch inherits.
//!
//! Lodestone never compiles anything. An install downloads the release
//! archive for this platform, checks it against the release's
//! `SHA256SUMS.txt`, and unpacks it under the apps folder.
//!
//! Pure parsing is separated from I/O so it can be unit-tested.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::SharedString;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Whose GitHub repos are searched for apps.
pub const OWNER: &str = "rwetz";
/// The topic that marks a repo as a Ferrite app.
pub const TOPIC: &str = "ferrite-app";
/// Lodestone's own repo: it updates itself from these releases.
pub const SELF_REPO: &str = "ferrite-lodestone";
/// This build's version, as its release is tagged.
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// The release-archive target for this build of Lodestone.
pub const TARGET: &str = if cfg!(windows) {
    "x86_64-pc-windows-msvc"
} else if cfg!(target_os = "macos") {
    "aarch64-apple-darwin"
} else {
    "x86_64-unknown-linux-gnu"
};

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// `v0.1.1`.
    pub tag: String,
    /// Unix seconds.
    pub published: i64,
    /// The archive for [`TARGET`].
    pub asset: String,
    pub asset_url: String,
    pub size: u64,
    pub sums_url: Option<String>,
    pub page: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct App {
    /// Stable id: the repo name, which is also the binary's name.
    pub key: SharedString,
    pub name: SharedString,
    pub about: SharedString,
    /// The latest release with an archive for this platform, if any.
    pub release: Option<Release>,
}

impl App {
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty() || self.name.to_lowercase().contains(&q) || self.about.to_lowercase().contains(&q)
    }
}

/// An app as it sits on disk.
#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub tag: String,
    pub exe: PathBuf,
}

/// `ferrite-desk-clock` → `Desk Clock`.
pub fn app_name(repo: &str) -> String {
    repo.strip_prefix("ferrite-")
        .unwrap_or(repo)
        .split('-')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── GitHub responses (pure) ───────────────────────────────────────────────

/// `(name, description)` of every repo in a `/users/:owner/repos` page that
/// carries [`TOPIC`] and isn't archived.
pub fn parse_repos(body: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value = serde_json::from_str(body).map_err(|_| "GitHub sent something that isn't JSON".to_string())?;
    let repos = v.as_array().ok_or("GitHub didn't send a list of repos")?;
    Ok(repos
        .iter()
        .filter(|r| !r["archived"].as_bool().unwrap_or(false))
        .filter(|r| r["topics"].as_array().is_some_and(|t| t.iter().any(|t| t == TOPIC)))
        .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["description"].as_str().unwrap_or("").to_string())))
        .collect())
}

/// A `/releases/latest` response, if it has an archive for `target`.
pub fn parse_release(body: &str, target: &str) -> Option<Release> {
    let v: Value = serde_json::from_str(body).ok()?;
    let assets = v["assets"].as_array()?;
    let named = |pred: &dyn Fn(&str) -> bool| assets.iter().find(|a| a["name"].as_str().is_some_and(pred));
    let archive = named(&|n| n.contains(target) && (n.ends_with(".zip") || n.ends_with(".tar.gz")))?;
    Some(Release {
        tag: v["tag_name"].as_str()?.to_string(),
        published: v["published_at"].as_str().and_then(parse_time).unwrap_or(0),
        asset: archive["name"].as_str()?.to_string(),
        asset_url: archive["browser_download_url"].as_str()?.to_string(),
        size: archive["size"].as_u64().unwrap_or(0),
        sums_url: named(&|n| n == "SHA256SUMS.txt").and_then(|a| a["browser_download_url"].as_str()).map(str::to_string),
        page: v["html_url"].as_str().unwrap_or("").to_string(),
    })
}

/// The hash `SHA256SUMS.txt` lists for `file` (`<hex>  <name>` lines).
pub fn sum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then(|| hash.to_lowercase())
    })
}

/// `2026-10-07T13:26:11Z` → Unix seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `8 hours ago`, the way git says it.
pub fn ago(secs: i64) -> String {
    let unit = |n: i64, u: &str| format!("{n} {u}{} ago", if n == 1 { "" } else { "s" });
    match secs.max(0) {
        0..60 => "just now".into(),
        s @ 60..3600 => unit(s / 60, "minute"),
        s @ 3600..86_400 => unit(s / 3600, "hour"),
        s @ 86_400..2_592_000 => unit(s / 86_400, "day"),
        s @ 2_592_000..31_536_000 => unit(s / 2_592_000, "month"),
        s => unit(s / 31_536_000, "year"),
    }
}

/// Whether `latest` is a newer tag than `installed` (`v0.1.10` > `v0.1.9`).
pub fn is_newer(latest: &str, installed: &str) -> bool {
    let parse = |t: &str| t.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok()).collect::<Option<Vec<_>>>();
    match (parse(latest), parse(installed)) {
        (Some(l), Some(i)) => l > i,
        _ => latest != installed,
    }
}

/// `5.2 MB`.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.)
}

// ── GitHub (I/O) ──────────────────────────────────────────────────────────

fn get(url: &str) -> Result<ureq::Body, String> {
    let mut req = ureq::get(url).header("User-Agent", "ferrite-lodestone").header("Accept", "application/vnd.github+json");
    // Optional: lifts the 60-requests-an-hour limit for anonymous calls.
    if let Some(token) = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()) {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let resp = req.call().map_err(|e| match e {
        ureq::Error::StatusCode(403 | 429) => "GitHub's rate limit is used up; try again in a while".to_string(),
        ureq::Error::StatusCode(code) => format!("GitHub returned HTTP {code}"),
        _ => "couldn't reach GitHub".to_string(),
    })?;
    Ok(resp.into_body())
}

fn get_text(url: &str) -> Result<String, String> {
    get(url)?.read_to_string().map_err(|_| "couldn't read GitHub's response".to_string())
}

/// Every app on GitHub with its latest release (blocking; run it off the
/// main thread). A repo without a release yet is still listed.
pub fn fetch_catalog() -> Result<Vec<App>, String> {
    let repos = parse_repos(&get_text(&format!("https://api.github.com/users/{OWNER}/repos?per_page=100&type=owner"))?)?;
    let mut apps: Vec<App> = repos
        .into_iter()
        .map(|(repo, about)| {
            let release = get_text(&format!("https://api.github.com/repos/{OWNER}/{repo}/releases/latest"))
                .ok()
                .and_then(|body| parse_release(&body, TARGET));
            fetch_icon(&repo);
            App { name: app_name(&repo).into(), key: repo.into(), about: about.into(), release }
        })
        .collect();
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

/// Where every Ferrite app keeps its logo.
const ICON: &str = "assets/logo.svg";

/// Cache `repo`'s logo for [`icon`]. An app without one keeps its
/// identicon; a failed fetch keeps the last copy.
fn fetch_icon(repo: &str) {
    let Some(path) = icons_dir().map(|d| d.join(format!("{repo}.svg"))) else { return };
    let Ok(svg) = get_text(&format!("https://raw.githubusercontent.com/{OWNER}/{repo}/HEAD/{ICON}")) else { return };
    if !is_svg(&svg) || std::fs::read_to_string(&path).is_ok_and(|old| old == svg) {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, svg);
}

/// Whether a download is an SVG and not, say, an error page.
pub fn is_svg(body: &str) -> bool {
    let head = body.trim_start();
    (head.starts_with("<svg") || head.starts_with("<?xml")) && body.contains("</svg>")
}

/// `key`'s cached logo, if Lodestone has fetched one.
pub fn icon(key: &str) -> Option<PathBuf> {
    icons_dir().map(|d| d.join(format!("{key}.svg"))).filter(|p| p.exists())
}

/// Lodestone's latest release, when it's newer than this build (blocking).
pub fn fetch_self() -> Result<Option<Release>, String> {
    let body = get_text(&format!("https://api.github.com/repos/{OWNER}/{SELF_REPO}/releases/latest"))?;
    Ok(parse_release(&body, TARGET).filter(|r| is_newer(&r.tag, VERSION)))
}

// ── On disk ───────────────────────────────────────────────────────────────

/// `<data>/ferrite`: `%LOCALAPPDATA%` on Windows.
fn data_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.map(|b| b.join("ferrite"))
}

/// Where installed apps live: `<data>/ferrite/apps/<repo>/<tag>/…`.
pub fn apps_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("apps"))
}

fn icons_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("icons"))
}

fn catalog_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("catalog.json"))
}

/// The last catalog fetched, so Lodestone opens with its apps offline.
pub fn load_catalog() -> Vec<App> {
    let Some(src) = catalog_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    let Ok(Value::Array(apps)) = serde_json::from_str::<Value>(&src) else { return Vec::new() };
    apps.iter()
        .filter_map(|a| {
            let s = |v: &Value| v.as_str().map(str::to_string);
            let r = &a["release"];
            Some(App {
                key: s(&a["key"])?.into(),
                name: s(&a["name"])?.into(),
                about: s(&a["about"]).unwrap_or_default().into(),
                release: r.is_object().then(|| {
                    Some(Release {
                        tag: s(&r["tag"])?,
                        published: r["published"].as_i64().unwrap_or(0),
                        asset: s(&r["asset"])?,
                        asset_url: s(&r["asset_url"])?,
                        size: r["size"].as_u64().unwrap_or(0),
                        sums_url: s(&r["sums_url"]),
                        page: s(&r["page"]).unwrap_or_default(),
                    })
                })?,
            })
        })
        .collect()
}

pub fn save_catalog(apps: &[App]) {
    let Some(path) = catalog_path() else { return };
    let v: Vec<Value> = apps
        .iter()
        .map(|a| {
            json!({
                "key": a.key.as_ref(), "name": a.name.as_ref(), "about": a.about.as_ref(),
                "release": a.release.as_ref().map(|r| json!({
                    "tag": r.tag, "published": r.published, "asset": r.asset, "asset_url": r.asset_url,
                    "size": r.size, "sums_url": r.sums_url, "page": r.page,
                })),
            })
        })
        .collect();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// The `installed` marker: the tag, then the binary's path inside the app's folder.
pub fn installed(key: &str) -> Option<Installed> {
    let dir = apps_dir()?.join(key);
    let src = std::fs::read_to_string(dir.join("installed")).ok()?;
    let mut lines = src.lines();
    let (tag, rel) = (lines.next()?.trim().to_string(), lines.next()?.trim());
    let exe = dir.join(rel);
    exe.exists().then_some(Installed { tag, exe })
}

/// `<name>` or `<name>.exe`, up to two folders down (the archive's own folder).
fn find_exe(dir: &Path, name: &str, depth: u8) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let entries: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
    entries.iter().find(|p| p.is_file() && p.file_name().is_some_and(|n| n == file.as_str())).cloned().or_else(|| {
        (depth > 0).then(|| entries.iter().filter(|p| p.is_dir()).find_map(|p| find_exe(p, name, depth - 1))).flatten()
    })
}

/// Download `r`'s archive into `root`, check it against the release's
/// `SHA256SUMS.txt`, and unpack it into `dest` (emptied first). `done`
/// counts the archive's bytes as they arrive.
fn download(r: &Release, root: &Path, dest: &Path, done: &AtomicU64) -> Result<(), String> {
    let expected = match &r.sums_url {
        Some(url) => Some(sum_for(&get_text(url)?, &r.asset).ok_or("the release's SHA256SUMS.txt doesn't list this archive")?),
        None => None,
    };

    let archive = root.join(&r.asset);
    let mut body = get(&r.asset_url)?;
    let mut reader = body.as_reader();
    let mut file = std::fs::File::create(&archive).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|_| "the download was cut off".to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    drop(file);
    let got = format!("{:x}", hasher.finalize());
    if expected.as_ref().is_some_and(|e| *e != got) {
        let _ = std::fs::remove_file(&archive);
        return Err("the download doesn't match the release's checksum".into());
    }

    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    // bsdtar ships with Windows 10+ and reads .zip; macOS and Linux have tar.
    let tar = if cfg!(windows) { PathBuf::from(std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32\\tar.exe") } else { "tar".into() };
    let out = quiet(Command::new(tar).arg("-xf").arg(&archive).arg("-C").arg(dest)).output().map_err(|e| format!("tar: {e}"))?;
    let _ = std::fs::remove_file(&archive);
    if !out.status.success() {
        return Err(format!("couldn't unpack: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Download, verify and unpack `app`'s latest release (blocking; run it off
/// the main thread). The previous version's folder is removed afterwards
/// when it isn't in use.
pub fn install(app: &App, done: &AtomicU64) -> Result<Installed, String> {
    let r = app.release.as_ref().ok_or("no release for this platform yet")?;
    let root = apps_dir().ok_or("no data directory")?.join(app.key.as_ref());
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let dest = root.join(&r.tag);
    download(r, &root, &dest, done)?;
    let exe = find_exe(&dest, &app.key, 2).ok_or("the archive has no app in it")?;
    let rel = exe.strip_prefix(&root).map_err(|e| e.to_string())?;
    std::fs::write(root.join("installed"), format!("{}\n{}\n", r.tag, rel.display())).map_err(|e| e.to_string())?;

    // Older versions go; one still running is left for the next install.
    for old in std::fs::read_dir(&root).into_iter().flatten().flatten().map(|e| e.path()) {
        if old.is_dir() && old != dest {
            let _ = std::fs::remove_dir_all(old);
        }
    }
    Ok(Installed { tag: r.tag.clone(), exe })
}

/// Where the replaced Lodestone waits until the next start removes it.
fn set_aside(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Put release `r` of Lodestone where the running binary is (blocking).
/// The running binary can't be overwritten on Windows but can be renamed,
/// so it moves aside first; the next start deletes it. Returns the path to
/// start the new version from.
pub fn update_self(r: &Release, done: &AtomicU64) -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let root = data_dir().ok_or("no data directory")?.join("self-update");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let dest = root.join(&r.tag);
    download(r, &root, &dest, done)?;
    let fresh = find_exe(&dest, "lodestone", 2).ok_or("the archive has no Lodestone in it")?;

    let aside = set_aside(&current);
    let _ = std::fs::remove_file(&aside);
    std::fs::rename(&current, &aside).map_err(|e| format!("couldn't move the running Lodestone aside: {e}"))?;
    if let Err(e) = std::fs::copy(&fresh, &current) {
        let _ = std::fs::rename(&aside, &current);
        return Err(format!("couldn't put the new Lodestone in place: {e}"));
    }
    let _ = std::fs::remove_dir_all(&root);
    Ok(current)
}

/// Delete the binary a self-update set aside, now that it isn't running.
pub fn clear_set_aside() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(set_aside(&exe));
    }
}

pub fn start(exe: &Path, shared: &Shared) -> Result<Child, String> {
    quiet(Command::new(exe).current_dir(exe.parent().unwrap_or(Path::new("."))).envs(shared.env()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{}: {err}", exe.display()))
}

/// Open a folder or a URL with the system's handler.
pub fn open(target: &std::ffi::OsStr) {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener).arg(target).spawn();
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
    fn names_read_well() {
        assert_eq!(app_name("ferrite-desk-clock"), "Desk Clock");
        assert_eq!(app_name("ferrite-almanac"), "Almanac");
        assert_eq!(app_name("logscope"), "Logscope");
    }

    #[test]
    fn repos_need_the_topic_and_not_archived() {
        let body = r#"[
            {"name": "ferrite-almanac", "description": "A clock", "topics": ["ferrite-app"], "archived": false},
            {"name": "ferrite-design", "description": "Design", "topics": ["gpui"]},
            {"name": "ferrite-old", "description": null, "topics": ["ferrite-app"], "archived": true},
            {"name": "ferrite-bare", "description": null, "topics": ["ferrite-app"]}
        ]"#;
        assert_eq!(parse_repos(body).unwrap(), vec![("ferrite-almanac".into(), "A clock".into()), ("ferrite-bare".into(), String::new())]);
        assert!(parse_repos("{\"message\": \"Not Found\"}").is_err());
    }

    #[test]
    fn release_picks_this_platforms_archive() {
        let body = r#"{"tag_name": "v0.1.1", "published_at": "2026-10-07T13:26:11Z", "html_url": "https://x/r",
            "assets": [
                {"name": "ferrite-a-v0.1.1-aarch64-apple-darwin.tar.gz", "size": 1, "browser_download_url": "https://x/mac"},
                {"name": "ferrite-a-v0.1.1-x86_64-pc-windows-msvc.zip", "size": 5204517, "browser_download_url": "https://x/win"},
                {"name": "SHA256SUMS.txt", "size": 9, "browser_download_url": "https://x/sums"}
            ]}"#;
        let r = parse_release(body, "x86_64-pc-windows-msvc").unwrap();
        assert_eq!((r.tag.as_str(), r.asset_url.as_str(), r.size), ("v0.1.1", "https://x/win", 5204517));
        assert_eq!(r.sums_url.as_deref(), Some("https://x/sums"));
        assert_eq!(r.published, 1_791_379_571);
        assert!(parse_release(body, "riscv64gc-unknown-linux-gnu").is_none());
    }

    #[test]
    fn sums_find_the_file() {
        let sums = "ABC123  ferrite-a-v0.1.1-x86_64-pc-windows-msvc.zip\ndef456 *ferrite-a-v0.1.1-aarch64-apple-darwin.tar.gz\n";
        assert_eq!(sum_for(sums, "ferrite-a-v0.1.1-x86_64-pc-windows-msvc.zip").as_deref(), Some("abc123"));
        assert_eq!(sum_for(sums, "ferrite-a-v0.1.1-aarch64-apple-darwin.tar.gz").as_deref(), Some("def456"));
        assert_eq!(sum_for(sums, "other.zip"), None);
    }

    #[test]
    fn times_and_ages() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_time("garbage"), None);
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(8 * 3600 + 59), "8 hours ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(-30), "just now");
    }

    #[test]
    fn icons_must_be_svg() {
        assert!(is_svg("<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>\n"));
        assert!(is_svg("<?xml version=\"1.0\"?>\n<svg></svg>"));
        assert!(!is_svg("404: Not Found"));
        assert!(!is_svg("<html><body>sign in</body></html>"));
    }

    #[test]
    fn newer_compares_numbers_not_text() {
        assert!(is_newer("v0.1.10", "v0.1.9"));
        assert!(is_newer("v0.2.0", "v0.1.1"));
        assert!(!is_newer("v0.1.1", "v0.1.1"));
        assert!(!is_newer("v0.1.0", "v0.1.1"));
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
        let a = App { key: "k".into(), name: "Barometer".into(), about: "weather station".into(), release: None };
        assert!(a.matches("baro") && a.matches("WEATHER") && a.matches("  ") && !a.matches("radio"));
    }
}
