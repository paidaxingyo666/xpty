use crate::{Child, ChildKiller, ExitStatus};
use std::io::{Error as IoError, Result as IoResult};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessId, TerminateProcess, WaitForSingleObject,
};

pub mod conpty;
mod procthreadattr;
mod pseudocon;

use filedescriptor::OwnedHandle;

const STILL_ACTIVE: u32 = 259;

#[derive(Debug)]
pub struct WinChild {
    proc: Mutex<OwnedHandle>,
    /// Shared waker for the Future impl, updated on each poll.
    waker: Arc<Mutex<Option<Waker>>>,
    /// Whether we already spawned a waiter thread.
    wait_started: AtomicBool,
}

impl WinChild {
    pub(crate) fn new(proc_handle: OwnedHandle) -> Self {
        Self {
            proc: Mutex::new(proc_handle),
            waker: Arc::new(Mutex::new(None)),
            wait_started: AtomicBool::new(false),
        }
    }

    /// Query process exit status without blocking.
    /// Returns Ok(None) if still running, Ok(Some(status)) if exited.
    /// Returns Err if the system call fails (e.g. invalid handle).
    fn is_complete(&mut self) -> IoResult<Option<ExitStatus>> {
        let mut status: u32 = 0;
        let proc = self
            .proc
            .lock()
            .map_err(|e| IoError::other(e.to_string()))?;
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle(), &mut status) };
        if res != 0 {
            if status == STILL_ACTIVE {
                Ok(None)
            } else {
                Ok(Some(ExitStatus::with_exit_code(status)))
            }
        } else {
            // GetExitCodeProcess failed — surface the error
            Err(IoError::last_os_error())
        }
    }

    fn do_kill(&mut self) -> IoResult<()> {
        let proc = self
            .proc
            .lock()
            .map_err(|e| IoError::other(e.to_string()))?;
        let res = unsafe { TerminateProcess(proc.as_raw_handle(), 1) };
        if res == 0 {
            Err(IoError::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl ChildKiller for WinChild {
    fn kill(&mut self) -> IoResult<()> {
        self.do_kill().ok();
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self.proc.lock().unwrap().try_clone().unwrap();
        Box::new(WinChildKiller { proc })
    }
}

#[derive(Debug)]
pub struct WinChildKiller {
    proc: OwnedHandle,
}

impl ChildKiller for WinChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        let res = unsafe { TerminateProcess(self.proc.as_raw_handle(), 1) };
        if res == 0 {
            Err(IoError::last_os_error())
        } else {
            Ok(())
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self.proc.try_clone().unwrap();
        Box::new(WinChildKiller { proc })
    }
}

impl Child for WinChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        self.is_complete()
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        // Fast path: already exited
        if let Some(status) = self.try_wait()? {
            return Ok(status);
        }

        // Use try_wait polling with WaitForSingleObject(100ms) as sleep.
        // We avoid INFINITE because ConPTY can prevent WaitForSingleObject
        // from ever returning in some configurations (the conhost process
        // keeps a reference to the child's console, and certain pipe buffer
        // states can deadlock).
        loop {
            {
                let proc = self
                    .proc
                    .lock()
                    .map_err(|e| IoError::other(e.to_string()))?;
                unsafe {
                    WaitForSingleObject(proc.as_raw_handle(), 100);
                }
            }
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
        }
    }

    fn process_id(&self) -> Option<u32> {
        let res = unsafe { GetProcessId(self.proc.lock().unwrap().as_raw_handle()) };
        if res == 0 {
            None
        } else {
            Some(res)
        }
    }

    fn as_raw_handle(&self) -> Option<RawHandle> {
        let proc = self.proc.lock().unwrap();
        Some(proc.as_raw_handle())
    }
}

impl std::future::Future for WinChild {
    type Output = crate::Result<ExitStatus>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<crate::Result<ExitStatus>> {
        match self.is_complete() {
            Ok(Some(status)) => Poll::Ready(Ok(status)),
            Err(err) => Poll::Ready(Err(crate::Error::Io(err))),
            Ok(None) => {
                *self.waker.lock().unwrap() = Some(cx.waker().clone());

                if !self.wait_started.swap(true, Ordering::SeqCst) {
                    let proc = match self.proc.lock() {
                        Ok(p) => match p.try_clone() {
                            Ok(p) => p,
                            Err(e) => return Poll::Ready(Err(crate::Error::Io(IoError::other(e)))),
                        },
                        Err(e) => return Poll::Ready(Err(crate::Error::other(e.to_string()))),
                    };

                    let handle_val = proc.as_raw_handle() as usize;
                    struct SendProc(#[allow(dead_code)] OwnedHandle);
                    unsafe impl Send for SendProc {}
                    let send_proc = SendProc(proc);
                    let waker = Arc::clone(&self.waker);
                    std::thread::spawn(move || {
                        // Poll with short timeouts instead of INFINITE
                        loop {
                            let ret = unsafe { WaitForSingleObject(handle_val as RawHandle, 500) };
                            // WAIT_OBJECT_0 (0) = signaled, WAIT_TIMEOUT (258) = retry
                            if ret != 258 {
                                break;
                            }
                        }
                        drop(send_proc);
                        if let Some(w) = waker.lock().unwrap().take() {
                            w.wake();
                        }
                    });
                }
                Poll::Pending
            }
        }
    }
}
