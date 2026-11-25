//! RobustVideoMatting via ONNX Runtime — high-quality, temporally-coherent video
//! matting. The recurrent state (r1..r4, fed back every frame) is what keeps
//! hair/hands/edges stable under motion, so no extra temporal smoothing or
//! guided-filter hacks are needed. Returns a soft alpha and a colour-decontaminated
//! foreground (clean hair edges, no old-background colour bleed).

use super::ort_env::ort_env;
use super::{gpu_providers, Device, Engine, EngineParams, Provider};
use crate::imgproc::Bgr;
use crate::models;
use crate::util::disp;
use crate::vlog;
use anyhow::{anyhow, bail, Result};
use ort::ep::{self, ExecutionProvider};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::{DynValue, Tensor, TensorRef};
use rayon::prelude::*;
use std::path::Path;
use std::time::Instant;

/// A good ONNX Runtime intra-op thread count for RVM: enough to use the fast cores,
/// capped so a many-thread machine doesn't oversubscribe this small net (on a 14700
/// all-28-logical = 77 ms vs 8 = 48 ms).
/// RVM's recommended downsample_ratio for a `width x height` input when the user has
/// not chosen one: ~0.375, 0.25 for >= 1080p.
pub fn default_dsr(width: usize, height: usize) -> f32 {
    if width.max(height) >= 1600 {
        0.25
    } else {
        0.375
    }
}

pub fn rvm_thread_count() -> usize {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    n.clamp(2, 8)
}

/// Build an inference session on `provider` (the CPU provider is always the fallback
/// for operators the GPU provider does not take).
pub fn build_session(model_path: &Path, provider: Provider, threads: usize) -> Result<Session> {
    let env = ort_env()?;
    let mut b = disp(Session::builder(env))?;
    b = disp(b.with_intra_threads(threads))?;
    b = disp(b.with_optimization_level(GraphOptimizationLevel::All))?;
    match provider {
        Provider::Cpu => {}
        Provider::Cuda => {
            // Before the CUDA context exists: wait for the GPU without spinning a CPU core.
            super::ort_env::cuda_blocking_sync();
            b = disp(b.with_memory_pattern(false))?;
            // Exhaustive: cuDNN times every convolution algorithm on the first run of each
            // input shape and keeps the fastest. Measured 1.5-3 ms/frame faster than
            // Heuristic on a GTX 1050 Ti (dsr 0.25-0.5); the cost is a slower first frame
            // after a size or Quality change.
            let cuda = ep::CUDA::default().with_conv_algorithm_search(ep::cuda::ConvAlgorithmSearch::Exhaustive);
            disp(cuda.register(&mut b))?;
        }
        Provider::Dml => {
            b = disp(b.with_memory_pattern(false))?; // required by the DirectML provider
            let dml = ep::DirectML::default();
            disp(dml.register(&mut b))?;
        }
    }
    disp(b.commit_from_file(model_path))
}

fn zero_state() -> Result<Vec<DynValue>> {
    let mut rec = Vec::with_capacity(4);
    for _ in 0..4 {
        let t = disp(Tensor::<f32>::from_array(([1usize, 1, 1, 1], vec![0.0f32])))?;
        rec.push(t.into_dyn());
    }
    Ok(rec)
}

fn dsr_tensor(dsr: f32) -> Result<DynValue> {
    Ok(disp(Tensor::<f32>::from_array(([1usize], vec![dsr])))?.into_dyn())
}

/// One inference. `src` is normalised RGB in NCHW order (`3 * h * w` floats). Updates
/// the recurrent state in place and returns (fgr, pha) when `want_output` is set.
fn run_once(
    sess: &mut Session,
    src: &[f32],
    w: usize,
    h: usize,
    rec: &mut Vec<DynValue>,
    dsr: &DynValue,
    want_output: bool,
) -> Result<Option<(Vec<f32>, Vec<f32>)>> {
    let src_t = disp(TensorRef::<f32>::from_array_view(([1usize, 3, h, w], src)))?;
    let mut outputs = disp(sess.run(ort::inputs! {
        "src" => src_t,
        "r1i" => &rec[0],
        "r2i" => &rec[1],
        "r3i" => &rec[2],
        "r4i" => &rec[3],
        "downsample_ratio" => dsr,
    }))?;
    let result = if want_output {
        let fgr = {
            let (_, data) = disp(outputs["fgr"].try_extract_tensor::<f32>())?;
            data.to_vec()
        };
        let pha = {
            let (_, data) = disp(outputs["pha"].try_extract_tensor::<f32>())?;
            data.to_vec()
        };
        if pha.len() != w * h || fgr.len() != 3 * w * h {
            bail!("unexpected RVM output size");
        }
        Some((fgr, pha))
    } else {
        None
    };
    let mut new_rec = Vec::with_capacity(4);
    for name in ["r1o", "r2o", "r3o", "r4o"] {
        new_rec.push(outputs.remove(name).ok_or_else(|| anyhow!("RVM output {} missing", name))?);
    }
    drop(outputs);
    *rec = new_rec;
    Ok(result)
}

/// Median ms/frame over a few dummy inferences. Fails if the provider builds a session
/// but can't actually run the model (e.g. some integrated/old GPUs).
fn bench_session(sess: &mut Session, width: usize, height: usize, n: usize) -> Result<f64> {
    let src = vec![0f32; 3 * width * height];
    let mut rec = zero_state()?;
    let dsr = dsr_tensor(0.375)?;
    // warm up (shader compile / arena alloc)
    run_once(sess, &src, width, height, &mut rec, &dsr, false)?;
    run_once(sess, &src, width, height, &mut rec, &dsr, false)?;
    let mut times = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        run_once(sess, &src, width, height, &mut rec, &dsr, false)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(times[times.len() / 2])
}

type Bench = Option<(f64, f64, Provider)>;

/// (session, provider, bench) on the fastest working provider.
///
/// Always falls back to the CPU, which is reliable and — for this small net on a
/// modern many-core CPU — frequently faster than an integrated/old GPU. A GPU is only
/// adopted when it both runs the model *and* clearly beats the CPU. `bench` is `None`
/// when no GPU was tried, else (cpu_ms, gpu_ms, gpu).
fn auto_session(model_path: &Path, threads: usize, width: usize, height: usize) -> Result<(Session, Provider, Bench)> {
    // An NVIDIA GPU that runs CUDA 12 beats the CPU on this net (even a GTX 1050 Ti:
    // 14-29 ms vs 23-54 ms per frame), so try CUDA first and skip the CPU trial. That
    // trial ran the model on 8 CPU threads at every start and every settings change
    // that rebuilds the pipeline. Weaker GPUs (DirectML) still race the CPU below.
    let mut cuda_failed = false;
    if gpu_providers().contains(&Provider::Cuda) {
        let trial = build_session(model_path, Provider::Cuda, threads)
            .and_then(|mut s| bench_session(&mut s, width, height, 2).map(|_| s)); // prove it runs
        match trial {
            Ok(sess) => return Ok((sess, Provider::Cuda, None)),
            Err(exc) => {
                vlog!("[vcam] {} failed: {}", Provider::Cuda.name(), exc);
                cuda_failed = true;
            }
        }
    }
    let mut cpu_sess = build_session(model_path, Provider::Cpu, threads)?;
    let gpus: Vec<Provider> = gpu_providers()
        .into_iter()
        .filter(|p| !(cuda_failed && *p == Provider::Cuda))
        .collect();
    if gpus.is_empty() {
        return Ok((cpu_sess, Provider::Cpu, None));
    }
    let cpu_ms = bench_session(&mut cpu_sess, width, height, 6)?;
    for prov in gpus {
        let trial = build_session(model_path, prov, threads)
            .and_then(|mut s| bench_session(&mut s, width, height, 6).map(|ms| (s, ms)));
        match trial {
            Ok((gpu_sess, gpu_ms)) => {
                if gpu_ms < cpu_ms * 0.9 {
                    // only switch if clearly (>10%) faster
                    return Ok((gpu_sess, prov, Some((cpu_ms, gpu_ms, prov))));
                }
                return Ok((cpu_sess, Provider::Cpu, Some((cpu_ms, gpu_ms, prov))));
            }
            Err(exc) => {
                vlog!("[vcam] {} failed: {}", prov.name(), exc);
            }
        }
    }
    Ok((cpu_sess, Provider::Cpu, None))
}

/// (session, provider, bench, note). `note` is non-empty when a forced device failed
/// and we fell back to the CPU.
fn select_session(
    model_path: &Path,
    threads: usize,
    width: usize,
    height: usize,
    device: Device,
) -> Result<(Session, Provider, Bench, String)> {
    let prov = match device {
        Device::Auto => {
            let (s, p, b) = auto_session(model_path, threads, width, height)?;
            return Ok((s, p, b, String::new()));
        }
        Device::Cpu => {
            return Ok((build_session(model_path, Provider::Cpu, threads)?, Provider::Cpu, None, String::new()));
        }
        Device::Cuda => Provider::Cuda,
        Device::Dml => Provider::Dml,
    };
    let forced = (|| -> Result<Session> {
        if !gpu_providers().contains(&prov) {
            bail!("{} is not available in this ONNX Runtime build", prov.name());
        }
        let mut sess = build_session(model_path, prov, threads)?;
        bench_session(&mut sess, width, height, 2)?; // prove it actually runs
        Ok(sess)
    })();
    match forced {
        Ok(sess) => Ok((sess, prov, None, String::new())),
        Err(exc) => {
            let msg: String = exc.to_string().chars().take(200).collect();
            let note = format!("{} failed, using CPU: {}", device.key().to_uppercase(), msg);
            vlog!("[vcam] {}", note);
            Ok((build_session(model_path, Provider::Cpu, threads)?, Provider::Cpu, None, note))
        }
    }
}

pub struct RvmEngine {
    sess: Session,
    pub provider: Provider,
    pub bench: Bench,
    pub note: String,
    pub threads: usize,
    pub model_name: String,
    /// downsample_ratio drives the coarse network's resolution and dominates speed.
    pub dsr: f32,
    dsr_value: DynValue,
    rec: Vec<DynValue>,
    src: Vec<f32>, // reused NCHW input buffer (no per-frame realloc)
    size: (usize, usize),
}

impl RvmEngine {
    pub fn new(width: usize, height: usize, p: &EngineParams, model: &str) -> Result<Self> {
        let threads = p.threads.filter(|t| *t > 0).unwrap_or_else(rvm_thread_count);
        // Pick the fastest WORKING execution provider. A GPU can be several times faster
        // than the CPU — but only a capable one: an integrated or old GPU is often
        // *slower* than a many-core CPU for this small net, and some GPU providers build
        // a session fine yet fail at run time. So we verify and time-trial the GPU and
        // only keep it when it clearly beats the CPU; otherwise we stay on the CPU.
        let model_path = models::ensure_named_model(model)?;
        let (sess, provider, bench, note) = select_session(&model_path, threads, width, height, p.device)?;
        // RVM's recommended ~0.375 (0.25 for >=1080p) keeps the refined full-res edges
        // sharp while running fast.
        let dsr = match p.downsample {
            Some(d) if d > 0.0 => d,
            _ => default_dsr(width, height),
        };
        Ok(RvmEngine {
            sess,
            provider,
            bench,
            note,
            threads,
            model_name: model.to_string(),
            dsr,
            dsr_value: dsr_tensor(dsr)?,
            rec: zero_state()?,
            src: Vec::new(),
            size: (0, 0),
        })
    }
}

impl Engine for RvmEngine {
    fn process(&mut self, frame: &Bgr) -> Result<(Vec<f32>, Option<Bgr>)> {
        let (w, h) = (frame.w, frame.h);
        let n = w * h;
        if self.size != (w, h) {
            // A different frame size invalidates the recurrent state.
            self.src = vec![0f32; 3 * n];
            self.rec = zero_state()?;
            self.size = (w, h);
        }
        // Fill the reused float32 NCHW buffer directly. RVM expects normalised RGB.
        {
            let (r_plane, rest) = self.src.split_at_mut(n);
            let (g_plane, b_plane) = rest.split_at_mut(n);
            r_plane
                .par_chunks_mut(w)
                .zip(g_plane.par_chunks_mut(w))
                .zip(b_plane.par_chunks_mut(w))
                .zip(frame.data.par_chunks(w * 3))
                .for_each(|(((r, g), b), px)| {
                    const K: f32 = 1.0 / 255.0;
                    for x in 0..r.len() {
                        b[x] = px[x * 3] as f32 * K;
                        g[x] = px[x * 3 + 1] as f32 * K;
                        r[x] = px[x * 3 + 2] as f32 * K;
                    }
                });
        }
        let out = run_once(&mut self.sess, &self.src, w, h, &mut self.rec, &self.dsr_value, true)?;
        let (fgr, alpha) = out.ok_or_else(|| anyhow!("RVM produced no output"))?;
        // Foreground RGB (3,H,W) float -> BGR uint8 HWC with a saturating scale.
        let mut fg = Bgr::new(w, h);
        let (fr, rest) = fgr.split_at(n);
        let (fgn, fb) = rest.split_at(n);
        fg.data
            .par_chunks_mut(w * 3)
            .zip(fr.par_chunks(w))
            .zip(fgn.par_chunks(w))
            .zip(fb.par_chunks(w))
            .for_each(|(((px, r), g), b)| {
                for x in 0..r.len() {
                    px[x * 3] = ((b[x] * 255.0).abs() + 0.5).min(255.0) as u8;
                    px[x * 3 + 1] = ((g[x] * 255.0).abs() + 0.5).min(255.0) as u8;
                    px[x * 3 + 2] = ((r[x] * 255.0).abs() + 0.5).min(255.0) as u8;
                }
            });
        Ok((alpha, Some(fg)))
    }

    /// Live-tune the quality/speed dial. downsample_ratio is a per-frame model input,
    /// so the change takes effect on the next frame with no session rebuild. The
    /// recurrent state is sized to the *old* coarse resolution, so it must be reset —
    /// otherwise the next inference fails on a shape mismatch (one frame re-initialises
    /// temporal coherence; imperceptible).
    fn set_downsample(&mut self, dsr: f32) {
        if (dsr - self.dsr).abs() < 1e-6 || !(dsr > 0.0) {
            return;
        }
        if let (Ok(v), Ok(rec)) = (dsr_tensor(dsr), zero_state()) {
            self.dsr = dsr;
            self.dsr_value = v;
            self.rec = rec;
        }
    }

    fn provider_label(&self) -> String {
        self.provider.short().to_string()
    }

    fn note(&self) -> String {
        self.note.clone()
    }
}
