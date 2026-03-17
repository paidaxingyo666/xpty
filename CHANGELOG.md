# Changelog

## 0.2.0

Major quality and modernization release.

### Breaking Changes

- **Custom error type**: Replaced `anyhow::Error` with `xpty::Error` enum — callers can now match specific error variants (`Io`, `WriterAlreadyTaken`, `CommandNotFound`, `NotExecutable`, etc.)
- **`native_pty_system()`** now returns concrete `NativePtySystem` instead of `Box<dyn PtySystem>`
- **`MasterPty` trait**: Unix-specific methods (`process_group_leader`, `as_raw_fd`, `tty_name`) now have cross-platform default impls returning `None`; `get_termios()` moved to `MasterPtyExt` extension trait
- **`SlavePty` trait**: Added `Send` bound
- **`PtyPair::slave`** type changed from `Box<dyn SlavePty + Send>` to `Box<dyn SlavePty>`
- **`get_argv()`** now returns `&[OsString]` instead of `&Vec<OsString>`
- Removed `anyhow` from public API and dependencies

### Bug Fixes

- **Fixed `WinChild::do_kill()` return value logic** — `TerminateProcess` nonzero return is success, not error
- **Fixed `WinChildKiller::kill()` same inverted logic**
- **Fixed `WinChild` Future impl** — no longer spawns a new thread on every `poll()`; uses `AtomicBool` + shared `Waker` to spawn waiter thread only once

### Dependency Modernization

- `winapi` → `windows-sys 0.59` (Microsoft official, actively maintained)
- `lazy_static` + `shared_library` → removed (ConPTY now statically linked via `windows-sys`)
- `bitflags 1.3` → removed (unused)
- `downcast-rs 1.0` → `2.0`
- `winreg 0.10` → `0.55`
- Added `thiserror 2` for error derive
- Removed `anyhow` dependency

### Improvements

- **Cached base environment**: `CommandBuilder` now caches the process environment snapshot via `OnceLock`, avoiding repeated cloning on each construction
- **`close_random_fds()` optimization**: Uses `close_range(2)` syscall on Linux 5.9+ for O(1) fd cleanup, with `/dev/fd` fallback for macOS and older kernels
- **Fixed all clippy warnings**: Redundant imports, field names, borrows, collapsible ifs, manual map, useless conversions
- **Renamed `psuedocon.rs` → `pseudocon.rs`** (spelling fix)
- **Safety comments** added to `unsafe impl Send/Sync for PseudoCon`
- **`MaybeUninit` cleanup**: Replaced unsound `MaybeUninit::zeroed().assume_init()` with `mem::zeroed()` for libc structs

### Testing

- Added 11 integration tests: openpty, resize, spawn, reader/writer, take_writer_twice, default_prog, tty_name, process_group_leader, get_termios, env, cwd
- Added GitHub Actions CI (Linux/macOS/Windows matrix, clippy, fmt, feature checks)

### Meta

- `rust-version = "1.70"` MSRV declared
- `edition = "2021"`

## 0.1.0

Initial release, forked from portable-pty 0.9.0 (wezterm).

### Changes from portable-pty

- Renamed crate from `portable-pty` to `xpty`
- Upgraded edition from 2018 to 2021
- Serial port support moved behind `serial` feature flag (off by default)
- Replaced internal `filedescriptor` path dependency with crates.io version
- Removed all wezterm-specific code and dependencies
