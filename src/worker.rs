//! Worker — runs the capture -> matte -> composite -> virtual-cam loop in a thread so a
//! GUI (or the CLI) can start/stop it and change options live.

use crate::background::{blur_background_image, load_background_image, solid_color_background};
use crate::camera::FrameGrabber;
use crate::color::{apply_dell_color, DEFAULT_COLOR_PRESET};
use crate::compositor::Compositor;
use crate::engine::rvm::default_dsr;
use crate::engine::{make_engine, Device, EngineParams};
use crate::geometry::{crop_pixels, fit_rect, normalize_rotation, ZOOM_MAX, ZOOM_MIN};
use crate::imgproc::{self, Bgr, Interp};
use crate::models::DEFAULT_MODEL;
use crate::pipeline::{Job, OutputPipeline, OutputShared};
use crate::util::file_name;
use crate::virtualcam::VirtualCam;
use crate::vlog;
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgMode {
    /// Virtual background OFF: the raw camera frame on a black canvas.
    Off,
    /// Blur the real background.
    Blur,
    /// A background picture (falls back to blur when it cannot be read).
    Image,
    /// A solid colour.
    Color,
}

/// What to do about the virtual camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VcamMode {
    /// Fail when it cannot be opened (GUI).
    Required,
    /// Warn and continue preview-only (CLI default).
    Optional,
    /// Never open it (`--no-virtualcam`).
    Disabled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub camera: usize,
    pub width: u32,
    pub height: u32,
    pub out_width: Option<u32>,
    pub out_height: Option<u32>,
    pub fps: u32,
    pub mode: BgMode,
    pub bg_path: Option<PathBuf>,
    /// BGR
    pub color: (u8, u8, u8),
    pub blur_image: bool,
    pub blur_strength: f32,
    pub mirror: bool,
    pub multi_person: bool,
    pub downsample: Option<f32>,
    pub threads: Option<usize>,
    pub device: Device,
    pub model: Option<String>,
    /// Normalized (x0, y0, x1, y1) of the camera frame to use, or `None` for all of it.
    pub crop: Option<[f32; 4]>,
    pub color_preset: String,
    /// brightness, contrast, warmth, tint, saturation (used by "Custom Color")
    pub color_values: [i32; 5],
    /// degrees clockwise
    pub rotation: u32,
    pub zoom: f32,
    /// Matte threshold, 0 = off (see `Compositor::set_threshold`). Live.
    pub threshold: f32,
    pub vcam: VcamMode,
    /// Run the webcam and the model only while a video app is using the virtual camera.
    /// In between, the virtual camera stays published with a black frame (standby).
    /// Needs the virtual camera (ignored with `VcamMode::Disabled`).
    pub on_demand: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            camera: 0,
            width: 1280,
            height: 720,
            out_width: None,
            out_height: None,
            fps: 30,
            mode: BgMode::Image,
            bg_path: None,
            color: (64, 177, 0),
            blur_image: false,
            blur_strength: 1.0,
            mirror: false,
            multi_person: false,
            downsample: None,
            threads: None,
            device: Device::Auto,
            model: Some(DEFAULT_MODEL.to_string()),
            crop: None,
            color_preset: DEFAULT_COLOR_PRESET.to_string(),
            color_values: [0; 5],
            rotation: 0,
            zoom: 1.0,
            threshold: 0.0,
            vcam: VcamMode::Required,
            on_demand: false,
        }
    }
}

impl Config {
    /// True when going from `self` to `other` needs the capture / engine / virtual-cam
    /// pipeline rebuilt (vs applied in place each frame). Everything else (mirror,
    /// downsample, one-person, background, colour, zoom) applies live on the next frame.
    fn needs_restart(&self, other: &Config) -> bool {
        self.crop != other.crop
            || self.camera != other.camera
            || self.width != other.width
            || self.height != other.height
            || self.out_width != other.out_width
            || self.out_height != other.out_height
            || self.fps != other.fps
            || self.threads != other.threads
            || self.device != other.device
            || self.rotation != other.rotation
            || self.model != other.model
            || self.vcam != other.vcam
            || self.on_demand != other.on_demand
    }

    /// True when this config runs the camera only while an app is watching.
    fn standby_enabled(&self) -> bool {
        self.on_demand && self.vcam != VcamMode::Disabled
    }

    /// The virtual-camera size in on-demand mode. It must be known before the webcam is
    /// opened (the queue stays open across sessions), so it is the output size when one
    /// is set, else the requested capture size.
    fn on_demand_size(&self) -> (usize, usize) {
        let w = self.out_width.filter(|v| *v > 0).unwrap_or(self.width) as usize;
        let h = self.out_height.filter(|v| *v > 0).unwrap_or(self.height) as usize;
        ((w & !1).max(2), (h & !1).max(2))
    }

    fn engine_params(&self) -> EngineParams {
        EngineParams {
            model: self.model.clone(),
            downsample: self.downsample,
            threads: self.threads,
            device: self.device,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Stopped,
    /// On-demand mode, no app watching: the virtual camera is published, the webcam
    /// and the model are off.
    Standby,
    Starting,
    Running,
    Error,
}

/// On-demand mode: how long no app may be watching before the webcam and the model are
/// turned off. Rides out the brief drops of the consumer count while an app streams.
const IDLE_STOP: Duration = Duration::from_secs(5);

/// A snapshot of the worker for the UI.
#[derive(Clone, Debug)]
pub struct Status {
    pub state: RunState,
    pub running: bool,
    pub error: Option<String>,
    pub fps: f32,
    /// Name of the virtual camera device in use.
    pub device: Option<String>,
    /// Set when the image background fell back to blur.
    pub bg_warning: Option<String>,
    pub provider: String,
    pub device_note: String,
    /// (w, h) of the full camera frame (after rotation).
    pub raw_size: Option<(usize, usize)>,
    /// (x, y, w, h) of the camera inside the output, or `None`.
    pub cam_rect: Option<(usize, usize, usize, usize)>,
    /// (w, h) the camera delivers after rotation and crop.
    pub cap_size: Option<(usize, usize)>,
    /// (w, h) of the frame sent to the virtual camera.
    pub out_size: Option<(usize, usize)>,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            state: RunState::Stopped,
            running: false,
            error: None,
            fps: 0.0,
            device: None,
            bg_warning: None,
            provider: String::new(),
            device_note: String::new(),
            raw_size: None,
            cam_rect: None,
            cap_size: None,
            out_size: None,
        }
    }
}

struct Inner {
    cfg: Mutex<Config>,
    status: Mutex<Status>,
    stop: AtomicBool,
    restart: AtomicBool,
    bg_dirty: AtomicBool,
    /// newest FULL camera frame (after rotation), for the crop tab
    raw: Mutex<Option<Arc<Bgr>>>,
    output: Mutex<Option<Arc<OutputShared>>>,
}

pub struct VCamWorker {
    inner: Arc<Inner>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Default for VCamWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl VCamWorker {
    pub fn new() -> Self {
        VCamWorker {
            inner: Arc::new(Inner {
                cfg: Mutex::new(Config::default()),
                status: Mutex::new(Status::default()),
                stop: AtomicBool::new(false),
                restart: AtomicBool::new(false),
                bg_dirty: AtomicBool::new(false),
                raw: Mutex::new(None),
                output: Mutex::new(None),
            }),
            done: Mutex::new(None),
        }
    }

    // ---- public, thread-safe ----

    /// Change settings. A pipeline-shape change while live reconnects automatically
    /// (no manual OFF/ON).
    pub fn configure<F: FnOnce(&mut Config)>(&self, f: F) {
        let restart = {
            let mut cfg = self.inner.cfg.lock().unwrap();
            let old = cfg.clone();
            f(&mut *cfg);
            old.needs_restart(&*cfg)
        };
        if restart && self.status().running {
            self.inner.restart.store(true, Ordering::SeqCst);
        }
    }

    pub fn config(&self) -> Config {
        self.inner.cfg.lock().unwrap().clone()
    }

    pub fn set_background(&self, mode: BgMode, path: Option<PathBuf>, color: Option<(u8, u8, u8)>, blur_image: Option<bool>) {
        {
            let mut cfg = self.inner.cfg.lock().unwrap();
            cfg.mode = mode;
            if let Some(p) = path {
                cfg.bg_path = Some(p);
            }
            if let Some(c) = color {
                cfg.color = c;
            }
            if let Some(b) = blur_image {
                cfg.blur_image = b;
            }
        }
        self.inner.bg_dirty.store(true, Ordering::SeqCst);
    }

    /// Blur intensity for both the live blur mode and a blurred image background.
    /// Rebuilds the compositor (like `set_background`) rather than applying per-frame,
    /// since the sigma is baked into the compositor.
    pub fn set_blur_strength(&self, strength: f32) {
        self.inner.cfg.lock().unwrap().blur_strength = strength;
        self.inner.bg_dirty.store(true, Ordering::SeqCst);
    }

    /// Live quality/speed balance (RVM downsample_ratio). Takes effect on the next
    /// frame while running; also remembered for the next start.
    pub fn set_quality(&self, downsample: f32) {
        self.inner.cfg.lock().unwrap().downsample = Some(downsample);
    }

    pub fn status(&self) -> Status {
        let mut st = self.inner.status.lock().unwrap().clone();
        if let Some(out) = self.inner.output.lock().unwrap().as_ref() {
            if let Ok(r) = out.cam_rect.lock() {
                st.cam_rect = *r;
            }
        }
        st
    }

    /// The latest processed (output) frame, or `None` when stopped.
    pub fn latest_frame(&self) -> Option<Arc<Bgr>> {
        let out = self.inner.output.lock().unwrap().clone()?;
        let frame = out.latest.lock().ok()?.clone();
        frame
    }

    /// The newest full (uncropped, unmirrored) camera frame, or `None` when stopped.
    pub fn latest_raw_frame(&self) -> Option<Arc<Bgr>> {
        self.inner.raw.lock().unwrap().clone()
    }

    pub fn start(&self) {
        {
            let st = self.inner.status.lock().unwrap();
            if st.running || st.state == RunState::Starting {
                return;
            }
        }
        self.inner.stop.store(false, Ordering::SeqCst);
        {
            let mut st = self.inner.status.lock().unwrap();
            st.error = None;
            st.state = RunState::Starting;
        }
        let (done_tx, done_rx) = mpsc::channel::<()>();
        *self.done.lock().unwrap() = Some(done_rx);
        let inner = self.inner.clone();
        let spawned = thread::Builder::new().name("vcam-worker".into()).spawn(move || {
            worker_loop(&inner);
            let _ = done_tx.send(());
        });
        if let Err(e) = spawned {
            let mut st = self.inner.status.lock().unwrap();
            st.error = Some(format!("could not start the worker thread: {}", e));
            st.state = RunState::Error;
        }
    }

    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(rx) = self.done.lock().unwrap().take() {
            let _ = rx.recv_timeout(Duration::from_secs(3));
        }
    }
}

impl Drop for VCamWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- internal ----

/// `cam` = the (cropped) camera image, `full` = the uncropped camera frame that sets
/// the scale, so cropping never enlarges what is left.
fn build_compositor(
    cfg: &Config,
    cam: (usize, usize),
    full: (usize, usize),
    out: (usize, usize),
) -> (Compositor, Option<String>) {
    let (mut comp, warning) = build_compositor_for(cfg, cam, out);
    comp.set_scale_reference(full);
    (comp, warning)
}

fn build_compositor_for(cfg: &Config, cam: (usize, usize), out: (usize, usize)) -> (Compositor, Option<String>) {
    let (width, height) = out;
    match cfg.mode {
        BgMode::Off => {
            // Virtual background OFF: the raw camera frame on a black canvas.
            let bg = solid_color_background((0, 0, 0), width, height);
            return (Compositor::new(width, height, Some(bg), false, 1.0, cam), None);
        }
        BgMode::Color => {
            let bg = solid_color_background(cfg.color, width, height);
            return (Compositor::new(width, height, Some(bg), false, 1.0, cam), None);
        }
        _ => {}
    }
    let strength = cfg.blur_strength;
    let mut warning = None;
    if cfg.mode == BgMode::Image {
        if let Some(path) = cfg.bg_path.as_ref() {
            if !path.is_file() {
                warning = Some(format!("background image not found: {}", file_name(path)));
            } else {
                match load_background_image(path, width, height) {
                    Ok(mut bg) => {
                        if cfg.blur_image {
                            bg = blur_background_image(&bg, strength);
                        }
                        return (Compositor::new(width, height, Some(bg), false, 1.0, cam), None);
                    }
                    Err(_) => {
                        warning = Some(format!("can't read background image: {}", file_name(path)));
                    }
                }
            }
            // missing/unreadable image -> fall back to blur (warning shown in the UI)
        }
    }
    // blur (and fallback when no image chosen)
    (Compositor::new(width, height, None, true, strength, cam), warning)
}

fn worker_loop(inner: &Arc<Inner>) {
    // Outer loop: a session runs until stop or until a pipeline-shape change
    // (camera/resolution/engine) requests a reconnect, then rebuilds — so the user can
    // change any option without clicking OFF/ON.
    let mut failure: Option<String> = None;
    // On-demand mode keeps the virtual camera open between sessions: a video app holds
    // the queue while it streams, so closing and re-creating it would fail.
    let mut keep: Option<VirtualCam> = None;
    while !inner.stop.load(Ordering::SeqCst) {
        inner.restart.store(false, Ordering::SeqCst);
        let cfg = inner.cfg.lock().unwrap().clone();
        if cfg.standby_enabled() {
            match standby(inner, &cfg, &mut keep) {
                Ok(true) => {} // an app opened the camera
                Ok(false) => continue, // stop, or a setting changed
                Err(e) => {
                    failure = Some(e.to_string());
                    break;
                }
            }
        } else {
            keep = None;
        }
        inner.status.lock().unwrap().state = RunState::Starting;
        if let Err(e) = run_session(inner, &mut keep) {
            failure = Some(e.to_string());
            break;
        }
    }
    drop(keep); // closes the virtual camera
    *inner.raw.lock().unwrap() = None;
    *inner.output.lock().unwrap() = None;
    let mut st = inner.status.lock().unwrap();
    st.running = false;
    st.device = None;
    st.cam_rect = None;
    match failure {
        Some(msg) => {
            vlog!("[vcam] ERROR: {}", msg);
            st.error = Some(msg);
            st.state = RunState::Error;
        }
        None => st.state = RunState::Stopped,
    }
}

/// On-demand mode: publish the virtual camera with a black frame (no webcam, no model)
/// and wait until a video app opens it. `Ok(true)` = an app is watching; `Ok(false)` =
/// stop or a setting change was requested.
fn standby(inner: &Arc<Inner>, cfg: &Config, keep: &mut Option<VirtualCam>) -> Result<bool> {
    let (w, h) = cfg.on_demand_size();
    if keep.as_ref().is_some_and(|c| c.size() != (w, h)) {
        *keep = None; // output size changed while idle
    }
    if keep.is_none() {
        *keep = Some(VirtualCam::open(w, h, cfg.fps as f64)?);
    }
    let cam = keep.as_mut().expect("opened above");
    *inner.output.lock().unwrap() = None; // no preview while idle
    *inner.raw.lock().unwrap() = None;
    {
        let mut st = inner.status.lock().unwrap();
        st.state = RunState::Standby;
        st.running = true;
        st.fps = 0.0;
        st.device = Some(cam.device().to_string());
        st.out_size = Some((w, h));
        st.cam_rect = None;
    }
    vlog!("[vcam] Standby: the webcam starts when an app uses '{}'", cam.device());
    let black = Bgr::new(w, h);
    let mut last_send: Option<Instant> = None;
    while !inner.stop.load(Ordering::SeqCst) && !inner.restart.load(Ordering::SeqCst) {
        if cam.consumers() > 0 {
            vlog!("[vcam] An app opened the virtual camera -> starting");
            return Ok(true);
        }
        // One frame a second keeps the camera visible to apps (OBS repeats it).
        if last_send.map_or(true, |t| t.elapsed() >= Duration::from_secs(1)) {
            cam.send(&black);
            last_send = Some(Instant::now());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Ok(false)
}

fn run_session(inner: &Arc<Inner>, keep: &mut Option<VirtualCam>) -> Result<()> {
    let cfg = inner.cfg.lock().unwrap().clone();
    let on_demand = cfg.standby_enabled();
    let rot = normalize_rotation(cfg.rotation as i64)?;
    let grabber = FrameGrabber::start(cfg.camera, cfg.width, cfg.height, cfg.fps, rot)?;
    let (raw_w, raw_h) = grabber.size(); // the full camera frame, 90/270 already swapped
    let crop_px = crop_pixels(cfg.crop, raw_w, raw_h, 16);
    // only this part of the frame is used from here on
    let (width, height) = match crop_px {
        Some((x0, y0, x1, y1)) => (x1 - x0, y1 - y0),
        None => (raw_w, raw_h),
    };
    let fps_note = if grabber.reported_fps() > 0.0 {
        format!(" (reports {:.0} fps)", grabber.reported_fps())
    } else {
        String::new()
    };
    vlog!("[vcam] Capturing {}x{} from camera {}{}", raw_w, raw_h, cfg.camera, fps_note);

    let mut engine = make_engine(&cfg.engine_params(), width, height)?;

    // Output canvas: independent of the camera size. The FULL camera frame would fit
    // inside it (never clipped); a crop keeps the size it has in that full frame, so
    // cropping only drops pixels and never enlarges the rest. Zoom scales from there.
    // The rest of the canvas is filled with the background.
    // On demand, the size is fixed up front (the virtual camera is already open).
    let demand_size = if on_demand { Some(cfg.on_demand_size()) } else { None };
    let out_set = demand_size.is_some()
        || (cfg.out_width.filter(|v| *v > 0).is_some() && cfg.out_height.filter(|v| *v > 0).is_some());
    let out_w = demand_size.map(|s| s.0).unwrap_or_else(|| cfg.out_width.map(|v| v as usize).filter(|v| *v > 0).unwrap_or(width));
    let out_h = demand_size.map(|s| s.1).unwrap_or_else(|| cfg.out_height.map(|v| v as usize).filter(|v| *v > 0).unwrap_or(height));
    if (out_w, out_h) != (width, height) {
        vlog!("[vcam] Scaling output to {}x{}", out_w, out_h);
    }
    // With no output size given, the output IS the crop's size: fit the crop to it.
    let scale_ref = if out_set { (raw_w, raw_h) } else { (width, height) };
    let (compositor, warning) = build_compositor(&cfg, (width, height), scale_ref, (out_w, out_h));
    inner.bg_dirty.store(false, Ordering::SeqCst);
    let mut pending_compositor = Some(compositor);

    // Reuse the virtual camera standby kept open, when it has the right size.
    let reused = match keep.take() {
        Some(c) if c.size() == (out_w, out_h) && cfg.vcam != VcamMode::Disabled => Some(c),
        _ => None,
    };
    let cam = match cfg.vcam {
        _ if reused.is_some() => reused,
        VcamMode::Disabled => None,
        VcamMode::Required => Some(VirtualCam::open(out_w, out_h, cfg.fps as f64)?),
        VcamMode::Optional => match VirtualCam::open(out_w, out_h, cfg.fps as f64) {
            Ok(c) => Some(c),
            Err(exc) => {
                vlog!("[vcam] WARNING: could not start a virtual camera: {}", exc);
                vlog!("[vcam] Install OBS Studio (provides 'OBS Virtual Camera') and retry.");
                vlog!("[vcam] Continuing in preview-only mode.");
                None
            }
        },
    };
    let device = cam.as_ref().map(|c| c.device().to_string());
    if let Some(d) = device.as_ref() {
        vlog!("[vcam] Virtual camera live -> '{}' (select it in your video app)", d);
    }

    // Compositing + cam.send run on a worker thread, overlapping the next frame's
    // inference (identical output, higher fps).
    let shared = Arc::new(OutputShared::default());
    *inner.output.lock().unwrap() = Some(shared.clone());
    let mut pipeline = OutputPipeline::new(cam, shared)?;

    {
        let mut st = inner.status.lock().unwrap();
        st.raw_size = Some((raw_w, raw_h));
        st.cap_size = Some((width, height));
        st.out_size = Some((out_w, out_h));
        st.provider = engine.provider_label();
        st.device_note = engine.note();
        st.bg_warning = warning;
        st.device = device;
        st.fps = 0.0;
        st.running = true;
        st.state = RunState::Running;
    }

    let mut frames: u64 = 0;
    let t0 = Instant::now();
    let mut last_seq: u64 = 0;
    let mut ones: Vec<f32> = Vec::new();
    let mut last_watched = Instant::now();
    let result = (|| -> Result<()> {
        while !inner.stop.load(Ordering::SeqCst) && !inner.restart.load(Ordering::SeqCst) {
            if let Some(e) = pipeline.error() {
                bail!("{}", e);
            }
            if on_demand {
                if pipeline.shared.consumers.load(Ordering::Relaxed) > 0 {
                    last_watched = Instant::now();
                } else if last_watched.elapsed() >= IDLE_STOP {
                    vlog!("[vcam] No app is using the virtual camera -> standby");
                    break;
                }
            }
            let (seq, frame) = grabber.read();
            let raw = match frame {
                Some(f) if seq != last_seq => f,
                _ => {
                    if !grabber.running() {
                        bail!("camera stopped delivering frames");
                    }
                    thread::sleep(Duration::from_millis(2)); // no new frame yet
                    continue;
                }
            };
            last_seq = seq;
            *inner.raw.lock().unwrap() = Some(raw.clone()); // full frame, for the crop tab

            if inner.bg_dirty.swap(false, Ordering::SeqCst) {
                let cfg_now = inner.cfg.lock().unwrap().clone();
                let (c, warning) = build_compositor(&cfg_now, (width, height), scale_ref, (out_w, out_h));
                pending_compositor = Some(c);
                inner.status.lock().unwrap().bg_warning = warning;
            }
            let (mirror, want_dsr, multi, bg_off, preset, color_values, zoom, threshold) = {
                let c = inner.cfg.lock().unwrap();
                (
                    c.mirror,
                    c.downsample,
                    c.multi_person,
                    c.mode == BgMode::Off,
                    c.color_preset.clone(),
                    c.color_values,
                    c.zoom,
                    c.threshold,
                )
            };

            // the rest of the camera frame is dropped
            let mut frame: Bgr = match crop_px {
                Some((x0, y0, x1, y1)) => imgproc::crop(&raw, x0, y0, x1, y1),
                None => (*raw).clone(),
            };
            if mirror {
                frame = imgproc::flip_horizontal(&frame);
            }
            // Dell-style colour management: after the mirror, before matting.
            apply_dell_color(&mut frame, &preset, color_values);
            // Matte at the size the person is shown. With zoom > 1 (or an output smaller
            // than the camera) the compositor shrinks the result anyway, so shrink the
            // frame first: the same output pixels for much less work (GTX 1050 Ti, dsr
            // 0.375: 1280x720 49 ms, 854x480 24 ms), and no resampling blur on the
            // matte afterwards. downsample_ratio is scaled up by the same factor so the
            // coarse pass keeps the resolution the Quality setting asks for.
            let base_dsr = want_dsr.unwrap_or_else(|| default_dsr(width, height));
            let (_, _, rw, rh) = fit_rect(width, height, scale_ref, out_w, out_h, true, zoom.clamp(ZOOM_MIN, ZOOM_MAX));
            let (tw, th) = (rw.max(16) as usize, rh.max(16) as usize);
            let (frame, dsr) = if !bg_off && tw < frame.w && th < frame.h {
                let small = imgproc::resize_bgr(&frame, tw, th, Interp::Area);
                (small, (base_dsr * width as f32 / tw as f32).min(1.0))
            } else {
                (frame, base_dsr)
            };
            engine.set_downsample(dsr); // RVM quality dial (live)

            let (alpha, fg) = if bg_off {
                // Virtual background OFF: no matting at all - the raw frame goes to the
                // compositor with a fully opaque matte, so it shows exactly as captured
                // (black everywhere else).
                if ones.len() != frame.w * frame.h {
                    ones = vec![1.0f32; frame.w * frame.h];
                }
                (ones.clone(), None)
            } else {
                engine.process(&frame)?
            };
            pipeline.submit(Job {
                compositor: pending_compositor.take(),
                source: fg.unwrap_or(frame),
                alpha,
                zoom,
                threshold,
                single: !multi && !bg_off,
            })?;
            frames += 1;
            if frames % 15 == 0 {
                let fps = frames as f64 / t0.elapsed().as_secs_f64().max(1e-6);
                inner.status.lock().unwrap().fps = fps as f32;
                if frames % 120 == 0 {
                    vlog!("[vcam] ~{:.1} fps", fps);
                }
            }
        }
        Ok(())
    })();

    grabber.stop();
    // Drain + join the output thread. On demand, keep the virtual camera open for the
    // next standby; otherwise dropping it closes it.
    let cam_back = pipeline.close();
    if on_demand {
        *keep = cam_back;
    }
    drop(engine);
    {
        let mut st = inner.status.lock().unwrap();
        st.running = false;
        st.device = None;
    }
    *inner.raw.lock().unwrap() = None;
    result.map_err(|e| anyhow!("{}", e))
}
