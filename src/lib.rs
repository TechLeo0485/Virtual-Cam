//! vcam — real-time webcam background replacement (Rust port of `vcam.py` / `vcam_ui.py`).
//!
//! Reads frames from a physical webcam (Media Foundation), isolates the person with a
//! RobustVideoMatting model run by ONNX Runtime, composites them over a background (image, blur or solid colour) and
//! publishes the result through the OBS Virtual Camera so any app can use it as a webcam.
//!
//! ```text
//! FrameGrabber (camera thread) -> crop -> engine.process() -> alpha matte
//!                                                               |
//! Compositor.render(frame, alpha) <- background / blur / colour / zoom
//!                                                               |
//! OutputPipeline (worker thread) -> VirtualCam.send() -> OBS Virtual Camera
//! ```

#[cfg(not(windows))]
compile_error!("vcam currently supports Windows only (Media Foundation + OBS Virtual Camera).");

pub mod logging;

pub mod background;
pub mod camera;
pub mod color;
pub mod compositor;
pub mod engine;
pub mod geometry;
pub mod imgproc;
pub mod models;
pub mod pipeline;
pub mod power;
pub mod ui;
pub mod update;
pub mod util;
pub mod virtualcam;
pub mod worker;
