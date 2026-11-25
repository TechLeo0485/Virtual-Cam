//! vcam-cli — real-time webcam background replacement from the command line.
//!
//! Examples
//! --------
//!     # Blur the real background (default when no --background is given)
//!     vcam-cli
//!
//!     # Replace background with an image
//!     vcam-cli --background backgrounds\beach.jpg
//!
//!     # Solid green screen, mirrored, camera #1
//!     vcam-cli --color 0,177,64 --camera 1 --mirror
//!
//!     # Headless (no preview window), just feed the virtual cam
//!     vcam-cli --background office.png --no-preview
//!
//! Press 'q' (or Esc) in the preview window to quit, or Ctrl+C in the terminal.

use clap::Parser;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vcam::engine::Device;
use vcam::imgproc;
use vcam::models;
use vcam::worker::{BgMode, RunState, VCamWorker, VcamMode};

fn parse_color(s: &str) -> Result<(u8, u8, u8), String> {
    vcam::background::parse_color(s)
}

fn parse_device(s: &str) -> Result<Device, String> {
    match s {
        "auto" => Ok(Device::Auto),
        "cpu" => Ok(Device::Cpu),
        "cuda" => Ok(Device::Cuda),
        "dml" => Ok(Device::Dml),
        _ => Err("choose from auto, cpu, cuda, dml".to_string()),
    }
}

fn parse_model(s: &str) -> Result<String, String> {
    if models::spec(s).is_some() {
        Ok(s.to_string())
    } else {
        Err(format!("choose from {}", models::model_names().join(", ")))
    }
}

fn parse_rotation(s: &str) -> Result<u32, String> {
    match s {
        "0" => Ok(0),
        "90" => Ok(90),
        "180" => Ok(180),
        "270" => Ok(270),
        _ => Err("choose from 0, 90, 180, 270".to_string()),
    }
}

/// Real-time webcam virtual background (RobustVideoMatting)
/// published as a system virtual camera (OBS Virtual Camera).
#[derive(Parser, Debug)]
#[command(name = "vcam-cli", version)]
struct Args {
    /// background image to composite behind you
    #[arg(short = 'b', long, value_name = "PATH", conflicts_with_all = ["blur", "color"])]
    background: Option<PathBuf>,

    /// blur the real background instead of replacing it
    #[arg(long, conflicts_with = "color")]
    blur: bool,

    /// solid color background, e.g. 0,177,64
    #[arg(long, value_name = "R,G,B", value_parser = parse_color)]
    color: Option<(u8, u8, u8)>,

    /// internal downsample_ratio 0-1 (default 0.375; lower = faster, softer hair)
    #[arg(long)]
    downsample: Option<f32>,

    /// ONNX Runtime CPU threads (default: min(8, cores); more can be slower)
    #[arg(long)]
    threads: Option<usize>,

    /// webcam device index
    #[arg(short = 'c', long, default_value_t = 0)]
    camera: usize,

    /// capture width
    #[arg(long, default_value_t = 1280)]
    width: u32,

    /// capture height
    #[arg(long, default_value_t = 720)]
    height: u32,

    /// virtual-cam output width (default: same as capture). Capture small/fast, upscale output.
    #[arg(long)]
    out_width: Option<u32>,

    /// virtual-cam output height (default: same as capture)
    #[arg(long)]
    out_height: Option<u32>,

    /// target frames per second
    #[arg(long, default_value_t = 30)]
    fps: u32,

    /// compute device: auto (benchmark and pick), cpu, cuda or dml
    #[arg(long, default_value = "auto", value_parser = parse_device)]
    device: Device,

    /// matting model (default: rvm_mobilenetv3_fp32)
    #[arg(long, value_parser = parse_model)]
    model: Option<String>,

    /// keep everyone in frame (default: keep only the single largest person)
    #[arg(long)]
    multi_person: bool,

    /// rotate the camera image clockwise by N degrees (applied before --mirror)
    #[arg(long, default_value = "0", value_parser = parse_rotation)]
    rotate: u32,

    /// zoom 0.5-2.0: 1 = actual size (a crop keeps its size), above 1 shrinks the camera image to 1/N (bottom-centred on the background), below 1 enlarges it
    #[arg(long, default_value_t = 1.0)]
    zoom: f32,

    /// matte threshold 0-0.9 (0 = off): matte values above it become solid person, below it background
    #[arg(long, default_value_t = 0.0)]
    threshold: f32,

    /// horizontally flip (selfie view)
    #[arg(long)]
    mirror: bool,

    /// don't open the preview window
    #[arg(long)]
    no_preview: bool,

    /// preview only; don't open a virtual camera
    #[arg(long)]
    no_virtualcam: bool,

    /// list the cameras and exit
    #[arg(long)]
    list_cameras: bool,

    /// make sure every model exists in DIR (copying or downloading), then exit
    #[arg(long, value_name = "DIR")]
    download_models: Option<PathBuf>,
}

/// The preview window: shows the worker's latest finished frame.
struct PreviewApp {
    worker: Arc<VCamWorker>,
    quit: Arc<AtomicBool>,
    tex: Option<egui::TextureHandle>,
    last: Instant,
}

impl eframe::App for PreviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let st = self.worker.status();
        let key_quit = ctx.input(|i| i.key_pressed(egui::Key::Q) || i.key_pressed(egui::Key::Escape));
        if key_quit || self.quit.load(Ordering::SeqCst) || st.state == RunState::Error || st.state == RunState::Stopped {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.last.elapsed() >= Duration::from_millis(33) {
            self.last = Instant::now();
            if let Some(frame) = self.worker.latest_frame() {
                let rgb = imgproc::bgr_to_rgb_bytes(&frame.data);
                let img = egui::ColorImage::from_rgb([frame.w, frame.h], &rgb);
                if let Some(tex) = self.tex.as_mut() {
                    tex.set(img, egui::TextureOptions::LINEAR);
                } else {
                    self.tex = Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR));
                }
            }
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
            let area = ui.available_rect_before_wrap();
            ui.painter().rect_filled(area, 0.0, egui::Color32::BLACK);
            match self.tex.as_ref() {
                Some(tex) => {
                    // scale to fit, keeping the aspect ratio
                    let size = tex.size_vec2();
                    let s = (area.width() / size.x).min(area.height() / size.y);
                    let rect = egui::Rect::from_center_size(area.center(), size * s);
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(tex.id(), rect, uv, egui::Color32::WHITE);
                }
                None => {
                    ui.painter().text(
                        area.center(),
                        egui::Align2::CENTER_CENTER,
                        "starting the camera...",
                        egui::FontId::proportional(16.0),
                        egui::Color32::GRAY,
                    );
                }
            }
        });
        ctx.request_repaint_after(Duration::from_millis(15));
    }
}

fn run(args: Args) -> anyhow::Result<i32> {
    if let Some(dir) = args.download_models.as_ref() {
        let n = models::bundle_all_models(dir)?;
        println!("[build] bundled {} model files into {}", n, dir.display());
        return Ok(0);
    }
    if args.list_cameras {
        for (i, name) in vcam::camera::list_cameras()?.iter().enumerate() {
            println!("{}: {}", i, name);
        }
        return Ok(0);
    }
    if let Some(bg) = args.background.as_ref() {
        if !bg.is_file() {
            anyhow::bail!("Background image not found: {}", bg.display());
        }
    }
    vcam::power::disable_power_throttling(); // full speed even when the window is hidden

    // Build the background choice (blur recomputes from the live frame each tick).
    let mode = if args.blur {
        BgMode::Blur
    } else if args.background.is_some() {
        BgMode::Image
    } else if args.color.is_some() {
        BgMode::Color
    } else {
        println!("[vcam] No background specified -> blurring the real background.");
        BgMode::Blur // sensible default: blur the real background
    };
    if args.rotate != 0 {
        println!("[vcam] Rotating camera image {} deg clockwise", args.rotate);
    }

    let worker = Arc::new(VCamWorker::new());
    worker.configure(|c| {
        c.camera = args.camera;
        c.width = args.width;
        c.height = args.height;
        c.out_width = args.out_width;
        c.out_height = args.out_height;
        c.fps = args.fps;
        c.mode = mode;
        c.bg_path = args.background.clone();
        if let Some(col) = args.color {
            c.color = col;
        }
        c.blur_image = false;
        c.blur_strength = 1.0;
        c.mirror = args.mirror;
        c.multi_person = args.multi_person;
        c.downsample = args.downsample;
        c.threads = args.threads;
        c.device = args.device;
        c.model = args.model.clone();
        c.rotation = args.rotate;
        c.zoom = args.zoom;
        c.threshold = args.threshold.clamp(0.0, vcam::compositor::THRESHOLD_MAX);
        c.vcam = if args.no_virtualcam { VcamMode::Disabled } else { VcamMode::Optional };
    });

    let quit = Arc::new(AtomicBool::new(false));
    {
        let quit = quit.clone();
        let _ = ctrlc::set_handler(move || {
            quit.store(true, Ordering::SeqCst);
        });
    }
    worker.start();

    if args.no_preview {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let st = worker.status();
            if quit.load(Ordering::SeqCst) {
                println!("\n[vcam] Interrupted.");
                break;
            }
            if st.state == RunState::Error || st.state == RunState::Stopped {
                break;
            }
        }
    } else {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("vcam preview (q/Esc to quit)")
                .with_inner_size([960.0, 540.0]),
            ..Default::default()
        };
        let app = PreviewApp { worker: worker.clone(), quit: quit.clone(), tex: None, last: Instant::now() };
        eframe::run_native("vcam preview (q/Esc to quit)", options, Box::new(move |cc| {
            vcam::ui::theme::install(&cc.egui_ctx);
            Ok(Box::new(app))
        }))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    }

    let error = worker.status().error;
    worker.stop();
    if let Some(e) = error {
        anyhow::bail!("{}", e); // camera busy, model download failed, ...
    }
    Ok(0)
}

fn main() {
    vcam::util::init_thread_pool();
    let args = Args::parse();
    match run(args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}
