//! The matting engine (RobustVideoMatting) and the ONNX Runtime device choice.

pub mod ort_env;
pub mod rvm;

use crate::imgproc::{self, Bgr, Interp};
use crate::models;
use crate::vlog;
use anyhow::{anyhow, Result};
use ort::ep::ExecutionProvider;
use rayon::prelude::*;

/// `--device` choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Auto,
    Cpu,
    Cuda,
    Dml,
}

impl Device {
    pub fn key(self) -> &'static str {
        match self {
            Device::Auto => "auto",
            Device::Cpu => "cpu",
            Device::Cuda => "cuda",
            Device::Dml => "dml",
        }
    }
}

/// The ONNX Runtime execution provider a session runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Cpu,
    Cuda,
    Dml,
}

impl Provider {
    /// The name ONNX Runtime uses.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Cpu => "CPUExecutionProvider",
            Provider::Cuda => "CUDAExecutionProvider",
            Provider::Dml => "DmlExecutionProvider",
        }
    }

    /// Short name for the status line.
    pub fn short(self) -> &'static str {
        match self {
            Provider::Cpu => "CPU",
            Provider::Cuda => "CUDA",
            Provider::Dml => "DirectML",
        }
    }
}

fn provider_available(p: Provider) -> bool {
    if ort_env::ort_env().is_err() {
        return false;
    }
    match p {
        Provider::Cpu => true,
        Provider::Cuda => ort::ep::CUDA::is_available().unwrap_or(false),
        Provider::Dml => ort::ep::DirectML::is_available().unwrap_or(false),
    }
}

/// GPU providers this ONNX Runtime build offers, in trial order.
pub fn gpu_providers() -> Vec<Provider> {
    [Provider::Dml, Provider::Cuda].into_iter().filter(|p| provider_available(*p)).collect()
}

/// Device choices this install can offer (for the UI dropdown).
pub fn available_devices() -> Vec<Device> {
    let mut out = vec![Device::Auto, Device::Cpu];
    if provider_available(Provider::Cuda) {
        out.push(Device::Cuda);
    }
    if provider_available(Provider::Dml) {
        out.push(Device::Dml);
    }
    out
}

/// What an engine needs to know (a subset of the worker configuration).
#[derive(Clone, Debug)]
pub struct EngineParams {
    pub model: Option<String>,
    /// RVM's downsample_ratio, the Quality dial (`None` = 0.375, 0.25 at >= 1080p).
    pub downsample: Option<f32>,
    pub threads: Option<usize>,
    pub device: Device,
}

pub trait Engine {
    /// Returns the alpha matte (`frame.w * frame.h`, values in [0, 1]) and, for RVM,
    /// a colour-decontaminated foreground.
    fn process(&mut self, frame: &Bgr) -> Result<(Vec<f32>, Option<Bgr>)>;
    /// Live quality/speed dial (RVM's downsample_ratio).
    fn set_downsample(&mut self, _dsr: f32) {}
    /// Shown in the status line ("CPU", "CUDA", "DirectML").
    fn provider_label(&self) -> String;
    /// Non-empty when a forced device failed and a fallback is in use.
    fn note(&self) -> String;
}

/// Keep only the single largest person blob in a soft alpha matte; suppress the rest
/// while preserving the kept subject's soft edges. Connected components run at half
/// resolution (blobs are large).
pub fn keep_largest(alpha: &mut [f32], w: usize, h: usize) {
    let half = w.max(h) > 640;
    let (sw, sh) = if half { (w / 2, h / 2) } else { (w, h) };
    if sw == 0 || sh == 0 {
        return;
    }
    let binary: Vec<u8> = if half {
        let small = imgproc::resize_f32(alpha, w, h, sw, sh, Interp::Linear);
        small.par_iter().map(|&a| if a > 0.5 { 1u8 } else { 0u8 }).collect()
    } else {
        alpha.par_iter().map(|&a| if a > 0.5 { 1u8 } else { 0u8 }).collect()
    };
    let biggest = match imgproc::largest_component(&binary, sw, sh) {
        Some(m) => m,
        None => return,
    };
    let keep = imgproc::dilate(&biggest, sw, sh, &imgproc::ellipse_half_widths(11));
    let keep = if half { imgproc::resize_nearest_u8(&keep, sw, sh, w, h) } else { keep };
    alpha.par_iter_mut().zip(keep.par_iter()).for_each(|(a, &k)| {
        if k == 0 {
            *a = 0.0;
        }
    });
}

/// Build the RobustVideoMatting engine for `width x height` camera images.
pub fn make_engine(p: &EngineParams, width: usize, height: usize) -> Result<Box<dyn Engine>> {
    let model_name = models::resolve_model(p.model.as_deref());
    let engine = rvm::RvmEngine::new(width, height, p, model_name)
        .map_err(|e| anyhow!("RobustVideoMatting could not start: {}", e))?;
    let on_cpu = engine.provider == Provider::Cpu;
    let accel = if on_cpu { format!("CPU x{} threads", engine.threads) } else { engine.provider.name().to_string() };
    vlog!(
        "[vcam] Engine: RobustVideoMatting [{}] (downsample_ratio={:.2}, {})",
        model_name, engine.dsr, accel
    );
    if let Some((cpu_ms, gpu_ms, gpu)) = engine.bench {
        // a GPU was time-trialled
        vlog!(
            "[vcam] Provider trial: CPU {:.0} ms/frame vs {} {:.0} ms/frame -> using {}.",
            cpu_ms,
            gpu.name(),
            gpu_ms,
            if on_cpu { "CPU (faster here)" } else { "GPU" }
        );
    } else if on_cpu && p.device == Device::Auto {
        vlog!(
            "[vcam] Running on CPU. With a powerful (discrete) NVIDIA GPU, the GPU build of \
             ONNX Runtime plus the CUDA 12 / cuDNN 9 DLLs can speed matting up."
        );
    }
    Ok(Box::new(engine))
}
