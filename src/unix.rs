//! Working with pseudo-terminals on Unix

use crate::{Child, CommandBuilder, Error, MasterPty, MasterPtyExt, PtyPair, PtySize, PtySystem, Result, SlavePty};
use filedescriptor::FileDescriptor;
use libc::{self, winsize};
use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::{io, mem, ptr};

pub use std::os::unix::io::RawFd;

#[derive(Default)]
pub struct UnixPtySystem {}

fn openpty(size: PtySize) -> Result<(UnixMasterPty, UnixSlavePty)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;

    let mut size = winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    };

    let result = unsafe {
        #[allow(clippy::unnecessary_mut_passed)]
        libc::openpty(
            &mut master,
            &mut slave,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    };

    if result != 0 {
        return Err(Error::Io(io::Error::last_os_error()));
    }

    let tty_name = tty_name(slave);

    let master = UnixMasterPty {
        fd: PtyFd(unsafe { FileDescriptor::from_raw_fd(master) }),
        took_writer: RefCell::new(false),
        tty_name,
    };
    let slave = UnixSlavePty {
        fd: PtyFd(unsafe { FileDescriptor::from_raw_fd(slave) }),
    };

    cloexec(master.fd.as_raw_fd())?;
    cloexec(slave.fd.as_raw_fd())?;

    Ok((master, slave))
}

impl PtySystem for UnixPtySystem {
    fn openpty(&self, size: PtySize) -> Result<PtyPair> {
        let (master, slave) = openpty(size)?;
        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct PtyFd(pub FileDescriptor);
impl std::ops::Deref for PtyFd {
    type Target = FileDescriptor;
    fn deref(&self) -> &FileDescriptor {
        &self.0
    }
}
impl std::ops::DerefMut for PtyFd {
    fn deref_mut(&mut self) -> &mut FileDescriptor {
        &mut self.0
    }
}

impl Read for PtyFd {
    fn read(&mut self, buf: &mut [u8]) -> std::result::Result<usize, io::Error> {
        match self.0.read(buf) {
            Err(ref e) if e.raw_os_error() == Some(libc::EIO) => Ok(0),
            x => x,
        }
    }
}

fn tty_name(fd: RawFd) -> Option<PathBuf> {
    let mut buf = vec![0 as std::ffi::c_char; 128];

    loop {
        let res = unsafe { libc::ttyname_r(fd, buf.as_mut_ptr(), buf.len()) };

        if res == libc::ERANGE {
            if buf.len() > 64 * 1024 {
                return None;
            }
            buf.resize(buf.len() * 2, 0 as std::ffi::c_char);
            continue;
        }

        return if res == 0 {
            let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
            let osstr = OsStr::from_bytes(cstr.to_bytes());
            Some(PathBuf::from(osstr))
        } else {
            None
        };
    }
}

/// Close all file descriptors numbered 3 or higher.
///
/// Uses `close_range(2)` on Linux 5.9+ for efficiency,
/// falling back to enumerating `/dev/fd`.
pub fn close_random_fds() {
    // Try close_range(2) on Linux for efficiency
    #[cfg(target_os = "linux")]
    {
        let ret = unsafe { libc::close_range(3, libc::c_uint::MAX, 0) };
        if ret == 0 {
            return;
        }
        // Fall through on older kernels that don't support close_range
    }

    // Fallback: enumerate /dev/fd (works on macOS, BSDs, and older Linux)
    if let Ok(dir) = std::fs::read_dir("/dev/fd") {
        let mut fds = vec![];
        for entry in dir {
            if let Some(num) = entry
                .ok()
                .map(|e| e.file_name())
                .and_then(|s| s.into_string().ok())
                .and_then(|n| n.parse::<libc::c_int>().ok())
            {
                if num > 2 {
                    fds.push(num);
                }
            }
        }
        for fd in fds {
            unsafe {
                libc::close(fd);
            }
        }
    }
}

impl PtyFd {
    fn resize(&self, size: PtySize) -> Result<()> {
        let ws_size = winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };

        if unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &ws_size as *const _,
            )
        } != 0
        {
            return Err(Error::Io(io::Error::last_os_error()));
        }

        Ok(())
    }

    fn get_size(&self) -> Result<PtySize> {
        let mut size: winsize = unsafe { mem::zeroed() };
        if unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::TIOCGWINSZ as _,
                &mut size as *mut _,
            )
        } != 0
        {
            return Err(Error::Io(io::Error::last_os_error()));
        }
        Ok(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }

    fn spawn_command(&self, builder: CommandBuilder) -> Result<std::process::Child> {
        let configured_umask = builder.umask;

        let mut cmd = builder.as_command()?;
        let controlling_tty = builder.get_controlling_tty();

        unsafe {
            cmd.stdin(self.as_stdio()?)
                .stdout(self.as_stdio()?)
                .stderr(self.as_stdio()?)
                .pre_exec(move || {
                    for signo in &[
                        libc::SIGCHLD,
                        libc::SIGHUP,
                        libc::SIGINT,
                        libc::SIGQUIT,
                        libc::SIGTERM,
                        libc::SIGALRM,
                    ] {
                        libc::signal(*signo, libc::SIG_DFL);
                    }

                    let empty_set: libc::sigset_t = mem::zeroed();
                    libc::sigprocmask(libc::SIG_SETMASK, &empty_set, ptr::null_mut());

                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }

                    #[allow(clippy::cast_lossless)]
                    if controlling_tty
                        && libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }

                    close_random_fds();

                    if let Some(mask) = configured_umask {
                        libc::umask(mask);
                    }

                    Ok(())
                })
        };

        let mut child = cmd.spawn()?;

        child.stdin.take();
        child.stdout.take();
        child.stderr.take();

        Ok(child)
    }
}

/// Represents the master end of a pty.
pub struct UnixMasterPty {
    fd: PtyFd,
    took_writer: RefCell<bool>,
    tty_name: Option<PathBuf>,
}

/// Represents the slave end of a pty.
struct UnixSlavePty {
    fd: PtyFd,
}

fn cloexec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result == -1 {
        return Err(Error::Io(io::Error::last_os_error()));
    }
    Ok(())
}

impl SlavePty for UnixSlavePty {
    fn spawn_command(
        &self,
        builder: CommandBuilder,
    ) -> Result<Box<dyn Child + Send + Sync>> {
        Ok(Box::new(self.fd.spawn_command(builder)?))
    }
}

impl MasterPty for UnixMasterPty {
    fn resize(&self, size: PtySize) -> Result<()> {
        self.fd.resize(size)
    }

    fn get_size(&self) -> Result<PtySize> {
        self.fd.get_size()
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        let fd = PtyFd(self.fd.try_clone()?);
        Ok(Box::new(fd))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        if *self.took_writer.borrow() {
            return Err(Error::WriterAlreadyTaken);
        }
        *self.took_writer.borrow_mut() = true;
        let fd = PtyFd(self.fd.try_clone()?);
        Ok(Box::new(UnixMasterWriter { fd }))
    }

    fn as_raw_fd(&self) -> Option<i32> {
        Some(self.fd.0.as_raw_fd())
    }

    fn tty_name(&self) -> Option<PathBuf> {
        self.tty_name.clone()
    }

    fn process_group_leader(&self) -> Option<i32> {
        match unsafe { libc::tcgetpgrp(self.fd.0.as_raw_fd()) } {
            pid if pid > 0 => Some(pid),
            _ => None,
        }
    }
}

impl MasterPtyExt for UnixMasterPty {
    fn get_termios(&self) -> Option<nix::sys::termios::Termios> {
        nix::sys::termios::tcgetattr(self.fd.0.as_fd()).ok()
    }
}

/// Master writer that sends EOT on drop.
struct UnixMasterWriter {
    fd: PtyFd,
}

impl Drop for UnixMasterWriter {
    fn drop(&mut self) {
        // Use mem::zeroed() which is sound for libc::termios (a C struct of
        // primitive types and arrays).
        let mut t: libc::termios = unsafe { mem::zeroed() };
        if unsafe { libc::tcgetattr(self.fd.0.as_raw_fd(), &mut t) } == 0 {
            let eot = t.c_cc[libc::VEOF];
            if eot != 0 {
                let _ = self.fd.0.write_all(&[b'\n', eot]);
            }
        }
    }
}

impl Write for UnixMasterWriter {
    fn write(&mut self, buf: &[u8]) -> std::result::Result<usize, io::Error> {
        self.fd.write(buf)
    }
    fn flush(&mut self) -> std::result::Result<(), io::Error> {
        self.fd.flush()
    }
}
