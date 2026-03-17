# xpty

Cross-platform async-ready PTY (pseudo terminal) interface for Rust.

Forked from [portable-pty](https://github.com/wezterm/wezterm/tree/main/pty) (part of [wezterm](https://github.com/wezterm/wezterm)) by Wez Furlong.

## Features

- **Cross-platform**: Unix (openpty) and Windows (ConPTY) support
- **Trait-based**: Runtime selection of PTY implementations via `PtySystem` trait
- **Serial port support**: Optional serial port TTY via `serial` feature

## Usage

```rust
use xpty::{CommandBuilder, PtySize, native_pty_system, PtySystem};

let pty_system = native_pty_system();

let mut pair = pty_system.openpty(PtySize {
    rows: 24,
    cols: 80,
    pixel_width: 0,
    pixel_height: 0,
})?;

let cmd = CommandBuilder::new("bash");
let child = pair.slave.spawn_command(cmd)?;

let mut reader = pair.master.try_clone_reader()?;
writeln!(pair.master.take_writer()?, "ls -l\r\n")?;
```

## Optional Features

| Feature | Description |
|---------|-------------|
| `serial` | Serial port TTY support via `serial2` |
| `serde_support` | Serde serialization for `PtySize` and `CommandBuilder` |

## Relationship to portable-pty

xpty is a fork of portable-pty 0.9.0 with the goal of becoming a more modern, independent cross-platform PTY library. Planned improvements include:

- Async support (tokio/async-std)
- Better Windows ConPTY control
- Improved error types
- Modern Rust idioms (edition 2021+)

## License

MIT - see [LICENSE.md](LICENSE.md)

Original code by Wez Furlong. See the git history for full attribution.
