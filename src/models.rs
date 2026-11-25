//! The selectable matting models: where they come from and how to find / fetch them.
//!
//! Both are RobustVideoMatting ONNX files run by ONNX Runtime, the same files the Python
//! app uses. (The Python app also offers MediaPipe selfie models; this port does not.)

use crate::util;
use crate::vlog;
use anyhow::{anyhow, bail, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct ModelSpec {
    pub name: &'static str,
    pub file: &'static str,
    pub min_bytes: u64,
    pub url: &'static str,
    /// Checked after a download when present.
    pub sha256: Option<&'static str>,
}

/// Every selectable model, by name.
pub const MODEL_REGISTRY: [ModelSpec; 2] = [
    ModelSpec {
        name: "rvm_mobilenetv3_fp32",
        file: "rvm_mobilenetv3_fp32.onnx",
        min_bytes: 1_000_000,
        url: "https://github.com/PeterL1n/RobustVideoMatting/releases/download/v1.0.0/rvm_mobilenetv3_fp32.onnx",
        sha256: None,
    },
    ModelSpec {
        name: "rvm_resnet50_fp32",
        file: "rvm_resnet50_fp32.onnx",
        min_bytes: 1_000_000,
        url: "https://github.com/PeterL1n/RobustVideoMatting/releases/download/v1.0.0/rvm_resnet50_fp32.onnx",
        sha256: None,
    },
];

pub const DEFAULT_MODEL: &str = "rvm_mobilenetv3_fp32";

pub fn model_names() -> Vec<&'static str> {
    MODEL_REGISTRY.iter().map(|m| m.name).collect()
}

pub fn spec(name: &str) -> Option<&'static ModelSpec> {
    MODEL_REGISTRY.iter().find(|m| m.name == name)
}

/// The registry name to use: `model` when it is a known one, else the default. Names
/// of models this port does not have (e.g. the Python app's MediaPipe selfie models in
/// a shared settings file) also fall back to the default.
pub fn resolve_model(model: Option<&str>) -> &'static str {
    model.and_then(spec).map(|s| s.name).unwrap_or(DEFAULT_MODEL)
}

/// Cheap sanity check: a real model is not tiny and not an HTML error page.
fn model_ok(path: &Path, min_bytes: u64) -> bool {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() || meta.len() < min_bytes {
        return false;
    }
    let mut head = [0u8; 64];
    let n = match std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) {
        Ok(n) => n,
        Err(_) => return false,
    };
    let first = head[..n].iter().find(|b| !b.is_ascii_whitespace());
    first != Some(&b'<') // '<html' / '<!DOCTYPE' = failed download
}

/// Where downloaded models are cached: `%LOCALAPPDATA%\vcam\models`.
pub fn cache_dir() -> PathBuf {
    util::app_data_dir().join("models")
}

fn sha256_hex(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

/// Download `url` to `dest` (follows redirects, uses the system proxy settings).
fn download(url: &str, dest: &Path) -> Result<()> {
    use windows::core::{IUnknown, HSTRING};
    use windows::Win32::System::Com::IBindStatusCallback;
    use windows::Win32::System::Com::Urlmon::URLDownloadToFileW;
    let url_w = HSTRING::from(url);
    let dest_w = HSTRING::from(dest.as_os_str());
    unsafe {
        URLDownloadToFileW(None::<&IUnknown>, &url_w, &dest_w, 0, None::<&IBindStatusCallback>)
            .map_err(|e| anyhow!("{}", e))?;
    }
    Ok(())
}

/// Fetch a model into `dir` (atomic: `.part` -> rename) and validate it.
pub fn download_model(spec: &ModelSpec, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(spec.file);
    let tmp = dir.join(format!("{}.part", spec.file));
    vlog!("[vcam] Downloading {} model -> {} ...", spec.name, path.display());
    let result = (|| -> Result<()> {
        download(spec.url, &tmp)?;
        if !model_ok(&tmp, spec.min_bytes) {
            bail!("the downloaded file is not a valid model");
        }
        if let Some(want) = spec.sha256 {
            let got = sha256_hex(&tmp)?;
            if !got.eq_ignore_ascii_case(want) {
                bail!("checksum mismatch (expected {}, got {})", want, got);
            }
        }
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        bail!("download of the {} model from {} failed: {}", spec.name, spec.url, e);
    }
    vlog!("[vcam] Model ready.");
    Ok(path)
}

/// Return a local path for the named model. Looks in `models\` and `model\` next to the
/// app and in the folders above it (so a source checkout finds `..\models`), then in
/// the download cache, and finally downloads it. A corrupt/partial cached copy is
/// deleted and fetched again.
pub fn ensure_named_model(name: &str) -> Result<PathBuf> {
    let spec = spec(name).ok_or_else(|| anyhow!("unknown model: {}", name))?;
    for root in util::search_roots() {
        for folder in ["models", "model"] {
            let cand = root.join(folder).join(spec.file);
            if model_ok(&cand, spec.min_bytes) {
                vlog!("[vcam] Using {} model: {}", name, cand.display());
                return Ok(cand);
            }
        }
    }
    let cache = cache_dir().join(spec.file);
    if model_ok(&cache, spec.min_bytes) {
        vlog!("[vcam] Using {} model: {}", name, cache.display());
        return Ok(cache);
    }
    if cache.exists() {
        vlog!("[vcam] Removing bad cached model {}", cache.display());
        let _ = std::fs::remove_file(&cache);
    }
    download_model(spec, &cache_dir())
}

/// Make sure every registry model exists in `dir`, copying a local copy when one is
/// found and downloading the rest (used by `build.cmd` to bundle the models).
pub fn bundle_all_models(dir: &Path) -> Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut n = 0;
    for spec in MODEL_REGISTRY.iter() {
        let dest = dir.join(spec.file);
        if !model_ok(&dest, spec.min_bytes) {
            let src = ensure_named_model(spec.name)?;
            if src != dest {
                std::fs::copy(&src, &dest)?;
            }
        }
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve() {
        assert_eq!(resolve_model(None), DEFAULT_MODEL);
        assert_eq!(resolve_model(Some("rvm_resnet50_fp32")), "rvm_resnet50_fp32");
        assert_eq!(resolve_model(Some("selfie_segmenter")), DEFAULT_MODEL);
        assert_eq!(resolve_model(Some("nope")), DEFAULT_MODEL);
        assert_eq!(MODEL_REGISTRY.len(), 2);
    }
}
