//! vcam.exe — the control panel.
//!
//! Run it, then in Google Meet: Settings -> Video -> Camera -> "OBS Virtual Camera".
//! `vcam.exe --selftest` exercises the bundle (ONNX Runtime + RVM model) without a
//! camera or window and writes the result to `vcam_selftest.txt`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use vcam::engine::rvm::RvmEngine;
use vcam::engine::{Device, Engine, EngineParams};
use vcam::imgproc::Bgr;
use vcam::models::DEFAULT_MODEL;

/// Exercise the bundle without a camera/GUI, writing the result to vcam_selftest.txt
/// (handy because the windowed build has no console).
fn selftest() -> i32 {
    let mut lines: Vec<String> = Vec::new();
    let mut ok = true;
    let params = EngineParams {
        model: Some(DEFAULT_MODEL.to_string()),
        downsample: None,
        threads: None,
        device: Device::Auto,
    };
    let result = (|| -> anyhow::Result<String> {
        let mut eng = RvmEngine::new(640, 480, &params, DEFAULT_MODEL)?;
        // pseudo-random frame (xorshift), no extra dependency needed
        let mut frame = Bgr::new(640, 480);
        let mut s: u32 = 0x1234_5678;
        for v in frame.data.iter_mut() {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *v = (s >> 24) as u8;
        }
        let (alpha, fg) = eng.process(&frame)?;
        let fg_len = fg.map(|f| f.data.len()).unwrap_or(0);
        Ok(format!(
            "RVM OK: alpha[{}] fg[{}] dsr={} provider={} note={:?}",
            alpha.len(),
            fg_len,
            eng.dsr,
            eng.provider.name(),
            eng.note
        ))
    })();
    match result {
        Ok(line) => lines.push(line),
        Err(exc) => {
            ok = false;
            lines.push(format!("RVM FAIL: {}", exc));
        }
    }
    lines.push(format!("virtualcam_installed={}", vcam::virtualcam::virtualcam_installed()));
    let report = format!("{}\n{}", if ok { "PASS" } else { "FAIL" }, lines.join("\n"));
    let _ = std::fs::write("vcam_selftest.txt", &report);
    println!("{}", report);
    if ok {
        0
    } else {
        1
    }
}

fn main() {
    vcam::util::init_thread_pool();
    // The windowed build has no console: keep every [vcam] message in the log file.
    vcam::logging::init_file_log();
    if std::env::args().any(|a| a == "--selftest") {
        std::process::exit(selftest());
    }
    // Keep matting at full speed when the window is minimized or covered.
    vcam::power::disable_power_throttling();
    if let Err(e) = vcam::ui::app::run() {
        vcam::vlog!("[vcam] UI error: {}", e);
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("vcam")
            .set_description(format!("vcam could not start its window:\n\n{}", e))
            .show();
        std::process::exit(1);
    }
}
