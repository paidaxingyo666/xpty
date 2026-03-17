//! Serial port based TTY implementation.
//!
//! This is different from the other implementations in that we cannot
//! explicitly spawn a process into the serial connection, so we can
//! only use a `CommandBuilder::new_default_prog` with the `openpty` method.

use crate::{
    Child, ChildKiller, CommandBuilder, Error, ExitStatus, MasterPty, PtyPair, PtySize, PtySystem,
    Result, SlavePty,
};
use filedescriptor::FileDescriptor;
use serial2::{CharSize, FlowControl, Parity, SerialPort, StopBits};
use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Result as IoResult, Write};
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

type Handle = Arc<SerialPort>;

pub struct SerialTty {
    port: OsString,
    baud: u32,
    char_size: CharSize,
    parity: Parity,
    stop_bits: StopBits,
    flow_control: FlowControl,
}

impl SerialTty {
    pub fn new<T: AsRef<OsStr> + ?Sized>(port: &T) -> Self {
        Self {
            port: port.as_ref().to_owned(),
            baud: 9600,
            char_size: CharSize::Bits8,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::XonXoff,
        }
    }

    pub fn set_baud_rate(&mut self, baud: u32) {
        self.baud = baud;
    }

    pub fn set_char_size(&mut self, char_size: CharSize) {
        self.char_size = char_size;
    }

    pub fn set_parity(&mut self, parity: Parity) {
        self.parity = parity;
    }

    pub fn set_stop_bits(&mut self, stop_bits: StopBits) {
        self.stop_bits = stop_bits;
    }

    pub fn set_flow_control(&mut self, flow_control: FlowControl) {
        self.flow_control = flow_control;
    }
}

impl PtySystem for SerialTty {
    fn openpty(&self, _size: PtySize) -> Result<PtyPair> {
        let mut port = SerialPort::open(&self.port, self.baud)
            .map_err(|e| Error::other(format!("openpty on serial port {:?}: {}", self.port, e)))?;

        let mut settings = port.get_configuration().map_err(Error::Io)?;
        settings.set_raw();
        settings.set_baud_rate(self.baud).map_err(Error::Io)?;
        settings.set_char_size(self.char_size);
        settings.set_flow_control(self.flow_control);
        settings.set_parity(self.parity);
        settings.set_stop_bits(self.stop_bits);
        log::debug!("serial settings: {:#?}", port.get_configuration());
        port.set_configuration(&settings).map_err(Error::Io)?;

        port.set_read_timeout(Duration::from_millis(50))
            .map_err(Error::Io)?;
        port.set_write_timeout(Duration::from_millis(50))
            .map_err(Error::Io)?;

        let port: Handle = Arc::new(port);

        Ok(PtyPair {
            slave: Box::new(Slave {
                port: Arc::clone(&port),
            }),
            master: Box::new(Master {
                port,
                took_writer: RefCell::new(false),
            }),
        })
    }
}

struct Slave {
    port: Handle,
}

impl SlavePty for Slave {
    fn spawn_command(&self, cmd: CommandBuilder) -> Result<Box<dyn Child + Send + Sync>> {
        if !cmd.is_default_prog() {
            return Err(Error::other(
                "can only use default prog commands with serial tty implementations",
            ));
        }
        Ok(Box::new(SerialChild {
            port: Arc::clone(&self.port),
        }))
    }
}

struct SerialChild {
    port: Handle,
}

impl std::fmt::Debug for SerialChild {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        fmt.debug_struct("SerialChild").finish()
    }
}

impl Child for SerialChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        Ok(None)
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        loop {
            std::thread::sleep(Duration::from_secs(5));

            if let Err(err) = self.port.read_cd() {
                log::error!("Error reading carrier detect: {:#}", err);
                return Ok(ExitStatus::with_exit_code(1));
            }
        }
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

impl ChildKiller for SerialChild {
    fn kill(&mut self) -> IoResult<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(SerialChildKiller)
    }
}

#[derive(Debug)]
struct SerialChildKiller;

impl ChildKiller for SerialChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(SerialChildKiller)
    }
}

struct Master {
    port: Handle,
    took_writer: RefCell<bool>,
}

struct MasterWriter {
    port: Handle,
}

impl Write for MasterWriter {
    fn write(&mut self, buf: &[u8]) -> std::result::Result<usize, std::io::Error> {
        self.port.write(buf)
    }

    fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
        self.port.flush()
    }
}

impl MasterPty for Master {
    fn resize(&self, _size: PtySize) -> Result<()> {
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize> {
        Ok(PtySize::default())
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        let fd = FileDescriptor::dup(&*self.port)?;
        Ok(Box::new(Reader { fd }))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>> {
        if *self.took_writer.borrow() {
            return Err(Error::WriterAlreadyTaken);
        }
        *self.took_writer.borrow_mut() = true;
        let port = Arc::clone(&self.port);
        Ok(Box::new(MasterWriter { port }))
    }

    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    fn as_raw_fd(&self) -> Option<i32> {
        None
    }

    fn tty_name(&self) -> Option<PathBuf> {
        None
    }
}

#[cfg(unix)]
impl crate::MasterPtyExt for Master {
    fn get_termios(&self) -> Option<nix::sys::termios::Termios> {
        None
    }
}

struct Reader {
    fd: FileDescriptor,
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::result::Result<usize, std::io::Error> {
        loop {
            #[cfg(unix)]
            {
                use filedescriptor::{poll, pollfd, AsRawSocketDescriptor, POLLIN};
                let mut poll_array = [pollfd {
                    fd: self.fd.as_socket_descriptor(),
                    events: POLLIN,
                    revents: 0,
                }];
                let _ = poll(&mut poll_array, None);
            }

            match self.fd.read(buf) {
                Ok(0) => {
                    if cfg!(windows) {
                        continue;
                    }
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "EOF on serial port",
                    ));
                }
                Ok(size) => return Ok(size),
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        continue;
                    }
                    log::error!("serial read error: {}", e);
                    return Err(e);
                }
            }
        }
    }
}
