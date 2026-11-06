use std::fs;
use std::path::{Path, PathBuf};

/// Where the Chromium that drives the page comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineKind {
    /// chrome-headless-shell from Chrome for Testing (what BlinkTerm ships)
    HeadlessShell,
    /// a full Chrome / Chromium build
    Chrome,
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub path: PathBuf,
    pub kind: EngineKind,
    pub version: String,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// BlinkTerm keeps its engine under ~/.local/share/blinkterm/engine/<version>/.
/// Reusing it means ymw works without downloading another 200 MB.
pub fn from_blinkterm_public() -> Option<PathBuf> {
    from_blinkterm().map(|e| e.path)
}

fn from_blinkterm() -> Option<Engine> {
    let root = home().join(".local/share/blinkterm/engine");
    let mut versions: Vec<PathBuf> = fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    // 153.0.8010.52 sorts fine as a string for the same major series; do a
    // numeric-ish compare anyway.
    versions.sort_by(|a, b| {
        let ka = a.file_name().unwrap_or_default().to_string_lossy().to_string();
        let kb = b.file_name().unwrap_or_default().to_string_lossy().to_string();
        cmp_version(&ka, &kb)
    });
    versions.reverse();
    for v in versions {
        for sub in ["chrome-headless-shell-mac-arm64", "chrome-headless-shell-mac-x64"] {
            let p = v.join(sub).join("chrome-headless-shell");
            if p.is_file() {
                let version = v
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                return Some(Engine {
                    path: p,
                    kind: EngineKind::HeadlessShell,
                    version,
                });
            }
        }
    }
    None
}

fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<u32> = a.split('.').filter_map(|x| x.parse().ok()).collect();
    let pb: Vec<u32> = b.split('.').filter_map(|x| x.parse().ok()).collect();
    pa.cmp(&pb)
}

fn from_known_apps() -> Option<Engine> {
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ];
    for c in candidates {
        if Path::new(c).is_file() {
            return Some(Engine {
                path: PathBuf::from(c),
                kind: EngineKind::Chrome,
                version: "system".into(),
            });
        }
    }
    None
}

/// ymw's own copy, installed with `ymw --install-engine`.
fn from_own() -> Option<Engine> {
    let root = home().join(".local/share/ymw/engine");
    let mut versions: Vec<PathBuf> = fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort_by(|a, b| {
        let ka = a.file_name().unwrap_or_default().to_string_lossy().to_string();
        let kb = b.file_name().unwrap_or_default().to_string_lossy().to_string();
        cmp_version(&ka, &kb)
    });
    versions.reverse();
    for v in versions {
        for sub in ["chrome-headless-shell-mac-arm64", "chrome-headless-shell-mac-x64"] {
            let p = v.join(sub).join("chrome-headless-shell");
            if p.is_file() {
                let version = v
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                return Some(Engine {
                    path: p,
                    kind: EngineKind::HeadlessShell,
                    version,
                });
            }
        }
    }
    None
}

fn from_path() -> Option<Engine> {
    let candidates = [
        "chrome-headless-shell",
        "chromium",
        "chromium-browser",
        "google-chrome",
    ];
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for c in candidates {
            let p = dir.join(c);
            if p.is_file() {
                let kind = if c == "chrome-headless-shell" {
                    EngineKind::HeadlessShell
                } else {
                    EngineKind::Chrome
                };
                return Some(Engine {
                    path: p,
                    kind,
                    version: "PATH".into(),
                });
            }
        }
    }
    None
}

pub fn find(explicit: Option<&Path>) -> Option<Engine> {
    if let Some(p) = explicit {
        let kind = if p
            .file_name()
            .map(|n| n.to_string_lossy().contains("headless-shell"))
            .unwrap_or(false)
        {
            EngineKind::HeadlessShell
        } else {
            EngineKind::Chrome
        };
        return Some(Engine {
            path: p.to_path_buf(),
            kind,
            version: "explicit".into(),
        });
    }
    if let Some(p) = std::env::var_os("YMW_ENGINE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(Engine {
                path: p,
                kind: EngineKind::HeadlessShell,
                version: "YMW_ENGINE".into(),
            });
        }
    }
    from_own()
        .or_else(from_blinkterm)
        .or_else(from_known_apps)
        .or_else(from_path)
}

pub fn default_profile() -> PathBuf {
    home().join(".local/share/ymw/profile")
}
