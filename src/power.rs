//! Windows power throttling (EcoQoS) opt-out.

use windows::Win32::System::Threading::{
    GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
};

/// Opt this process out of Windows power throttling.
///
/// Windows 11 treats a process whose window is minimized or covered as background
/// work: it lowers its clock speed and, on hybrid CPUs (Intel 12th gen+), moves its
/// threads to the slow efficiency cores. For a live camera that means the frame rate
/// collapses the moment the window is hidden, so we opt out and keep matting at full
/// speed. Returns false on Windows builds without the API.
pub fn disable_power_throttling() -> bool {
    // ControlMask = EXECUTION_SPEED with StateMask = 0 -> "never throttle me".
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const PROCESS_POWER_THROTTLING_STATE as *const core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
        .is_ok()
    }
}
