//! Loads ONNX Runtime (`onnxruntime.dll`) once per process.
//!
//! The DLL is loaded from an explicit path — never by bare name, because Windows ships
//! an old `onnxruntime.dll` in System32 that would be picked up otherwise. The CUDA /
//! cuDNN DLL folders (`nvidia\*\bin`, the layout of the pip `nvidia-*-cu12` packages)
//! are added to PATH **before** the runtime is loaded so the CUDA provider can find them.

use crate::util;
use crate::vlog;
use anyhow::{anyhow, Result};
use ort::environment::Environment;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ORT_ENV: OnceLock<Result<Environment, String>> = OnceLock::new();

/// Candidate locations of `onnxruntime.dll`, most specific first.
fn find_runtime_dll() -> Option<PathBuf> {
    for var in ["VCAM_ORT_DLL", "ORT_DYLIB_PATH"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    const RELATIVE: [&str; 5] = [
        "onnxruntime.dll",
        "onnxruntime\\onnxruntime.dll",
        "onnxruntime\\capi\\onnxruntime.dll",
        // a Python virtualenv next to (or above) the app, e.g. the original project's
        "venv\\Lib\\site-packages\\onnxruntime\\capi\\onnxruntime.dll",
        ".venv\\Lib\\site-packages\\onnxruntime\\capi\\onnxruntime.dll",
    ];
    for root in util::search_roots() {
        for rel in RELATIVE {
            let p = root.join(rel);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Every `<base>\nvidia\<package>\bin` folder.
fn nvidia_bin_dirs(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(base.join("nvidia")) {
        for entry in rd.flatten() {
            let bin = entry.path().join("bin");
            if bin.is_dir() {
                out.push(bin);
            }
        }
    }
    out
}

fn prepend_to_path(dirs: &[PathBuf]) {
    if dirs.is_empty() {
        return;
    }
    let mut parts: Vec<PathBuf> = dirs.to_vec();
    if let Some(old) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&old));
    }
    if let Ok(joined) = std::env::join_paths(parts) {
        std::env::set_var("PATH", joined);
    }
}

fn init() -> Result<Environment, String> {
    let dll = find_runtime_dll().ok_or_else(|| {
        "onnxruntime.dll was not found. Put ONNX Runtime 1.20 or newer (onnxruntime.dll and, for \
         NVIDIA GPUs, onnxruntime_providers_cuda.dll + onnxruntime_providers_shared.dll) next to \
         vcam.exe, or set VCAM_ORT_DLL to its full path. See README.md."
            .to_string()
    })?;
    let dll_dir = dll.parent().map(|p| p.to_path_buf()).unwrap_or_default();

    // CUDA 12 / cuDNN 9 runtime DLLs: next to the app (bundle), or in the site-packages
    // folder the runtime itself came from (<site-packages>\onnxruntime\capi\onnxruntime.dll).
    let mut dirs: Vec<PathBuf> = vec![dll_dir.clone()];
    let mut bases: Vec<PathBuf> = vec![util::exe_dir(), dll_dir.clone()];
    if let Some(site) = dll_dir.parent().and_then(|p| p.parent()) {
        bases.push(site.to_path_buf());
    }
    for base in &bases {
        dirs.extend(nvidia_bin_dirs(base));
    }
    prepend_to_path(&dirs);

    let builder = ort::init_from(&dll).map_err(|e| format!("could not load {}: {}", dll.display(), e))?;
    let env = builder
        .with_name("vcam")
        .with_telemetry(false)
        .build()
        .map_err(|e| format!("could not start ONNX Runtime: {}", e))?;
    vlog!("[vcam] ONNX Runtime loaded from {}", dll.display());
    Ok(env)
}

/// The process-wide ONNX Runtime environment (loaded on first use).
pub fn ort_env() -> Result<&'static Environment> {
    match ORT_ENV.get_or_init(init) {
        Ok(env) => Ok(env),
        Err(e) => Err(anyhow!("{}", e)),
    }
}
