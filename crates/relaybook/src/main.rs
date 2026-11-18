fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    keep_std_handles_private();
    cli::execute()
}

/// Relaybook's stdio must not outlive relaybook in git, agents or their helpers.
#[cfg(windows)]
fn keep_std_handles_private() {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // SAFETY: `GetStdHandle` borrows this process's standard handles.
    // `SetHandleInformation` only clears the inherit bit and does not close
    // them. A null or invalid handle is skipped, and a failed call is ignored.
    unsafe {
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = usize::MAX as *mut std::ffi::c_void;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[allow(non_snake_case)]
    fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
    #[allow(non_snake_case)]
    fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
}

#[cfg(windows)]
#[test]
fn keep_std_handles_private_clears_the_inherit_bit() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[allow(non_snake_case)]
        fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
    }
    keep_std_handles_private();
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // SAFETY: reads the flags of this process's standard error handle and does not close it.
    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return;
        }
        let mut flags = 0_u32;
        let ok = GetHandleInformation(handle, &mut flags);
        assert_ne!(ok, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }
}

mod agents;
mod claude_hook;
mod cli;
mod commands;
mod exe;
mod gate;
mod git;
mod gitinfo;
mod graphify;
mod guide;
mod hooks;
mod install;
mod live;
mod managed;
mod manifest;
mod paths;
mod picker;
mod remote;
mod render;
mod runloop;
mod setup;
mod signoff;
mod stream;
mod uninstall;
mod workflow;
