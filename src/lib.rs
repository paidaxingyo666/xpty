//! Cross-platform async-ready PTY interface.
//!
//! This crate provides a cross platform API for working with the
//! pseudo terminal (pty) interfaces provided by the system.
//! Unlike other crates in this space, this crate provides a set
//! of traits that allow selecting from different implementations
//! at runtime.
//!
//! Forked from [portable-pty](https://github.com/wezterm/wezterm/tree/main/pty)
//! (part of wezterm).
//!
//! ```no_run
//! use xpty::{CommandBuilder, PtySize, native_pty_system, PtySystem};
//!
//! // Use the native pty implementation for the system
//! let pty_system = native_pty_system();
//!
//! // Create a new pty
//! let mut pair = pty_system.openpty(PtySize {
//!     rows: 24,
//!     cols: 80,
//!     pixel_width: 0,
//!     pixel_height: 0,
//! })?;
//!
//! // Spawn a shell into the pty
//! let cmd = CommandBuilder::new("bash");
//! let child = pair.slave.spawn_command(cmd)?;
//!
//! // Read and parse output from the pty with reader
//! let mut reader = pair.master.try_clone_reader()?;
//!
//! // Send data to the pty by writing to the master
//! writeln!(pair.master.take_writer()?, "ls -l\r\n")?;
//! # Ok::<(), xpty::Error>(())
//! ```
//!
pub mod error;
pub use error::{Error, Result};

pub mod cmdbuilder;
pub use cmdbuilder::CommandBuilder;

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod win;

#[cfg(feature = "serial")]
pub mod serial;

use downcast_rs::{impl_downcast, Downcast};
#[cfg(feature = "serde_support")]
use serde::{Deserialize, Serialize};
use std::io::Result as IoResult;

/// Represents the size of the visible display area in the pty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde_support", derive(Serialize, Deserialize))]
pub struct PtySize {
    /// The number of lines of text
    pub rows: u16,
    /// The number of columns of text
    pub cols: u16,
    /// The width of a cell in pixels.  Note that some systems never
    /// fill this value and ignore it.
    pub pixel_width: u16,
    /// The height of a cell in pixels.  Note that some systems never
    /// fill this value and ignore it.
    pub pixel_height: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Represents the master/control end of the pty.
///
/// All methods on this trait are cross-platform. Platform-specific
/// extensions are available via [`MasterPtyExt`] (unix only).
pub trait MasterPty: Downcast + Send {
    /// Inform the kernel and thus the child process that the window resized.
    fn resize(&self, size: PtySize) -> Result<()>;
    /// Retrieves the size of the pty as known by the kernel.
    fn get_size(&self) -> Result<PtySize>;
    /// Obtain a readable handle; output from the slave(s) is readable
    /// via this stream.
    fn try_clone_reader(&self) -> Result<Box<dyn std::io::Read + Send>>;
    /// Obtain a writable handle; writing to it will send data to the
    /// slave end.  Dropping the writer will send EOF to the slave end.
    /// It is invalid to take the writer more than once.
    fn take_writer(&self) -> Result<Box<dyn std::io::Write + Send>>;

    /// If applicable, return the local process id of the process group
    /// or session leader.  Returns `None` on non-Unix platforms.
    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    /// If applicable, return the raw file descriptor of the master pty.
    /// Returns `None` on non-Unix platforms.
    fn as_raw_fd(&self) -> Option<i32> {
        None
    }

    /// Returns the TTY device name (e.g., `/dev/pts/0`).
    /// Returns `None` on non-Unix platforms.
    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}
impl_downcast!(MasterPty);

/// Unix-specific extensions for [`MasterPty`].
///
/// Provides access to termios settings, which requires platform-specific types.
#[cfg(unix)]
pub trait MasterPtyExt {
    /// If applicable, return the termios associated with the stream.
    fn get_termios(&self) -> Option<nix::sys::termios::Termios> {
        None
    }
}

/// Represents a child process spawned into the pty.
pub trait Child: std::fmt::Debug + ChildKiller + Downcast + Send {
    /// Poll the child to see if it has completed.  Does not block.
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>>;
    /// Blocks execution until the child process has completed.
    fn wait(&mut self) -> IoResult<ExitStatus>;
    /// Returns the process identifier of the child process, if applicable.
    fn process_id(&self) -> Option<u32>;
    /// Returns the process handle of the child process (Windows only).
    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle>;
}
impl_downcast!(Child);

/// Represents the ability to signal a Child to terminate.
pub trait ChildKiller: std::fmt::Debug + Downcast + Send {
    /// Terminate the child process.
    fn kill(&mut self) -> IoResult<()>;

    /// Clone an object that can be split out from the Child in order
    /// to send it signals independently from a thread that may be
    /// blocked in `.wait`.
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync>;
}
impl_downcast!(ChildKiller);

/// Represents the slave side of a pty.
/// Can be used to spawn processes into the pty.
pub trait SlavePty: Send {
    /// Spawns the command specified by the provided CommandBuilder.
    fn spawn_command(&self, cmd: CommandBuilder) -> Result<Box<dyn Child + Send + Sync>>;
}

/// Represents the exit status of a child process.
#[derive(Debug, Clone)]
pub struct ExitStatus {
    code: u32,
    signal: Option<String>,
}

impl ExitStatus {
    /// Construct an ExitStatus from a process return code.
    pub fn with_exit_code(code: u32) -> Self {
        Self { code, signal: None }
    }

    /// Construct an ExitStatus from a signal name.
    pub fn with_signal(signal: &str) -> Self {
        Self {
            code: 1,
            signal: Some(signal.to_string()),
        }
    }

    /// Returns true if the status indicates successful completion.
    pub fn success(&self) -> bool {
        self.signal.is_none() && self.code == 0
    }

    /// Returns the exit code.
    pub fn exit_code(&self) -> u32 {
        self.code
    }

    /// Returns the signal name if present.
    pub fn signal(&self) -> Option<&str> {
        self.signal.as_deref()
    }
}

impl From<std::process::ExitStatus> for ExitStatus {
    fn from(status: std::process::ExitStatus) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            if let Some(signal) = status.signal() {
                let signame = unsafe { libc::strsignal(signal) };
                let signal = if signame.is_null() {
                    format!("Signal {}", signal)
                } else {
                    let signame = unsafe { std::ffi::CStr::from_ptr(signame) };
                    signame.to_string_lossy().to_string()
                };

                return ExitStatus {
                    code: status.code().map(|c| c as u32).unwrap_or(1),
                    signal: Some(signal),
                };
            }
        }

        let code =
            status
                .code()
                .map(|c| c as u32)
                .unwrap_or_else(|| if status.success() { 0 } else { 1 });

        ExitStatus { code, signal: None }
    }
}

impl std::fmt::Display for ExitStatus {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.success() {
            write!(fmt, "Success")
        } else {
            match &self.signal {
                Some(sig) => write!(fmt, "Terminated by {}", sig),
                None => write!(fmt, "Exited with code {}", self.code),
            }
        }
    }
}

/// A pair of master and slave PTY handles.
pub struct PtyPair {
    // slave is listed first so that it is dropped first.
    // The drop order is stable and specified by rust rfc 1857
    pub slave: Box<dyn SlavePty>,
    pub master: Box<dyn MasterPty + Send>,
}

/// The `PtySystem` trait allows an application to work with multiple
/// possible Pty implementations at runtime.
pub trait PtySystem: Downcast {
    /// Create a new Pty instance with the window size set to the specified
    /// dimensions.  Returns a (master, slave) Pty pair.
    fn openpty(&self, size: PtySize) -> Result<PtyPair>;
}
impl_downcast!(PtySystem);

impl Child for std::process::Child {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        std::process::Child::try_wait(self).map(|s| s.map(Into::into))
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        std::process::Child::wait(self).map(Into::into)
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.id())
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        Some(std::os::windows::io::AsRawHandle::as_raw_handle(self))
    }
}

#[derive(Debug)]
struct ProcessSignaller {
    pid: Option<u32>,

    #[cfg(windows)]
    handle: Option<filedescriptor::OwnedHandle>,
}

#[cfg(windows)]
impl ChildKiller for ProcessSignaller {
    fn kill(&mut self) -> IoResult<()> {
        if let Some(handle) = &self.handle {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                if windows_sys::Win32::System::Threading::TerminateProcess(
                    handle.as_raw_handle(),
                    127,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self {
            pid: self.pid,
            handle: self.handle.as_ref().and_then(|h| h.try_clone().ok()),
        })
    }
}

#[cfg(unix)]
impl ChildKiller for ProcessSignaller {
    fn kill(&mut self) -> IoResult<()> {
        if let Some(pid) = self.pid {
            let result = unsafe { libc::kill(pid as i32, libc::SIGHUP) };
            if result != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self { pid: self.pid })
    }
}

impl ChildKiller for std::process::Child {
    fn kill(&mut self) -> IoResult<()> {
        #[cfg(unix)]
        {
            let result = unsafe { libc::kill(self.id() as i32, libc::SIGHUP) };
            if result != 0 {
                return Err(std::io::Error::last_os_error());
            }

            for attempt in 0..5 {
                if attempt > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if let Ok(Some(_)) = self.try_wait() {
                    return Ok(());
                }
            }
        }

        std::process::Child::kill(self)
    }

    #[cfg(windows)]
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        use std::os::windows::io::AsRawHandle;
        struct RawDup(std::os::windows::io::RawHandle);
        impl AsRawHandle for RawDup {
            fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
                self.0
            }
        }

        Box::new(ProcessSignaller {
            pid: self.process_id(),
            handle: Child::as_raw_handle(self)
                .as_ref()
                .and_then(|h| filedescriptor::OwnedHandle::dup(&RawDup(*h)).ok()),
        })
    }

    #[cfg(unix)]
    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(ProcessSignaller {
            pid: self.process_id(),
        })
    }
}

/// Returns a `NativePtySystem` for the current platform.
///
/// If you need a trait object (e.g., for runtime dispatch or mock injection),
/// use [`native_pty_system_boxed`] instead.
pub fn native_pty_system() -> NativePtySystem {
    NativePtySystem::default()
}

/// Returns the native PTY system as a boxed trait object.
///
/// Useful when you need runtime polymorphism, e.g., swapping in a mock
/// implementation for testing.
pub fn native_pty_system_boxed() -> Box<dyn PtySystem + Send> {
    Box::new(NativePtySystem::default())
}

#[cfg(unix)]
pub type NativePtySystem = unix::UnixPtySystem;
#[cfg(windows)]
pub type NativePtySystem = win::conpty::ConPtySystem;
