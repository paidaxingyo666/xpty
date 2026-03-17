use xpty::{CommandBuilder, PtySize, PtySystem};

/// Helper: build a command that prints "hello" and exits.
fn echo_hello() -> CommandBuilder {
    #[cfg(unix)]
    {
        let mut cmd = CommandBuilder::new("echo");
        cmd.arg("hello");
        cmd
    }
    #[cfg(windows)]
    {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.arg("/C");
        cmd.arg("echo");
        cmd.arg("hello");
        cmd
    }
}

/// Drain reader in background to prevent ConPTY pipe deadlock on Windows.
fn drain_reader(reader: Box<dyn std::io::Read + Send>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut output = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        output
    })
}

/// Wait for child with try_wait polling + timeout.
/// WaitForSingleObject hangs on some Windows CI environments, so we
/// poll with try_wait() instead.
fn wait_for_child(
    child: &mut Box<dyn xpty::Child + Send + Sync>,
    timeout: std::time::Duration,
) -> xpty::ExitStatus {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(e) => panic!("try_wait error: {e}"),
        }
        if start.elapsed() > timeout {
            panic!(
                "child did not exit within {:.0}s",
                timeout.as_secs_f64()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn test_openpty() {
    let pty = xpty::native_pty_system();
    let _pair = pty.openpty(PtySize::default()).unwrap();
}

#[test]
fn test_resize() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let new_size = PtySize {
        rows: 50,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    };
    pair.master.resize(new_size).unwrap();

    let got = pair.master.get_size().unwrap();
    assert_eq!(got.rows, 50);
    assert_eq!(got.cols, 120);
}

#[test]
fn test_spawn_and_wait() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let cmd = echo_hello();
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().unwrap();
    let _drain = drain_reader(reader);

    let timeout = std::time::Duration::from_secs(30);
    let status = wait_for_child(&mut child, timeout);
    assert!(status.success());
}

#[test]
fn test_take_writer_twice_fails() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let _writer1 = pair.master.take_writer().unwrap();
    let result = pair.master.take_writer();
    assert!(result.is_err());
}

#[test]
fn test_reader_writer() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let cmd = echo_hello();
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().unwrap();
    let reader_handle = drain_reader(reader);

    let timeout = std::time::Duration::from_secs(30);
    let _status = wait_for_child(&mut child, timeout);

    // Drop master to close ConPTY — reader sees EOF
    drop(pair.master);

    let output = reader_handle.join().expect("reader thread panicked");
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("hello"), "got: {text:?}");
}

#[test]
fn test_default_prog() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let cmd = CommandBuilder::new_default_prog();
    assert!(cmd.is_default_prog());

    let mut child = pair.slave.spawn_command(cmd).unwrap();

    let reader = pair.master.try_clone_reader().unwrap();
    let _drain = drain_reader(reader);

    xpty::ChildKiller::kill(&mut *child).ok();
    let timeout = std::time::Duration::from_secs(30);
    let _ = wait_for_child(&mut child, timeout);
}

#[cfg(unix)]
#[test]
fn test_tty_name() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let name = pair.master.tty_name();
    assert!(name.is_some(), "expected a tty name on unix");
    let name = name.unwrap();
    assert!(
        name.to_string_lossy().contains("pts") || name.to_string_lossy().contains("tty"),
        "unexpected tty name: {:?}",
        name
    );
}

#[cfg(unix)]
#[test]
fn test_process_group_leader() {
    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let cmd = CommandBuilder::new("sleep");
    let mut child = pair
        .slave
        .spawn_command({
            let mut c = cmd;
            c.arg("0.1");
            c
        })
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(50));

    let pgl = pair.master.process_group_leader();
    assert!(pgl.is_some(), "expected a process group leader");

    child.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn test_get_termios() {
    use xpty::MasterPtyExt;

    let pty = xpty::native_pty_system();
    let pair = pty.openpty(PtySize::default()).unwrap();

    let master_ref: &dyn xpty::MasterPty = &*pair.master;
    let unix_master = master_ref
        .downcast_ref::<xpty::unix::UnixMasterPty>()
        .expect("should be UnixMasterPty");
    let termios = unix_master.get_termios();
    assert!(termios.is_some(), "expected termios on unix");
}

#[test]
fn test_command_builder_env() {
    let mut cmd = CommandBuilder::new("test");
    cmd.env("MY_VAR", "my_value");
    assert_eq!(
        cmd.get_env("MY_VAR"),
        Some(std::ffi::OsStr::new("my_value"))
    );

    cmd.env_remove("MY_VAR");
    assert_eq!(cmd.get_env("MY_VAR"), None);
}

#[test]
fn test_command_builder_cwd() {
    let mut cmd = CommandBuilder::new("test");
    assert!(cmd.get_cwd().is_none());

    cmd.cwd("/tmp");
    assert_eq!(cmd.get_cwd(), Some(&std::ffi::OsString::from("/tmp")));

    cmd.clear_cwd();
    assert!(cmd.get_cwd().is_none());
}
