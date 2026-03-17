#[cfg(feature = "serde_support")]
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(unix)]
use std::path::Component;
use std::path::Path;
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Used to deal with Windows having case-insensitive environment variables.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde_support", derive(Serialize, Deserialize))]
struct EnvEntry {
    is_from_base_env: bool,
    preferred_key: OsString,
    value: OsString,
}

impl EnvEntry {
    fn map_key(k: OsString) -> OsString {
        if cfg!(windows) {
            match k.to_str() {
                Some(s) => s.to_lowercase().into(),
                None => k,
            }
        } else {
            k
        }
    }
}

#[cfg(unix)]
fn get_shell() -> String {
    use nix::unistd::{access, AccessFlags};
    use std::ffi::CStr;
    use std::str;

    let ent = unsafe { libc::getpwuid(libc::getuid()) };
    if !ent.is_null() {
        let shell = unsafe { CStr::from_ptr((*ent).pw_shell) };
        match shell.to_str().map(str::to_owned) {
            Err(err) => {
                log::warn!(
                    "passwd database shell could not be \
                     represented as utf-8: {err:#}, \
                     falling back to /bin/sh"
                );
            }
            Ok(shell) => {
                if let Err(err) = access(Path::new(&shell), AccessFlags::X_OK) {
                    log::warn!(
                        "passwd database shell={shell:?} which is \
                         not executable ({err:#}), falling back to /bin/sh"
                    );
                } else {
                    return shell;
                }
            }
        }
    }
    "/bin/sh".into()
}

/// Returns a cached snapshot of the base environment.
fn get_base_env() -> BTreeMap<OsString, EnvEntry> {
    static BASE_ENV: OnceLock<BTreeMap<OsString, EnvEntry>> = OnceLock::new();
    BASE_ENV
        .get_or_init(|| {
            let mut env: BTreeMap<OsString, EnvEntry> = std::env::vars_os()
                .map(|(key, value)| {
                    (
                        EnvEntry::map_key(key.clone()),
                        EnvEntry {
                            is_from_base_env: true,
                            preferred_key: key,
                            value,
                        },
                    )
                })
                .collect();

            #[cfg(unix)]
            {
                let key = EnvEntry::map_key("SHELL".into());
                if !env.contains_key(&key) {
                    env.insert(
                        EnvEntry::map_key("SHELL".into()),
                        EnvEntry {
                            is_from_base_env: true,
                            preferred_key: "SHELL".into(),
                            value: get_shell().into(),
                        },
                    );
                }
            }

            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
                use winreg::enums::{RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
                use winreg::types::FromRegValue;
                use winreg::{RegKey, RegValue};

                fn reg_value_to_string(value: &RegValue) -> std::result::Result<OsString, Box<dyn std::error::Error>> {
                    match value.vtype {
                        RegType::REG_EXPAND_SZ => {
                            let src = unsafe {
                                std::slice::from_raw_parts(
                                    value.bytes.as_ptr() as *const u16,
                                    value.bytes.len() / 2,
                                )
                            };
                            let size = unsafe {
                                ExpandEnvironmentStringsW(src.as_ptr(), std::ptr::null_mut(), 0)
                            };
                            let mut buf = vec![0u16; size as usize + 1];
                            unsafe {
                                ExpandEnvironmentStringsW(
                                    src.as_ptr(),
                                    buf.as_mut_ptr(),
                                    buf.len() as u32,
                                )
                            };

                            let mut buf = buf.as_slice();
                            while let Some(0) = buf.last() {
                                buf = &buf[0..buf.len() - 1];
                            }
                            Ok(OsString::from_wide(buf))
                        }
                        _ => Ok(OsString::from_reg_value(value)?),
                    }
                }

                if let Ok(sys_env) = RegKey::predef(HKEY_LOCAL_MACHINE)
                    .open_subkey("System\\CurrentControlSet\\Control\\Session Manager\\Environment")
                {
                    for res in sys_env.enum_values() {
                        if let Ok((name, value)) = res {
                            if name.to_ascii_lowercase() == "username" {
                                continue;
                            }
                            if let Ok(value) = reg_value_to_string(&value) {
                                log::trace!("adding SYS env: {:?} {:?}", name, value);
                                env.insert(
                                    EnvEntry::map_key(name.clone().into()),
                                    EnvEntry {
                                        is_from_base_env: true,
                                        preferred_key: name.into(),
                                        value,
                                    },
                                );
                            }
                        }
                    }
                }

                if let Ok(sys_env) =
                    RegKey::predef(HKEY_CURRENT_USER).open_subkey("Environment")
                {
                    for res in sys_env.enum_values() {
                        if let Ok((name, value)) = res {
                            if let Ok(value) = reg_value_to_string(&value) {
                                let value = if name.to_ascii_lowercase() == "path" {
                                    match env.get(&EnvEntry::map_key(name.clone().into())) {
                                        Some(entry) => {
                                            let mut result = OsString::new();
                                            result.push(&entry.value);
                                            result.push(";");
                                            result.push(&value);
                                            result
                                        }
                                        None => value,
                                    }
                                } else {
                                    value
                                };

                                log::trace!("adding USER env: {:?} {:?}", name, value);
                                env.insert(
                                    EnvEntry::map_key(name.clone().into()),
                                    EnvEntry {
                                        is_from_base_env: true,
                                        preferred_key: name.into(),
                                        value,
                                    },
                                );
                            }
                        }
                    }
                }
            }

            env
        })
        .clone()
}

/// `CommandBuilder` is used to prepare a command to be spawned into a pty.
/// The interface is intentionally similar to that of `std::process::Command`.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde_support", derive(Serialize, Deserialize))]
pub struct CommandBuilder {
    args: Vec<OsString>,
    envs: BTreeMap<OsString, EnvEntry>,
    cwd: Option<OsString>,
    #[cfg(unix)]
    pub(crate) umask: Option<libc::mode_t>,
    controlling_tty: bool,
    /// User-supplied pre_exec hooks (unix only).
    /// Stored separately to avoid breaking Clone/PartialEq.
    #[cfg(unix)]
    #[cfg_attr(feature = "serde_support", serde(skip))]
    pub(crate) pre_exec_hooks: PreExecHooks,
}

/// Wrapper for pre_exec closures that doesn't participate in Clone/PartialEq.
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct PreExecHooks {
    pub(crate) hooks: Vec<Box<dyn FnMut() -> std::result::Result<(), std::io::Error> + Send + Sync>>,
}

#[cfg(unix)]
impl Clone for PreExecHooks {
    fn clone(&self) -> Self {
        // Cannot clone closures; the clone gets an empty hook list.
        // This is acceptable — hooks are typically set just before spawn.
        PreExecHooks { hooks: Vec::new() }
    }
}

#[cfg(unix)]
impl PartialEq for PreExecHooks {
    fn eq(&self, other: &Self) -> bool {
        self.hooks.len() == other.hooks.len()
    }
}

#[cfg(unix)]
impl std::fmt::Debug for PreExecHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreExecHooks")
            .field("count", &self.hooks.len())
            .finish()
    }
}

impl CommandBuilder {
    /// Create a new builder instance with argv\[0\] set to the specified
    /// program.
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            args: vec![program.as_ref().to_owned()],
            envs: get_base_env(),
            cwd: None,
            #[cfg(unix)]
            umask: None,
            controlling_tty: true,
            #[cfg(unix)]
            pre_exec_hooks: PreExecHooks::default(),
        }
    }

    /// Create a new builder instance from a pre-built argument vector.
    pub fn from_argv(args: Vec<OsString>) -> Self {
        Self {
            args,
            envs: get_base_env(),
            cwd: None,
            #[cfg(unix)]
            umask: None,
            controlling_tty: true,
            #[cfg(unix)]
            pre_exec_hooks: PreExecHooks::default(),
        }
    }

    /// Set whether we should set the pty as the controlling terminal.
    /// The default is true, which is usually what you want, but you
    /// may need to set this to false if you are crossing container
    /// boundaries (eg: flatpak).
    pub fn set_controlling_tty(&mut self, controlling_tty: bool) {
        self.controlling_tty = controlling_tty;
    }

    pub fn get_controlling_tty(&self) -> bool {
        self.controlling_tty
    }

    /// Create a new builder instance that will run the default program
    /// (typically the user's shell). Will panic if `arg` is called on it.
    pub fn new_default_prog() -> Self {
        Self {
            args: vec![],
            envs: get_base_env(),
            cwd: None,
            #[cfg(unix)]
            umask: None,
            controlling_tty: true,
            #[cfg(unix)]
            pre_exec_hooks: PreExecHooks::default(),
        }
    }

    /// Returns true if this builder was created via `new_default_prog`.
    pub fn is_default_prog(&self) -> bool {
        self.args.is_empty()
    }

    /// Append an argument to the current command line.
    ///
    /// # Panics
    ///
    /// Panics if called on a builder created via [`new_default_prog`](Self::new_default_prog).
    /// Use [`is_default_prog`](Self::is_default_prog) to check first, or create the
    /// builder with [`new`](Self::new) instead.
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) {
        if self.is_default_prog() {
            panic!("attempted to add args to a default_prog builder");
        }
        self.args.push(arg.as_ref().to_owned());
    }

    /// Set the actual program for a default_prog builder.
    ///
    /// # Panics
    ///
    /// Panics if called on a builder that was NOT created via
    /// [`new_default_prog`](Self::new_default_prog).
    pub fn replace_default_prog(&mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) {
        if !self.is_default_prog() {
            panic!("attempted to replace_default_prog on a non-default_prog builder");
        }
        for arg in args {
            self.args.push(arg.as_ref().to_owned());
        }
    }

    /// Append a sequence of arguments to the current command line.
    pub fn args<I, S>(&mut self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg);
        }
    }

    pub fn get_argv(&self) -> &[OsString] {
        &self.args
    }

    pub fn get_argv_mut(&mut self) -> &mut Vec<OsString> {
        &mut self.args
    }

    /// Override the value of an environmental variable.
    pub fn env<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key: OsString = key.as_ref().into();
        let value: OsString = value.as_ref().into();
        self.envs.insert(
            EnvEntry::map_key(key.clone()),
            EnvEntry {
                is_from_base_env: false,
                preferred_key: key,
                value,
            },
        );
    }

    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) {
        let key = key.as_ref().into();
        self.envs.remove(&EnvEntry::map_key(key));
    }

    pub fn env_clear(&mut self) {
        self.envs.clear();
    }

    pub fn get_env<K: AsRef<OsStr>>(&self, key: K) -> Option<&OsStr> {
        let key = key.as_ref().into();
        self.envs
            .get(&EnvEntry::map_key(key))
            .map(|entry| entry.value.as_os_str())
    }

    pub fn cwd<D: AsRef<OsStr>>(&mut self, dir: D) {
        self.cwd = Some(dir.as_ref().to_owned());
    }

    pub fn clear_cwd(&mut self) {
        self.cwd.take();
    }

    pub fn get_cwd(&self) -> Option<&OsString> {
        self.cwd.as_ref()
    }

    /// Iterate over the configured environment. Only includes environment
    /// variables set by the caller via `env`, not variables set in the base
    /// environment.
    pub fn iter_extra_env_as_str(&self) -> impl Iterator<Item = (&str, &str)> {
        self.envs.values().filter_map(|entry| {
            if entry.is_from_base_env {
                None
            } else {
                let key = entry.preferred_key.to_str()?;
                let value = entry.value.to_str()?;
                Some((key, value))
            }
        })
    }

    pub fn iter_full_env_as_str(&self) -> impl Iterator<Item = (&str, &str)> {
        self.envs.values().filter_map(|entry| {
            let key = entry.preferred_key.to_str()?;
            let value = entry.value.to_str()?;
            Some((key, value))
        })
    }

    /// Return the configured command and arguments as a single string,
    /// quoted per the unix shell conventions.
    pub fn as_unix_command_line(&self) -> Result<String> {
        let mut strs = vec![];
        for arg in &self.args {
            let s = arg.to_str().ok_or(Error::InvalidUtf8)?;
            strs.push(s);
        }
        Ok(shell_words::join(strs))
    }
}

#[cfg(unix)]
impl CommandBuilder {
    /// Register a closure to be called in the child process after `fork()`
    /// but before `exec()`.
    ///
    /// This is useful for operations like setting uid/gid, entering namespaces,
    /// or adjusting resource limits. Multiple hooks are called in registration order,
    /// **after** xpty's own setup (signal reset, `setsid`, `TIOCSCTTY`, fd cleanup,
    /// umask).
    ///
    /// # Safety
    ///
    /// The closure executes in a forked-but-not-yet-exec'd child process.
    /// At this point the child is a single-threaded copy of a potentially
    /// multi-threaded parent, so **only async-signal-safe functions may be
    /// called** (POSIX.1-2008 §2.4.3).
    ///
    /// ## What is safe
    ///
    /// - Raw syscalls and libc wrappers documented as async-signal-safe:
    ///   `setsid`, `setuid`, `setgid`, `dup2`, `close`, `ioctl`, `chroot`,
    ///   `chdir`, `umask`, `write` (to an fd, not `std::io::Write`), etc.
    /// - Reading/writing to memory that was set up before `fork()`.
    ///
    /// ## What is NOT safe
    ///
    /// - **Heap allocation** (`malloc` / `Box::new` / `Vec::push` / `String`
    ///   formatting) — the allocator may hold a lock from another thread that
    ///   was not forked.
    /// - **Acquiring any lock** (`Mutex`, `RwLock`, `println!`) — same reason.
    /// - **Anything in `std::io`** that allocates or locks internally
    ///   (e.g., `BufWriter`, `stdout()`).
    /// - **Calling non-async-signal-safe libc functions** such as `getpwnam`,
    ///   `dlopen`, `openlog`.
    ///
    /// Violating these constraints leads to undefined behavior — typically
    /// a deadlock in the child that is extremely hard to diagnose.
    ///
    /// For the full list of async-signal-safe functions see
    /// [`signal-safety(7)`](https://man7.org/linux/man-pages/man7/signal-safety.7.html)
    /// and [`std::os::unix::process::CommandExt::pre_exec`].
    pub unsafe fn pre_exec<F>(&mut self, hook: F)
    where
        F: FnMut() -> std::result::Result<(), std::io::Error> + Send + Sync + 'static,
    {
        self.pre_exec_hooks.hooks.push(Box::new(hook));
    }

    pub fn umask(&mut self, mask: Option<libc::mode_t>) {
        self.umask = mask;
    }

    fn resolve_path(&self) -> Option<&OsStr> {
        self.get_env("PATH")
    }

    fn search_path(&self, exe: &OsStr, cwd: &OsStr) -> Result<OsString> {
        use nix::unistd::{access, AccessFlags};

        let exe_path: &Path = exe.as_ref();
        if exe_path.is_relative() {
            let cwd: &Path = cwd.as_ref();

            if is_cwd_relative_path(exe_path) {
                let abs_path = cwd.join(exe_path);

                if abs_path.is_dir() {
                    return Err(Error::IsDirectory(abs_path.display().to_string()));
                } else if access(&abs_path, AccessFlags::X_OK).is_ok() {
                    return Ok(abs_path.into_os_string());
                } else if access(&abs_path, AccessFlags::F_OK).is_ok() {
                    return Err(Error::NotExecutable(abs_path.display().to_string()));
                }

                return Err(Error::CommandNotFound(abs_path.display().to_string()));
            }

            let mut errors = vec![];
            if let Some(path) = self.resolve_path() {
                for path in std::env::split_paths(&path) {
                    let candidate = cwd.join(&path).join(exe);

                    if candidate.is_dir() {
                        errors.push(format!("{} exists but is a directory", candidate.display()));
                    } else if access(&candidate, AccessFlags::X_OK).is_ok() {
                        return Ok(candidate.into_os_string());
                    } else if access(&candidate, AccessFlags::F_OK).is_ok() {
                        errors.push(format!(
                            "{} exists but is not executable",
                            candidate.display()
                        ));
                    }
                }
                errors.push(format!("No viable candidates found in PATH {path:?}"));
            } else {
                errors.push("Unable to resolve the PATH".to_string());
            }
            Err(Error::PathResolution(format!(
                "Unable to spawn {}: {}",
                exe_path.display(),
                errors.join(".\n")
            )))
        } else if exe_path.is_dir() {
            Err(Error::IsDirectory(exe_path.display().to_string()))
        } else {
            if let Err(err) = access(exe_path, AccessFlags::X_OK) {
                if access(exe_path, AccessFlags::F_OK).is_ok() {
                    return Err(Error::NotExecutable(format!(
                        "{} ({err:#})",
                        exe_path.display()
                    )));
                } else {
                    return Err(Error::CommandNotFound(format!(
                        "{} ({err:#})",
                        exe_path.display()
                    )));
                }
            }

            Ok(exe.to_owned())
        }
    }

    /// Convert the CommandBuilder to a `std::process::Command` instance.
    pub(crate) fn as_command(&self) -> Result<std::process::Command> {
        use std::os::unix::process::CommandExt;

        let home = self.get_home_dir();
        let dir: &OsStr = self
            .cwd
            .as_deref()
            .filter(|dir| std::path::Path::new(dir).is_dir())
            .unwrap_or(home.as_ref());
        let shell = self.get_shell();

        let mut cmd = if self.is_default_prog() {
            let mut cmd = std::process::Command::new(&shell);
            let basename = shell.rsplit('/').next().unwrap_or(&shell);
            cmd.arg0(format!("-{}", basename));
            cmd
        } else {
            let resolved = self.search_path(&self.args[0], dir)?;
            let mut cmd = std::process::Command::new(&resolved);
            cmd.arg0(&self.args[0]);
            cmd.args(&self.args[1..]);
            cmd
        };

        cmd.current_dir(dir);
        cmd.env_clear();
        cmd.env("SHELL", shell);
        cmd.envs(
            self.envs
                .values()
                .map(|entry| (entry.preferred_key.as_os_str(), entry.value.as_os_str())),
        );

        Ok(cmd)
    }

    /// Determine which shell to run.
    pub fn get_shell(&self) -> String {
        use nix::unistd::{access, AccessFlags};

        if let Some(shell) = self.get_env("SHELL").and_then(OsStr::to_str) {
            match access(shell, AccessFlags::X_OK) {
                Ok(()) => return shell.into(),
                Err(err) => log::warn!(
                    "$SHELL -> {shell:?} which is \
                     not executable ({err:#}), falling back to password db lookup"
                ),
            }
        }

        get_shell()
    }

    fn get_home_dir(&self) -> String {
        if let Some(home_dir) = self.get_env("HOME").and_then(OsStr::to_str) {
            return home_dir.into();
        }

        let ent = unsafe { libc::getpwuid(libc::getuid()) };
        if ent.is_null() {
            "/".into()
        } else {
            use std::ffi::CStr;
            use std::str;
            let home = unsafe { CStr::from_ptr((*ent).pw_dir) };
            home.to_str().map(str::to_owned).unwrap_or_else(|_| "/".into())
        }
    }
}

#[cfg(windows)]
impl CommandBuilder {
    fn search_path(&self, exe: &OsStr) -> OsString {
        if let Some(path) = self.get_env("PATH") {
            let extensions = self.get_env("PATHEXT").unwrap_or(OsStr::new(".EXE"));
            for path in std::env::split_paths(&path) {
                let candidate = path.join(exe);
                if candidate.exists() {
                    return candidate.into_os_string();
                }

                for ext in std::env::split_paths(&extensions) {
                    let ext = ext.to_str().expect("PATHEXT entries must be utf8");
                    let path = path.join(exe).with_extension(&ext[1..]);
                    if path.exists() {
                        return path.into_os_string();
                    }
                }
            }
        }

        exe.to_owned()
    }

    pub(crate) fn current_directory(&self) -> Option<Vec<u16>> {
        let home: Option<&OsStr> = self
            .get_env("USERPROFILE")
            .filter(|path| Path::new(path).is_dir());
        let cwd: Option<&OsStr> = self.cwd.as_deref().filter(|path| Path::new(path).is_dir());
        let dir: Option<&OsStr> = cwd.or(home);

        dir.map(|dir| {
            let mut wide = vec![];

            if Path::new(dir).is_relative() {
                if let Ok(ccwd) = std::env::current_dir() {
                    wide.extend(ccwd.join(dir).as_os_str().encode_wide());
                } else {
                    wide.extend(dir.encode_wide());
                }
            } else {
                wide.extend(dir.encode_wide());
            }

            wide.push(0);
            wide
        })
    }

    pub(crate) fn environment_block(&self) -> Vec<u16> {
        let mut block = vec![];

        for entry in self.envs.values() {
            block.extend(entry.preferred_key.encode_wide());
            block.push(b'=' as u16);
            block.extend(entry.value.encode_wide());
            block.push(0);
        }
        block.push(0);

        block
    }

    pub fn get_shell(&self) -> String {
        let exe: OsString = self
            .get_env("ComSpec")
            .unwrap_or(OsStr::new("cmd.exe"))
            .into();
        exe.into_string()
            .unwrap_or_else(|_| "%CompSpec%".to_string())
    }

    pub(crate) fn cmdline(&self) -> Result<(Vec<u16>, Vec<u16>)> {
        let mut cmdline = Vec::<u16>::new();

        let exe: OsString = if self.is_default_prog() {
            self.get_env("ComSpec")
                .unwrap_or(OsStr::new("cmd.exe"))
                .into()
        } else {
            self.search_path(&self.args[0])
        };

        Self::append_quoted(&exe, &mut cmdline);

        let mut exe: Vec<u16> = exe.encode_wide().collect();
        exe.push(0);

        for arg in self.args.iter().skip(1) {
            cmdline.push(' ' as u16);
            if arg.encode_wide().any(|c| c == 0) {
                return Err(Error::other(format!(
                    "invalid encoding for command line argument {:?}",
                    arg
                )));
            }
            Self::append_quoted(arg, &mut cmdline);
        }
        cmdline.push(0);
        Ok((exe, cmdline))
    }

    fn append_quoted(arg: &OsStr, cmdline: &mut Vec<u16>) {
        if !arg.is_empty()
            && !arg.encode_wide().any(|c| {
                c == ' ' as u16
                    || c == '\t' as u16
                    || c == '\n' as u16
                    || c == '\x0b' as u16
                    || c == '\"' as u16
            })
        {
            cmdline.extend(arg.encode_wide());
            return;
        }
        cmdline.push('"' as u16);

        let arg: Vec<_> = arg.encode_wide().collect();
        let mut i = 0;
        while i < arg.len() {
            let mut num_backslashes = 0;
            while i < arg.len() && arg[i] == '\\' as u16 {
                i += 1;
                num_backslashes += 1;
            }

            if i == arg.len() {
                for _ in 0..num_backslashes * 2 {
                    cmdline.push('\\' as u16);
                }
                break;
            } else if arg[i] == b'"' as u16 {
                for _ in 0..num_backslashes * 2 + 1 {
                    cmdline.push('\\' as u16);
                }
                cmdline.push(arg[i]);
            } else {
                for _ in 0..num_backslashes {
                    cmdline.push('\\' as u16);
                }
                cmdline.push(arg[i]);
            }
            i += 1;
        }
        cmdline.push('"' as u16);
    }
}

#[cfg(unix)]
fn is_cwd_relative_path<P: AsRef<Path>>(p: P) -> bool {
    matches!(
        p.as_ref().components().next(),
        Some(Component::CurDir | Component::ParentDir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_cwd_relative() {
        assert!(is_cwd_relative_path("."));
        assert!(is_cwd_relative_path("./foo"));
        assert!(is_cwd_relative_path("../foo"));
        assert!(!is_cwd_relative_path("foo"));
        assert!(!is_cwd_relative_path("/foo"));
    }

    #[test]
    fn test_env() {
        let mut cmd = CommandBuilder::new("dummy");
        let package_name = cmd.get_env("CARGO_PKG_NAME");
        assert_eq!(package_name, Some(OsStr::new("xpty")));

        cmd.env("foo key", "foo value");
        cmd.env("bar key", "bar value");

        let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
        assert_eq!(
            iterated_envs,
            vec![("bar key", "bar value"), ("foo key", "foo value")]
        );

        {
            let mut cmd = cmd.clone();
            cmd.env_remove("foo key");
            let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
            assert_eq!(iterated_envs, vec![("bar key", "bar value")]);
        }

        {
            let mut cmd = cmd.clone();
            cmd.env_remove("bar key");
            let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
            assert_eq!(iterated_envs, vec![("foo key", "foo value")]);
        }

        {
            let mut cmd = cmd.clone();
            cmd.env_clear();
            let iterated_envs = cmd.iter_extra_env_as_str().collect::<Vec<_>>();
            assert!(iterated_envs.is_empty());
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_env_case_insensitive_override() {
        let mut cmd = CommandBuilder::new("dummy");
        cmd.env("Cargo_Pkg_Authors", "Not Wez");
        assert!(cmd.get_env("cargo_pkg_authors") == Some(OsStr::new("Not Wez")));

        cmd.env_remove("cARGO_pKG_aUTHORS");
        assert!(cmd.get_env("CARGO_PKG_AUTHORS").is_none());
    }
}
