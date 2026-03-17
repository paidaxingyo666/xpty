//! PseudoConsole (ConPTY) wrapper.
//!
//! Uses statically linked Windows API functions from `windows-sys`.
//! Requires Windows 10 version 1809 (October 2018 Update) or later.

use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::error::{Error, Result};
use crate::win::procthreadattr::ProcThreadAttributeList;
use filedescriptor::{FileDescriptor, OwnedHandle};
use std::ffi::OsString;
use std::io::Error as IoError;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::{mem, ptr};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, S_OK};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// Flags for `CreatePseudoConsole`. Combine with bitwise OR.
pub const PSEUDOCONSOLE_INHERIT_CURSOR: u32 = 0x1;
pub const PSEUDOCONSOLE_RESIZE_QUIRK: u32 = 0x2;
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: u32 = 0x4;
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: u32 = 0x8;

/// Default flags used by [`PseudoCon::new`].
pub const DEFAULT_PSEUDOCONSOLE_FLAGS: u32 =
    PSEUDOCONSOLE_INHERIT_CURSOR | PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE;

/// Wrapper around a Windows PseudoConsole (ConPTY) handle.
///
/// # Safety
/// The HPCON handle is owned by this struct and closed on drop.
/// HPCON handles are safe to send between threads according to
/// the Windows ConPTY documentation.
pub struct PseudoCon {
    con: HPCON,
}

unsafe impl Send for PseudoCon {}
unsafe impl Sync for PseudoCon {}

impl Drop for PseudoCon {
    fn drop(&mut self) {
        unsafe { ClosePseudoConsole(self.con) };
    }
}

impl PseudoCon {
    /// Create a new PseudoConsole with [`DEFAULT_PSEUDOCONSOLE_FLAGS`].
    pub fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self> {
        Self::new_with_flags(size, input, output, DEFAULT_PSEUDOCONSOLE_FLAGS)
    }

    /// Create a new PseudoConsole with explicit flags.
    ///
    /// Use [`DEFAULT_PSEUDOCONSOLE_FLAGS`] as a starting point and add/remove
    /// flags as needed.  For example, to disable `PSEUDOCONSOLE_INHERIT_CURSOR`
    /// (which can cause screen-clearing artifacts):
    ///
    /// ```ignore
    /// let flags = DEFAULT_PSEUDOCONSOLE_FLAGS & !PSEUDOCONSOLE_INHERIT_CURSOR;
    /// PseudoCon::new_with_flags(size, input, output, flags)?;
    /// ```
    pub fn new_with_flags(
        size: COORD,
        input: FileDescriptor,
        output: FileDescriptor,
        flags: u32,
    ) -> Result<Self> {
        let mut con: HPCON = INVALID_HANDLE_VALUE as HPCON;
        let result = unsafe {
            CreatePseudoConsole(
                size,
                input.as_raw_handle() as isize,
                output.as_raw_handle() as isize,
                flags,
                &mut con,
            )
        };
        if result != S_OK {
            return Err(Error::Hresult(result));
        }
        Ok(Self { con })
    }

    pub fn resize(&self, size: COORD) -> Result<()> {
        let result = unsafe { ResizePseudoConsole(self.con, size) };
        if result != S_OK {
            return Err(Error::Hresult(result));
        }
        Ok(())
    }

    pub fn spawn_command(&self, cmd: CommandBuilder) -> Result<WinChild> {
        let mut si: STARTUPINFOEXW = unsafe { mem::zeroed() };
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE as HANDLE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE as HANDLE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE as HANDLE;

        let mut attrs = ProcThreadAttributeList::with_capacity(1)?;
        attrs.set_pty(self.con)?;
        si.lpAttributeList = attrs.as_mut_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let (mut exe, mut cmdline) = cmd.cmdline()?;
        let cmd_os = OsString::from_wide(&cmdline);

        let cwd = cmd.current_directory();

        let res = unsafe {
            CreateProcessW(
                exe.as_mut_slice().as_mut_ptr(),
                cmdline.as_mut_slice().as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                cmd.environment_block().as_mut_slice().as_mut_ptr() as *mut _,
                cwd.as_ref()
                    .map(|c| c.as_slice().as_ptr())
                    .unwrap_or(ptr::null()),
                &mut si.StartupInfo,
                &mut pi,
            )
        };
        if res == 0 {
            let err = IoError::last_os_error();
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            log::error!("{}", msg);
            return Err(Error::other(msg));
        }

        // Close the thread handle to avoid leaking it
        let _main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as *mut _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as *mut _) };

        Ok(WinChild::new(proc))
    }
}
