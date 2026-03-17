# Changelog

## 0.1.0 (unreleased)

Initial release, forked from portable-pty 0.9.0 (wezterm).

### Changes from portable-pty

- Renamed crate from `portable-pty` to `xpty`
- Upgraded edition from 2018 to 2021
- Serial port support moved behind `serial` feature flag (off by default)
- Replaced internal `filedescriptor` path dependency with crates.io version
- Removed all wezterm-specific code and dependencies
