//! Small shared helpers: locations on disk, Python-compatible rounding, error mapping.

use std::fmt::Display;
use std::path::{Path, PathBuf};

/// Size Rayon's global pool (the per-frame image loops) before anything uses it.
///
/// Rayon's default is one worker per logical CPU (20 on an i7-12700KF). The loops are
/// short (1-5 ms, memory-bound) and run about 36 times a frame, so the idle workers spin
/// between them: measured 4.26 CPU cores at 18.8 fps with the default pool, 0.38 cores at
/// 20.7 fps with 4 workers (CLI, blur, CUDA). `RAYON_NUM_THREADS`, when set, still wins.
pub fn init_thread_pool() {
    if std::env::var_os("RAYON_NUM_THREADS").is_some() {
        return;
    }
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4);
    let _ = rayon::ThreadPoolBuilder::new().num_threads(n).build_global();
}

/// Folder that holds the running executable.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `%LOCALAPPDATA%\vcam` — settings, log and the downloaded-model cache (the same folder
/// the Python app uses, so settings and cached models carry over).
pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("vcam")
}

/// The executable's folder followed by its ancestors (nearest first). Data that ships
/// next to the app (models, assets, onnxruntime) is looked up along this list, so both
/// a bundled folder (`vcam.exe` + `models\`) and a `cargo run` from the source tree
/// (`target\release\vcam.exe`, data two or three levels up) work without configuration.
pub fn search_roots() -> Vec<PathBuf> {
    let start = exe_dir();
    start.ancestors().take(6).map(|p| p.to_path_buf()).collect()
}

/// First existing `root/rel` along [`search_roots`].
pub fn find_near_exe(rel: &str) -> Option<PathBuf> {
    search_roots().into_iter().map(|r| r.join(rel)).find(|p| p.exists())
}

/// Python's `round()`: round half to even, returned as an integer.
pub fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Map any displayable error into `anyhow` (used for error types that are not
/// `Send + Sync`, such as some of the ONNX Runtime binding's).
pub fn disp<T, E: Display>(r: Result<T, E>) -> anyhow::Result<T> {
    r.map_err(|e| anyhow::anyhow!("{}", e))
}

/// File name of a path, for messages.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}
