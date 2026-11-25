//! Update check against the project's GitHub Releases.
//!
//! A release is a git tag `vX.Y.Z` pushed to GitHub; `.github/workflows/release.yml`
//! builds it and publishes `vcam-X.Y.Z-windows-x64.zip` on the Releases page. This
//! module asks GitHub for the newest published release (drafts and pre-releases are
//! skipped) and compares its tag with the running version (`Cargo.toml`).
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
pub struct Release {
    /// e.g. "0.9.1"
    pub version: String,
    /// The release's page on GitHub (notes + download).
    pub url: String,
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
    Ok(Release { version: tag.trim_start_matches(['v', 'V']).to_string(), url: page })
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    /// No repository in this build.
    Disabled,
    Checking,
    UpToDate,
    Available(Release),
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

    /// Look again now (no effect while a check is running or in a build without a repo).
    pub fn check_now(&self) {
        if self.state() != UpdateState::Checking {
            if let Some(tx) = self.wake.as_ref() {
                let _ = tx.send(());
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
}
