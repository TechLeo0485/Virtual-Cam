//! Output pipeline: compositing + virtual-cam send on their own thread.

use crate::compositor::Compositor;
use crate::engine::keep_largest;
use crate::imgproc::Bgr;
use crate::virtualcam::VirtualCam;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// One frame's worth of work for the output thread.
pub struct Job {
    /// A new compositor to switch to (the first job must carry one; later jobs carry
    /// one only when the background changed).
    pub compositor: Option<Compositor>,
    /// The matted foreground (RVM) or the camera frame.
    pub source: Bgr,
    pub alpha: Vec<f32>,
    /// Live zoom, applied by the render thread.
    pub zoom: f32,
    /// Live matte threshold (0 = off), applied by the render thread.
    pub threshold: f32,
    /// Keep only the largest person. Done here rather than on the inference thread,
    /// so its 3-4 ms per frame overlap the next inference instead of adding to it.
    pub single: bool,
}

/// What the output thread publishes for the UI.
#[derive(Default)]
pub struct OutputShared {
    /// Most recently sent output frame (for preview); `None` until the first.
    pub latest: Mutex<Option<Arc<Bgr>>>,
    /// (x, y, w, h) of the camera inside the output, or `None` for an exact fit.
    pub cam_rect: Mutex<Option<(usize, usize, usize, usize)>>,
    pub error: Mutex<Option<String>>,
    /// Video apps reading the virtual camera (see `VirtualCam::consumers`), refreshed
    /// every few frames; 0 until the first check.
    pub consumers: AtomicUsize,
}

/// Runs compositing + virtual-cam send on a worker thread so that work overlaps the
/// *next* frame's inference instead of waiting behind it.
///
/// The matte is a serial wall — each frame feeds the recurrent state into the next, so
/// inferences can't overlap each other. But the compositing + send AFTER inference
/// don't depend on the next frame, so doing them on a second thread hides them behind
/// the next inference. Frames are handled in submit order and the output is identical
/// to doing it inline — a pure throughput win, costing one extra frame of latency
/// (imperceptible for a webcam).
pub struct OutputPipeline {
    tx: Option<SyncSender<Job>>,
    /// The output thread hands the virtual camera back here when it ends.
    done: Receiver<Option<VirtualCam>>,
    pub shared: Arc<OutputShared>,
}

impl OutputPipeline {
    pub fn new(cam: Option<VirtualCam>, shared: Arc<OutputShared>) -> Result<OutputPipeline> {
        // small queue => bounded latency + back-pressure
        let (tx, rx) = mpsc::sync_channel::<Job>(2);
        let (done_tx, done_rx) = mpsc::channel::<Option<VirtualCam>>();
        let sh = shared.clone();
        thread::Builder::new().name("vcam-output".into()).spawn(move || {
            let mut cam = cam;
            let mut compositor: Option<Compositor> = None;
            let mut sent: u64 = 0;
            while let Ok(mut job) = rx.recv() {
                if sh.error.lock().map(|e| e.is_some()).unwrap_or(true) {
                    continue; // drain quietly so submit() never blocks
                }
                if let Some(c) = job.compositor {
                    compositor = Some(c);
                }
                let comp = match compositor.as_mut() {
                    Some(c) => c,
                    None => {
                        if let Ok(mut e) = sh.error.lock() {
                            *e = Some("output pipeline started without a compositor".to_string());
                        }
                        continue;
                    }
                };
                if job.alpha.len() != job.source.w * job.source.h {
                    if let Ok(mut e) = sh.error.lock() {
                        *e = Some("matte size does not match the frame".to_string());
                    }
                    continue;
                }
                if job.single {
                    keep_largest(&mut job.alpha, job.source.w, job.source.h);
                }
                let out = comp.render(&job.source, &job.alpha, job.zoom, job.threshold);
                if let Some(cam) = cam.as_mut() {
                    cam.send(out);
                    if sent % 10 == 0 {
                        sh.consumers.store(cam.consumers(), Ordering::Relaxed);
                    }
                    sent += 1;
                    cam.sleep_until_next_frame();
                }
                let shared_frame = Arc::new(out.clone());
                let rect = comp.rect();
                if let Ok(mut l) = sh.latest.lock() {
                    *l = Some(shared_frame);
                }
                if let Ok(mut r) = sh.cam_rect.lock() {
                    *r = rect;
                }
            }
            // Hand the virtual camera back (on-demand mode keeps it open between
            // sessions); if nobody takes it, it is dropped here, which closes it.
            let _ = done_tx.send(cam);
        })?;
        Ok(OutputPipeline { tx: Some(tx), done: done_rx, shared })
    }

    /// The worker's failure, if any.
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|e| e.clone())
    }

    /// Hand one frame's render+send to the worker. Blocks (back-pressure) if the worker
    /// is two frames behind; fails if the worker has failed.
    pub fn submit(&self, job: Job) -> Result<()> {
        if let Some(e) = self.error() {
            return Err(anyhow!("{}", e));
        }
        match self.tx.as_ref() {
            Some(tx) => tx.send(job).map_err(|_| anyhow!("the output thread has stopped")),
            None => Err(anyhow!("the output pipeline is closed")),
        }
    }

    /// Drain and stop the worker. Returns the virtual camera, still open; dropping it
    /// closes it.
    pub fn close(&mut self) -> Option<VirtualCam> {
        if self.tx.take().is_some() {
            self.done.recv_timeout(Duration::from_secs(2)).ok().flatten()
        } else {
            None
        }
    }
}

impl Drop for OutputPipeline {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
