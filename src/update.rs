//! Update check against the project's GitHub Releases.
//!
//! A release is a git tag `vX.Y.Z` pushed to GitHub; `.github/workflows/release.yml`
//! builds it and publishes two installers on the Releases page: `vcam-setup-X.Y.Z.exe`
//! (everything) and `vcam-update-X.Y.Z-rtN.exe` (program files only, for installs on
//! runtime revision N). This module asks GitHub for the newest published release (drafts
//! and pre-releases are skipped), compares its tag with the running version
//! (`Cargo.toml`), and can download and run the installer that fits this installation.
//!
//! The repository ("owner/name") is baked in at build time: `VCAM_UPDATE_REPO` when set,
//! otherwise `GITHUB_REPOSITORY`, which GitHub Actions sets on every build. A local
//! build without either has no repository and never checks. GitHub only answers for a
//! public repository (or one whose releases are public); a private one reads as
//! "check failed".

use crate::vlog;
use anyhow::{anyhow, bail, Result};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The running version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often a long-running vcam (e.g. in the tray) looks again.
const RECHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);

/// "owner/name" of the GitHub repository releases come from, or `None` (no checks).
pub fn repo() -> Option<&'static str> {
    option_env!("VCAM_UPDATE_REPO")
        .or(option_env!("GITHUB_REPOSITORY"))
        .map(str::trim)
        .filter(|r| r.contains('/') && !r.starts_with('/') && !r.ends_with('/'))
}

/// `"v1.2.3"`, `"1.2"`, `"1.2.3-beta"` -> (1, 2, 3) / (1, 2, 0) / (1, 2, 3).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

/// True when `candidate` is a higher version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// SHA-256 (hex) when GitHub reports a digest for the file.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// e.g. "0.9.1"
    pub version: String,
    /// The release's page on GitHub (notes + download).
    pub url: String,
    pub assets: Vec<Asset>,
}

impl Release {
    /// The installer that brings this installation to the release: the small update
    /// package when it was built for the same runtime revision, else the full setup.
    /// `None` for a copy the setup did not install (no `runtime-rev.txt`), which can
    /// only be updated from the release page.
    pub fn installer_for(&self, runtime_rev: Option<&str>) -> Option<&Asset> {
        let rev = runtime_rev?;
        let small = format!("vcam-update-{}-rt{}.exe", self.version, rev);
        let full = format!("vcam-setup-{}.exe", self.version);
        let find = |n: &str| self.assets.iter().find(|a| a.name.eq_ignore_ascii_case(n));
        find(&small).or_else(|| find(&full))
    }
}

/// Runtime revision of this installation: `runtime-rev.txt` next to the exe, written by
/// the full installer (the DLL + model set; see ci/runtime-rev.txt). `None` for a
/// portable or developer copy.
pub fn installed_runtime_rev() -> Option<String> {
    std::fs::read_to_string(crate::util::exe_dir().join("runtime-rev.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Download `asset` to %TEMP%, check its size (and SHA-256 when known), and start it
/// silently. The installer closes vcam, installs, and starts vcam again. Blocks for the
/// whole download: run it off the UI thread.
pub fn download_and_launch(asset: &Asset) -> Result<()> {
    let dir = std::env::temp_dir().join("vcam-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&asset.name);
    let _ = std::fs::remove_file(&path);
    vlog!("[vcam] Downloading {} ...", asset.url);
    let checked = (|| -> Result<()> {
        crate::models::download(&asset.url, &path)?;
        let len = std::fs::metadata(&path)?.len();
        if asset.size > 0 && len != asset.size {
            bail!("the download is incomplete ({} of {} bytes)", len, asset.size);
        }
        if let Some(want) = asset.sha256.as_deref() {
            let got = crate::models::sha256_hex(&path)?;
            if !got.eq_ignore_ascii_case(want) {
                bail!("the download is damaged (checksum mismatch)");
            }
        }
        Ok(())
    })();
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    launch_installer(&path)
}

/// Start the installer silently. ShellExecute (not CreateProcess), so an installation
/// for all users can raise its admin prompt.
fn launch_installer(path: &std::path::Path) -> Result<()> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let file = HSTRING::from(path.as_os_str());
    let params = HSTRING::from("/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS");
    let code = unsafe { ShellExecuteW(HWND::default(), w!("open"), &file, &params, PCWSTR::null(), SW_SHOWNORMAL) };
    let code = code.0 as isize;
    if code <= 32 {
        bail!("could not start the installer (error {})", code);
    }
    vlog!("[vcam] Started {}", path.display());
    Ok(())
}

/// The newest published release, whatever its version.
pub fn latest_release() -> Result<Release> {
    let repo = repo().ok_or_else(|| anyhow!("this build has no update repository"))?;
    // The query string defeats the WinINet cache URLDownloadToFile goes through.
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let url = format!("https://api.github.com/repos/{}/releases/latest?t={}", repo, stamp);
    let dir = crate::util::app_data_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("update-check.json");
    let fetched = crate::models::download(&url, &tmp).and_then(|_| Ok(std::fs::read_to_string(&tmp)?));
    let _ = std::fs::remove_file(&tmp);
    let text = fetched.map_err(|e| anyhow!("could not reach GitHub: {}", e))?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).ok_or_else(|| anyhow!("no release found"))?;
    if parse_version(tag).is_none() {
        bail!("the latest release tag {:?} is not a version", tag);
    }
    let page = v
        .get("html_url")
        .and_then(|u| u.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://github.com/{}/releases/latest", repo));
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a.get("name")?.as_str()?.to_string(),
                        url: a.get("browser_download_url")?.as_str()?.to_string(),
                        size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                        sha256: a
                            .get("digest")
                            .and_then(|d| d.as_str())
                            .and_then(|d| d.strip_prefix("sha256:"))
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), url: page, assets })
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    /// No repository in this build.
    Disabled,
    Checking,
    UpToDate,
    Available(Release),
    /// Downloading the installer for this version.
    Downloading(String),
    /// The installer is running: the app should exit now.
    Launched,
    Failed(String),
}

/// Checks in a background thread: at start, every 12 hours, and on `check_now`.
pub struct UpdateChecker {
    state: Arc<Mutex<UpdateState>>,
    wake: Option<Sender<()>>,
}

impl UpdateChecker {
    pub fn start() -> UpdateChecker {
        if repo().is_none() {
            return UpdateChecker { state: Arc::new(Mutex::new(UpdateState::Disabled)), wake: None };
        }
        let state = Arc::new(Mutex::new(UpdateState::Checking));
        let (tx, rx) = mpsc::channel::<()>();
        let st = state.clone();
        let spawned = std::thread::Builder::new().name("vcam-update".into()).spawn(move || loop {
            let busy = st.lock().map(|s| matches!(*s, UpdateState::Downloading(_) | UpdateState::Launched)).unwrap_or(false);
            if busy {
                // an update is being installed: don't overwrite its state
                if let Err(mpsc::RecvTimeoutError::Disconnected) = rx.recv_timeout(RECHECK_EVERY) {
                    break;
                }
                continue;
            }
            if let Ok(mut s) = st.lock() {
                *s = UpdateState::Checking;
            }
            let next = match latest_release() {
                Ok(r) if is_newer(&r.version, VERSION) => {
                    vlog!("[vcam] Update available: {} (running {}) {}", r.version, VERSION, r.url);
                    UpdateState::Available(r)
                }
                Ok(_) => UpdateState::UpToDate,
                Err(e) => {
                    vlog!("[vcam] Update check failed: {}", e);
                    UpdateState::Failed(e.to_string())
                }
            };
            if let Ok(mut s) = st.lock() {
                *s = next;
            }
            match rx.recv_timeout(RECHECK_EVERY) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // the app is closing
            }
        });
        let wake = match spawned {
            Ok(_) => Some(tx),
            Err(_) => {
                *state.lock().unwrap() = UpdateState::Failed("could not start the update check".into());
                None
            }
        };
        UpdateChecker { state, wake }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().map(|s| s.clone()).unwrap_or(UpdateState::Disabled)
    }

    /// Look again now (no effect while a check or an update is running, or in a build
    /// without a repo).
    pub fn check_now(&self) {
        let idle = !matches!(self.state(), UpdateState::Checking | UpdateState::Downloading(_) | UpdateState::Launched);
        if idle {
            if let Some(tx) = self.wake.as_ref() {
                let _ = tx.send(());
            }
        }
    }

    /// Install `release`: download its installer in the background and start it (state
    /// `Downloading`, then `Launched` or `Failed`). Without an installer for this copy,
    /// opens the release page instead.
    pub fn install(&self, release: &Release) {
        let Some(asset) = release.installer_for(installed_runtime_rev().as_deref()).cloned() else {
            open_in_browser(&release.url);
            return;
        };
        if let Ok(mut s) = self.state.lock() {
            *s = UpdateState::Downloading(release.version.clone());
        }
        let st = self.state.clone();
        let spawned = std::thread::Builder::new().name("vcam-update-install".into()).spawn(move || {
            let next = match download_and_launch(&asset) {
                Ok(()) => UpdateState::Launched,
                Err(e) => {
                    vlog!("[vcam] Update failed: {}", e);
                    UpdateState::Failed(format!("update failed: {}", e))
                }
            };
            if let Ok(mut s) = st.lock() {
                *s = next;
            }
        });
        if spawned.is_err() {
            if let Ok(mut s) = self.state.lock() {
                *s = UpdateState::Failed("could not start the download".into());
            }
        }
    }
}

/// Open `url` in the default browser.
pub fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("explorer").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.9.0"), Some((0, 9, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.3-beta.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("v0.10.0", "0.9.0")); // not a string comparison
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.9.0", "0.9.0"));
        assert!(!is_newer("0.8.5", "0.9.0"));
        assert!(!is_newer("garbage", "0.9.0"));
    }

    #[test]
    fn picks_the_small_update_when_the_runtime_matches() {
        let asset = |n: &str| Asset { name: n.into(), url: format!("https://x/{}", n), size: 1, sha256: None };
        let rel = Release {
            version: "0.9.1".into(),
            url: "https://x".into(),
            assets: vec![asset("vcam-setup-0.9.1.exe"), asset("vcam-update-0.9.1-rt1.exe")],
        };
        assert_eq!(rel.installer_for(Some("1")).unwrap().name, "vcam-update-0.9.1-rt1.exe");
        assert_eq!(rel.installer_for(Some("2")).unwrap().name, "vcam-setup-0.9.1.exe");
        assert!(rel.installer_for(None).is_none()); // not installed by the setup
    }
}
