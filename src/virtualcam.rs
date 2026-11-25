//! Virtual-camera output through the OBS Virtual Camera (DirectShow source).
//!
//! OBS's virtual camera module reads frames from a named shared-memory queue
//! (`OBSVirtualCamVideo`, three NV12 frames plus a small header). Writing that queue is
//! all it takes to feed the camera — OBS itself does not need to be running. This is
//! the same protocol `pyvirtualcam` uses.

use crate::imgproc::Bgr;
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::core::{s, w, HSTRING};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::Foundation::{CloseHandle, BOOL, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS, FILE_MAP_READ,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ};

/// CLSID of the OBS Virtual Camera DirectShow filter (stable across OBS 26+).
pub const OBS_VCAM_CLSID: &str = "{A3FCE0F5-3493-419F-958A-ABA1250EC20B}";
pub const DEVICE_NAME: &str = "OBS Virtual Camera";
const OBS_VCAM_DLLS: [&str; 2] = [
    r"C:\Program Files\obs-studio\data\obs-plugins\win-dshow\obs-virtualcam-module64.dll",
    r"C:\Program Files\obs-studio\data\obs-plugins\win-dshow\obs-virtualcam-module32.dll",
];

// --------------------------------------------------------------------------- //
// Driver status / install
// --------------------------------------------------------------------------- //

/// Existing OBS Virtual Camera module DLL paths (64- and 32-bit).
pub fn find_virtualcam_dlls() -> Vec<String> {
    OBS_VCAM_DLLS.iter().filter(|p| Path::new(p).is_file()).map(|p| p.to_string()).collect()
}

/// True if the OBS Virtual Camera DirectShow source is registered system-wide.
pub fn virtualcam_installed() -> bool {
    let subkey = HSTRING::from(format!("SOFTWARE\\Classes\\CLSID\\{}", OBS_VCAM_CLSID));
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, &subkey, 0, KEY_READ, &mut key) == ERROR_SUCCESS {
            let _ = RegCloseKey(key);
            true
        } else {
            false
        }
    }
}

pub struct VirtualCamStatus {
    pub installed: bool,
    pub obs_found: bool,
    pub dlls: Vec<String>,
}

pub fn virtualcam_status() -> VirtualCamStatus {
    let dlls = find_virtualcam_dlls();
    VirtualCamStatus { installed: virtualcam_installed(), obs_found: !dlls.is_empty(), dlls }
}

/// regsvr32 the DLLs with a single UAC elevation prompt, then verify.
fn run_regsvr(dlls: &[String], unregister: bool) -> (bool, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if dlls.is_empty() {
        return (
            false,
            "OBS Virtual Camera not found. Install OBS Studio (https://obsproject.com) — it provides \
             the virtual-camera driver."
                .to_string(),
        );
    }
    let flag = if unregister { "/u /s" } else { "/s" };
    let mut lines = vec!["@echo off".to_string()];
    for d in dlls {
        lines.push(format!("regsvr32 {} \"{}\"", flag, d));
    }
    let bat = std::env::temp_dir().join("vcam_virtualcam_setup.bat");
    if let Err(e) = std::fs::write(&bat, lines.join("\r\n")) {
        return (false, format!("Could not write the setup script: {}", e));
    }
    let ps = format!("Start-Process -FilePath '{}' -Verb RunAs -Wait", bat.display());
    let run = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = std::fs::remove_file(&bat);
    if let Err(e) = run {
        return (false, format!("Elevation failed or was cancelled: {}", e));
    }
    // Trust the registry, not regsvr32's (often-zero) exit code.
    let now = virtualcam_installed();
    if unregister {
        let msg = if !now { "Virtual camera uninstalled." } else { "Could not unregister (elevation cancelled?)." };
        (!now, msg.to_string())
    } else {
        let msg = if now {
            "Virtual camera installed — pick 'OBS Virtual Camera' in your app."
        } else {
            "Registration did not take effect (elevation cancelled?)."
        };
        (now, msg.to_string())
    }
}

pub fn install_virtualcam() -> (bool, String) {
    run_regsvr(&find_virtualcam_dlls(), false)
}

pub fn uninstall_virtualcam() -> (bool, String) {
    run_regsvr(&find_virtualcam_dlls(), true)
}

// --------------------------------------------------------------------------- //
// Shared-memory queue (obs-studio: shared-memory-queue.c)
// --------------------------------------------------------------------------- //

#[repr(C)]
struct QueueHeader {
    write_idx: u32,
    read_idx: u32,
    state: u32,
    offsets: [u32; 3],
    queue_type: u32,
    cx: u32,
    cy: u32,
    interval: u64,
    reserved: [u32; 8],
}

const STATE_STARTING: u32 = 1;
const STATE_READY: u32 = 2;
const STATE_STOPPING: u32 = 3;
const FRAME_HEADER_SIZE: usize = 32;

fn align32(v: usize) -> usize {
    (v + 31) & !31
}

/// A live virtual camera. Frames are BGR; they are converted to NV12 (BT.601 limited
/// range, like libyuv / pyvirtualcam) and written to the queue.
pub struct VirtualCam {
    handle: HANDLE,
    view: *mut u8,
    offsets: [usize; 3],
    width: usize,
    height: usize,
    fps: f64,
    nv12: Vec<u8>,
    clock: Instant,
    last_frame: Option<Instant>,
    avg_delta: f64,
    first_delta: bool,
    extra_time: f64,
}

// The mapping is only touched by the thread that owns the VirtualCam.
unsafe impl Send for VirtualCam {}

impl VirtualCam {
    pub fn open(width: usize, height: usize, fps: f64) -> Result<VirtualCam> {
        if !virtualcam_installed() {
            bail!("OBS Virtual Camera device not found! Did you install OBS?");
        }
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            bail!("the virtual camera needs even, non-zero dimensions (got {}x{})", width, height);
        }
        let fps = if fps > 0.0 { fps } else { 30.0 };
        let frame_size = width * height * 3 / 2;
        let mut size = align32(std::mem::size_of::<QueueHeader>());
        let mut offsets = [0usize; 3];
        for off in offsets.iter_mut() {
            *off = size;
            size = align32(size + frame_size + FRAME_HEADER_SIZE);
        }
        unsafe {
            // fail if already in use
            if let Ok(existing) = OpenFileMappingW(FILE_MAP_READ.0, BOOL(0), w!("OBSVirtualCamVideo")) {
                let _ = CloseHandle(existing);
                bail!(
                    "virtual camera output could not be started (it is already in use — is OBS's \
                     virtual camera or another vcam running?)"
                );
            }
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size as u32,
                w!("OBSVirtualCamVideo"),
            )
            .map_err(|e| anyhow!("virtual camera output could not be started: {}", e))?;
            let addr = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if addr.Value.is_null() {
                let _ = CloseHandle(handle);
                bail!("virtual camera output could not be started (mapping failed)");
            }
            let view = addr.Value as *mut u8;
            let header = QueueHeader {
                write_idx: 0,
                read_idx: 0,
                state: STATE_STARTING,
                offsets: [offsets[0] as u32, offsets[1] as u32, offsets[2] as u32],
                queue_type: 0,
                cx: width as u32,
                cy: height as u32,
                interval: (10_000_000.0 / fps) as u64,
                reserved: [0; 8],
            };
            std::ptr::write_unaligned(view as *mut QueueHeader, header);
            Ok(VirtualCam {
                handle,
                view,
                offsets,
                width,
                height,
                fps,
                nv12: vec![0u8; frame_size],
                clock: Instant::now(),
                last_frame: None,
                avg_delta: 1.0 / fps,
                first_delta: true,
                extra_time: 0.0,
            })
        }
    }

    /// The name of the camera device to select in video apps.
    pub fn device(&self) -> &'static str {
        DEVICE_NAME
    }

    /// (width, height) of the frames this queue carries.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// How many video apps are reading the camera right now.
    ///
    /// OBS's virtual-camera filter opens the shared-memory queue only while an app is
    /// streaming from it and closes it when the app turns the camera off, so the
    /// section's handle count (all processes) minus our own handle is the number of
    /// apps watching. An app that only lists cameras does not open it. Measured with a
    /// DirectShow app on OBS 32.2: 1 -> 2 within 0.4 s of streaming, back to 1 at
    /// release; while streaming it briefly reads 1 now and then (the filter reopens the
    /// queue), so callers should wait a few seconds before treating 0 as "nobody".
    /// Returns 1 when the count can't be read, so vcam never stops a camera in use.
    pub fn consumers(&self) -> usize {
        match section_handle_count(self.handle) {
            Some(n) => n.saturating_sub(1) as usize,
            None => 1,
        }
    }

    /// Send one frame (it must have the camera's size).
    pub fn send(&mut self, frame: &Bgr) {
        if frame.w != self.width || frame.h != self.height {
            return;
        }
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            let delta = now.duration_since(prev).as_secs_f64();
            if self.first_delta {
                self.avg_delta = delta;
                self.first_delta = false;
            } else {
                self.avg_delta += (delta - self.avg_delta) * 0.2;
            }
        }
        self.last_frame = Some(now);

        bgr_to_nv12(frame, &mut self.nv12);
        unsafe {
            let header = self.view as *mut QueueHeader;
            let inc = (*header).write_idx.wrapping_add(1);
            std::ptr::write_volatile(std::ptr::addr_of_mut!((*header).write_idx), inc);
            let idx = (inc % 3) as usize;
            let slot = self.view.add(self.offsets[idx]);
            let timestamp = self.clock.elapsed().as_nanos() as u64;
            std::ptr::write_unaligned(slot as *mut u64, timestamp);
            std::ptr::copy_nonoverlapping(self.nv12.as_ptr(), slot.add(FRAME_HEADER_SIZE), self.nv12.len());
            std::ptr::write_volatile(std::ptr::addr_of_mut!((*header).read_idx), inc);
            std::ptr::write_volatile(std::ptr::addr_of_mut!((*header).state), STATE_READY);
        }
    }

    /// Adaptively sleep until the next frame is due, continuously adjusting the sleep
    /// so the target frame rate is reached (pyvirtualcam's algorithm).
    pub fn sleep_until_next_frame(&mut self) {
        let last = match self.last_frame {
            Some(t) => t,
            None => return,
        };
        let next = last + Duration::from_secs_f64(1.0 / self.fps);
        let now = Instant::now();
        if now < next {
            let avg_fps = if self.avg_delta > 0.0 { 1.0 / self.avg_delta } else { self.fps };
            let factor = self.fps / avg_fps - 1.0;
            self.extra_time = (self.extra_time + 0.01 * factor).max(0.0);
            let t_sleep = next.duration_since(now).as_secs_f64() - self.extra_time;
            if t_sleep > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(t_sleep));
            }
        }
    }
}

impl Drop for VirtualCam {
    fn drop(&mut self) {
        unsafe {
            let header = self.view as *mut QueueHeader;
            std::ptr::write_volatile(std::ptr::addr_of_mut!((*header).state), STATE_STOPPING);
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view as *mut core::ffi::c_void });
            let _ = CloseHandle(self.handle);
        }
    }
}

/// `PUBLIC_OBJECT_BASIC_INFORMATION` (NtQueryObject class 0).
#[repr(C)]
struct ObjectBasicInformation {
    attributes: u32,
    granted_access: u32,
    handle_count: u32,
    pointer_count: u32,
    reserved: [u32; 10],
}

type NtQueryObjectFn = unsafe extern "system" fn(HANDLE, u32, *mut core::ffi::c_void, u32, *mut u32) -> i32;

/// Number of open handles to the kernel object behind `handle`, in every process.
fn section_handle_count(handle: HANDLE) -> Option<u32> {
    static NT_QUERY_OBJECT: OnceLock<Option<NtQueryObjectFn>> = OnceLock::new();
    let query = (*NT_QUERY_OBJECT.get_or_init(|| unsafe {
        let ntdll = GetModuleHandleW(w!("ntdll.dll")).ok()?;
        let proc = GetProcAddress(ntdll, s!("NtQueryObject"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, NtQueryObjectFn>(proc))
    }))?;
    let mut info = ObjectBasicInformation {
        attributes: 0,
        granted_access: 0,
        handle_count: 0,
        pointer_count: 0,
        reserved: [0; 10],
    };
    let status = unsafe {
        query(
            handle,
            0, // ObjectBasicInformation
            &mut info as *mut ObjectBasicInformation as *mut core::ffi::c_void,
            std::mem::size_of::<ObjectBasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    (status >= 0).then_some(info.handle_count)
}

/// BGR -> NV12 with the BT.601 limited-range integer coefficients libyuv uses
/// (chroma from the 2x2 average).
pub fn bgr_to_nv12(frame: &Bgr, out: &mut [u8]) {
    let (w, h) = (frame.w, frame.h);
    let (y_plane, uv_plane) = out.split_at_mut(w * h);
    y_plane.par_chunks_mut(w).zip(frame.data.par_chunks(w * 3)).for_each(|(yrow, src)| {
        for (y, px) in yrow.iter_mut().zip(src.chunks_exact(3)) {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            *y = ((66 * r + 129 * g + 25 * b + 0x1080) >> 8) as u8;
        }
    });
    let data = &frame.data;
    uv_plane.par_chunks_mut(w).enumerate().for_each(|(cy, uvrow)| {
        let r0 = &data[(2 * cy) * w * 3..(2 * cy + 1) * w * 3];
        let r1 = &data[(2 * cy + 1) * w * 3..(2 * cy + 2) * w * 3];
        for cx in 0..w / 2 {
            let i = cx * 6;
            let b = (r0[i] as i32 + r0[i + 3] as i32 + r1[i] as i32 + r1[i + 3] as i32 + 2) >> 2;
            let g = (r0[i + 1] as i32 + r0[i + 4] as i32 + r1[i + 1] as i32 + r1[i + 4] as i32 + 2) >> 2;
            let r = (r0[i + 2] as i32 + r0[i + 5] as i32 + r1[i + 2] as i32 + r1[i + 5] as i32 + 2) >> 2;
            uvrow[cx * 2] = ((112 * b - 74 * g - 38 * r + 0x8080) >> 8).clamp(0, 255) as u8;
            uvrow[cx * 2 + 1] = ((112 * r - 94 * g - 18 * b + 0x8080) >> 8).clamp(0, 255) as u8;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_matches_obs() {
        assert_eq!(std::mem::size_of::<QueueHeader>(), 80);
        assert_eq!(align32(80), 96);
    }

    #[test]
    fn nv12_of_white_and_black() {
        let mut out = vec![0u8; 4 * 2 * 3 / 2];
        bgr_to_nv12(&Bgr::filled(4, 2, (255, 255, 255)), &mut out);
        assert_eq!(out[0], 235);
        assert_eq!(out[8], 128);
        bgr_to_nv12(&Bgr::filled(4, 2, (0, 0, 0)), &mut out);
        assert_eq!(out[0], 16);
        assert_eq!(out[9], 128);
    }
}
