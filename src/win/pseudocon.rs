//! PseudoConsole (ConPTY) wrapper.
//!
//! ConPTY functions (`CreatePseudoConsole`, `ResizePseudoConsole`,
//! `ClosePseudoConsole`) are loaded dynamically at runtime.  This allows
//! applications to ship a sideloaded `conpty.dll` + `OpenConsole.exe`
//! alongside the binary for a newer ConPTY implementation than the one
//! bundled with the OS.
//!
//! Load order:
//! 1. `conpty.dll` in the application directory (sideloaded)
//! 2. `kernel32.dll` (system default, requires Windows 10 1809+)
//!
//! All other Win32 functions remain statically linked via `windows-sys`.

use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::error::{Error, Result};
use crate::win::procthreadattr::ProcThreadAttributeList;
use filedescriptor::{FileDescriptor, OwnedHandle};
use std::ffi::OsString;
use std::io::Error as IoError;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::OnceLock;
use std::{mem, ptr};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, S_OK};
use windows_sys::Win32::System::Console::{COORD, HPCON};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// Flags for `CreatePseudoConsole`. Combine with bitwise OR.
///
/// These are part of the public API for downstream callers to customize
/// ConPTY behavior via [`PseudoCon::new_with_flags`].
#[allow(dead_code)]
pub const PSEUDOCONSOLE_INHERIT_CURSOR: u32 = 0x1;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_RESIZE_QUIRK: u32 = 0x2;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: u32 = 0x4;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: u32 = 0x8;

/// Default flags used by [`PseudoCon::new`].
///
/// Does NOT include `PSEUDOCONSOLE_INHERIT_CURSOR` because it causes conhost
/// to send a `\x1b[6n` (Device Status Report) cursor query and block until the
/// client responds with `\x1b[row;colR` on the input pipe.  Most PTY consumers
/// never reply to DSR, which deadlocks the entire session.  Add the flag
/// explicitly via [`PseudoCon::new_with_flags`] if cursor inheritance is needed
/// and your code handles DSR responses.
pub const DEFAULT_PSEUDOCONSOLE_FLAGS: u32 =
    PSEUDOCONSOLE_RESIZE_QUIRK | PSEUDOCONSOLE_WIN32_INPUT_MODE;

// --- Dynamic loading of ConPTY functions ---

type CreatePseudoConsoleFn =
    unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type ResizePseudoConsoleFn = unsafe extern "system" fn(HPCON, COORD) -> i32;
type ClosePseudoConsoleFn = unsafe extern "system" fn(HPCON);

struct ConPtyFuncs {
    // Library must stay alive for the function pointers to remain valid
    _lib: libloading::Library,
    create: CreatePseudoConsoleFn,
    resize: ResizePseudoConsoleFn,
    close: ClosePseudoConsoleFn,
}

// SAFETY: The function pointers are from a loaded DLL and are valid for the
// lifetime of _lib.  Windows DLL functions are safe to call from any thread.
unsafe impl Send for ConPtyFuncs {}
unsafe impl Sync for ConPtyFuncs {}

fn try_load_from(dll: &str) -> std::result::Result<ConPtyFuncs, libloading::Error> {
    unsafe {
        let lib = libloading::Library::new(dll)?;
        let create: libloading::Symbol<CreatePseudoConsoleFn> = lib.get(b"CreatePseudoConsole")?;
        let resize: libloading::Symbol<ResizePseudoConsoleFn> = lib.get(b"ResizePseudoConsole")?;
        let close: libloading::Symbol<ClosePseudoConsoleFn> = lib.get(b"ClosePseudoConsole")?;
        Ok(ConPtyFuncs {
            create: *create,
            resize: *resize,
            close: *close,
            _lib: lib,
        })
    }
}

fn load_conpty() -> ConPtyFuncs {
    // First verify the system supports ConPTY at all (kernel32.dll)
    let kernel = try_load_from("kernel32.dll").expect(
        "this system does not support ConPTY. Windows 10 October 2018 or newer is required",
    );

    // Prefer a sideloaded conpty.dll (ships newer ConPTY + OpenConsole.exe)
    match try_load_from("conpty.dll") {
        Ok(sideloaded) => {
            log::info!("using sideloaded conpty.dll");
            sideloaded
        }
        Err(_) => kernel,
    }
}

static CONPTY: OnceLock<ConPtyFuncs> = OnceLock::new();

fn conpty() -> &'static ConPtyFuncs {
    CONPTY.get_or_init(load_conpty)
}

// --- PseudoCon ---

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
        unsafe { (conpty().close)(self.con) };
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
            (conpty().create)(
                size,
                input.as_raw_handle(),
                output.as_raw_handle(),
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
        let result = unsafe { (conpty().resize)(self.con, size) };
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
                &si.StartupInfo,
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

        let _main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as *mut _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as *mut _) };

        Ok(WinChild::new(proc))
    }
}
