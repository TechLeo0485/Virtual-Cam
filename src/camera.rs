//! Webcam capture through Media Foundation, plus the background `FrameGrabber` thread.
//!
//! The camera is opened with a Source Reader that converts whatever the device
//! delivers (MJPG, YUY2, NV12, ...) to RGB32, so frames arrive as plain BGR pixels.

use crate::imgproc::{self, Bgr};
use crate::vlog;
use anyhow::{anyhow, bail, Result};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources,
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_MJPG, MFVideoFormat_NV12, MFVideoFormat_RGB32,
    MFVideoFormat_YUY2, MFSTARTUP_FULL, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
    MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED};

const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// COM + Media Foundation for the current thread.
struct MfGuard {
    com: bool,
}

impl MfGuard {
    fn new() -> Result<Self> {
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                if com {
                    CoUninitialize();
                }
                bail!("Media Foundation could not start: {}", e);
            }
            Ok(MfGuard { com })
        }
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

unsafe fn enum_devices() -> Result<Vec<IMFActivate>> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 1)?;
    let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned nothing"))?;
    attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;
    MFEnumDeviceSources(&attrs, &mut list, &mut count)?;
    let mut out = Vec::new();
    if !list.is_null() {
        for i in 0..count as usize {
            if let Some(act) = (*list.add(i)).take() {
                out.push(act);
            }
        }
        CoTaskMemFree(Some(list as *const c_void));
    }
    Ok(out)
}

unsafe fn friendly_name(act: &IMFActivate) -> String {
    let mut pw = PWSTR::null();
    let mut len: u32 = 0;
    if act.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut pw, &mut len).is_err() || pw.is_null() {
        return String::from("(unnamed camera)");
    }
    let name = pw.to_string().unwrap_or_else(|_| String::from("(unnamed camera)"));
    CoTaskMemFree(Some(pw.0 as *const c_void));
    name
}

/// Names of the video capture devices, in index order.
pub fn list_cameras() -> Result<Vec<String>> {
    let _mf = MfGuard::new()?;
    unsafe {
        let devices = enum_devices()?;
        Ok(devices.iter().map(|d| friendly_name(d)).collect())
    }
}

struct NativeFormat {
    mt: IMFMediaType,
    width: u32,
    height: u32,
    fps: f64,
    subtype: GUID,
}

unsafe fn frame_size(mt: &IMFMediaType) -> Option<(u32, u32)> {
    let v = mt.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
    Some(((v >> 32) as u32, (v & 0xffff_ffff) as u32))
}

unsafe fn frame_rate(mt: &IMFMediaType) -> f64 {
    match mt.GetUINT64(&MF_MT_FRAME_RATE) {
        Ok(v) => {
            let num = (v >> 32) as f64;
            let den = (v & 0xffff_ffff) as f64;
            if den > 0.0 {
                num / den
            } else {
                0.0
            }
        }
        Err(_) => 0.0,
    }
}

/// Lower is better: closest size first, then a frame rate that reaches the request
/// (the lowest such), then MJPG — many USB webcams only deliver 720p+ at full frame
/// rate in MJPG, while YUY2 at that size drops to ~10 fps.
fn format_score(f: &NativeFormat, width: u32, height: u32, fps: u32) -> (u64, u64, u32) {
    let dw = (f.width as i64 - width as i64).unsigned_abs();
    let dh = (f.height as i64 - height as i64).unsigned_abs();
    let want = fps as f64;
    let rate = if f.fps + 0.5 >= want {
        ((f.fps - want).max(0.0) * 100.0) as u64
    } else {
        100_000 + ((want - f.fps) * 100.0) as u64
    };
    let kind = if f.subtype == MFVideoFormat_MJPG {
        0
    } else if f.subtype == MFVideoFormat_NV12 {
        1
    } else if f.subtype == MFVideoFormat_YUY2 {
        2
    } else {
        3
    };
    (dw * dw + dh * dh, rate, kind)
}

/// An open webcam delivering BGR frames. Lives on the thread that opened it.
pub struct Camera {
    reader: IMFSourceReader,
    source: IMFMediaSource,
    width: usize,
    height: usize,
    stride: i32,
    fps: f64,
    _mf: MfGuard,
}

impl Camera {
    pub fn open(index: usize, width: u32, height: u32, fps: u32) -> Result<Camera> {
        let mf = MfGuard::new()?;
        let busy_hint = format!(
            "Could not open camera {}. A webcam can only be used by one app at a time; close anything \
             else using it (Meet/Zoom/Teams/OBS, or another vcam), or try a different camera index.",
            index
        );
        unsafe {
            let devices = enum_devices()?;
            let act = devices
                .get(index)
                .ok_or_else(|| anyhow!("{} ({} camera(s) found.)", busy_hint, devices.len()))?;
            let source: IMFMediaSource = act.ActivateObject().map_err(|e| anyhow!("{} ({})", busy_hint, e))?;

            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned nothing"))?;
            // Lets the reader decode MJPG and convert YUV formats to RGB32 for us.
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs)
                .map_err(|e| anyhow!("{} ({})", busy_hint, e))?;

            // What the device offers.
            let mut formats: Vec<NativeFormat> = Vec::new();
            for i in 0..512u32 {
                let mt = match reader.GetNativeMediaType(VIDEO_STREAM, i) {
                    Ok(mt) => mt,
                    Err(_) => break,
                };
                let size = frame_size(&mt);
                let subtype = mt.GetGUID(&MF_MT_SUBTYPE);
                if let (Some((w, h)), Ok(subtype)) = (size, subtype) {
                    let rate = frame_rate(&mt);
                    formats.push(NativeFormat { mt, width: w, height: h, fps: rate, subtype });
                }
            }
            if formats.is_empty() {
                bail!("camera {} reports no video formats", index);
            }
            formats.sort_by_key(|f| format_score(f, width, height, fps));

            // Select the native format first, then ask for RGB32 output.
            let mut chosen: Option<f64> = None;
            for f in formats.iter().take(12) {
                if reader.SetCurrentMediaType(VIDEO_STREAM, None, &f.mt).is_err() {
                    continue;
                }
                let out = MFCreateMediaType()?;
                out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
                if reader.SetCurrentMediaType(VIDEO_STREAM, None, &out).is_ok() {
                    chosen = Some(f.fps);
                    break;
                }
            }
            let native_fps = chosen.ok_or_else(|| anyhow!("camera {}: no usable video format", index))?;

            let cur = reader.GetCurrentMediaType(VIDEO_STREAM)?;
            let (w, h) = frame_size(&cur).ok_or_else(|| anyhow!("camera {}: unknown frame size", index))?;
            let stride = cur.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|v| v as i32).unwrap_or(w as i32 * 4);
            Ok(Camera {
                reader,
                source,
                width: w as usize,
                height: h as usize,
                stride,
                fps: native_fps,
                _mf: mf,
            })
        }
    }

    /// The size the camera actually delivers (it may not honour the request).
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn reported_fps(&self) -> f64 {
        self.fps
    }

    /// Block until the next frame. `Ok(None)` = nothing this time (stream tick); an
    /// error means the camera stopped delivering frames.
    pub fn read(&mut self) -> Result<Option<Bgr>> {
        unsafe {
            let mut actual: u32 = 0;
            let mut flags: u32 = 0;
            let mut timestamp: i64 = 0;
            let mut sample: Option<IMFSample> = None;
            self.reader.ReadSample(
                VIDEO_STREAM,
                0,
                Some(&mut actual),
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )?;
            let stop = (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32;
            if flags & stop != 0 {
                bail!("the camera stream ended");
            }
            let sample = match sample {
                Some(s) => s,
                None => return Ok(None),
            };
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (w, h) = (self.width, self.height);
            let mut frame = Bgr::new(w, h);

            if let Ok(buf2d) = buffer.cast::<IMF2DBuffer>() {
                // Scanline 0 is the top row; the pitch is negative for bottom-up images.
                let mut scan0: *mut u8 = std::ptr::null_mut();
                let mut pitch: i32 = 0;
                buf2d.Lock2D(&mut scan0, &mut pitch)?;
                if !scan0.is_null() {
                    copy_bgrx_rows(scan0, pitch as isize, w, h, &mut frame.data);
                }
                let _ = buf2d.Unlock2D();
            } else {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut len: u32 = 0;
                buffer.Lock(&mut data, None, Some(&mut len))?;
                let pitch = self.stride.unsigned_abs() as usize;
                if !data.is_null() && pitch >= w * 4 && len as usize >= pitch * h {
                    if self.stride >= 0 {
                        copy_bgrx_rows(data, pitch as isize, w, h, &mut frame.data);
                    } else {
                        let last = data.add(pitch * (h - 1));
                        copy_bgrx_rows(last, -(pitch as isize), w, h, &mut frame.data);
                    }
                }
                let _ = buffer.Unlock();
            }
            Ok(Some(frame))
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        unsafe {
            let _ = self.source.Shutdown();
        }
    }
}

/// Copy `h` rows of BGRX pixels starting at `scan0` (rows `pitch` bytes apart) to BGR.
unsafe fn copy_bgrx_rows(scan0: *const u8, pitch: isize, w: usize, h: usize, dst: &mut [u8]) {
    for y in 0..h {
        let row = std::slice::from_raw_parts(scan0.offset(pitch * y as isize), w * 4);
        let out = &mut dst[y * w * 3..(y + 1) * w * 3];
        for (o, s) in out.chunks_exact_mut(3).zip(row.chunks_exact(4)) {
            o[0] = s[0];
            o[1] = s[1];
            o[2] = s[2];
        }
    }
}

// --------------------------------------------------------------------------- //
// FrameGrabber
// --------------------------------------------------------------------------- //

struct GrabShared {
    frame: Mutex<(u64, Option<Arc<Bgr>>)>,
    running: AtomicBool,
    stop: AtomicBool,
}

/// Continuously pulls frames in a background thread so the main loop never blocks on
/// the camera. Reading can block for a full frame interval (~55 ms at 18 fps);
/// overlapping that with matting/compositing lets us run at the camera's true frame
/// rate instead of read+process serialized. The camera is opened, used and closed on
/// that thread.
pub struct FrameGrabber {
    shared: Arc<GrabShared>,
    done: mpsc::Receiver<()>,
    size: (usize, usize),
    fps: f64,
}

impl FrameGrabber {
    /// Open camera `index` and start grabbing. `rotation` (degrees clockwise) is applied
    /// to every frame. Returns once the camera is open (or failed to open).
    pub fn start(index: usize, width: u32, height: u32, fps: u32, rotation: u32) -> Result<FrameGrabber> {
        let shared = Arc::new(GrabShared {
            frame: Mutex::new((0, None)),
            running: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let (open_tx, open_rx) = mpsc::channel::<Result<((usize, usize), f64), String>>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let sh = shared.clone();
        thread::Builder::new().name("vcam-camera".into()).spawn(move || {
            let mut cam = match Camera::open(index, width, height, fps) {
                Ok(c) => {
                    let _ = open_tx.send(Ok((c.size(), c.reported_fps())));
                    c
                }
                Err(e) => {
                    sh.running.store(false, Ordering::SeqCst);
                    let _ = open_tx.send(Err(e.to_string()));
                    let _ = done_tx.send(());
                    return;
                }
            };
            while !sh.stop.load(Ordering::SeqCst) {
                match cam.read() {
                    Ok(Some(frame)) => {
                        let frame = if rotation != 0 { imgproc::rotate(&frame, rotation) } else { frame };
                        if let Ok(mut g) = sh.frame.lock() {
                            g.0 += 1;
                            g.1 = Some(Arc::new(frame));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        vlog!("[vcam] Camera read failed: {}", e);
                        break;
                    }
                }
            }
            sh.running.store(false, Ordering::SeqCst);
            drop(cam);
            let _ = done_tx.send(());
        })?;
        match open_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok((size, cam_fps))) => {
                let size = crate::geometry::rotated_size(size.0, size.1, rotation);
                Ok(FrameGrabber { shared, done: done_rx, size, fps: cam_fps })
            }
            Ok(Err(e)) => Err(anyhow!("{}", e)),
            Err(_) => {
                shared.stop.store(true, Ordering::SeqCst);
                Err(anyhow!("Could not open camera {}: it did not respond.", index))
            }
        }
    }

    /// Frame size after rotation.
    pub fn size(&self) -> (usize, usize) {
        self.size
    }

    pub fn reported_fps(&self) -> f64 {
        self.fps
    }

    pub fn running(&self) -> bool {
        self.shared.running.load(Ordering::SeqCst)
    }

    /// (sequence number, latest frame). The sequence number lets the caller skip
    /// re-processing a frame it has already handled.
    pub fn read(&self) -> (u64, Option<Arc<Bgr>>) {
        match self.shared.frame.lock() {
            Ok(g) => (g.0, g.1.clone()),
            Err(_) => (0, None),
        }
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.done.recv_timeout(Duration::from_secs(1));
    }
}

impl Drop for FrameGrabber {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}
