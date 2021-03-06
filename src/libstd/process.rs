// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A module for working with processes.
//!
//! This module is mostly concerned with spawning and interacting with child
//! processes, but it also provides [`abort`] and [`exit`] for terminating the
//! current process.
//!
//! # Spawning a process
//!
//! The [`Command`] struct is used to configure and spawn processes:
//!
//! ```
//! use std::process::Command;
//!
//! let output = Command::new("echo")
//!                      .arg("Hello world")
//!                      .output()
//!                      .expect("Failed to execute command");
//!
//! assert_eq!(b"Hello world\n", output.stdout.as_slice());
//! ```
//!
//! Several methods on [`Command`], such as [`spawn`] or [`output`], can be used
//! to spawn a process. In particular, [`output`] spawns the child process and
//! waits until the process terminates, while [`spawn`] will return a [`Child`]
//! that represents the spawned child process.
//!
//! # Handling I/O
//!
//! The [`stdout`], [`stdin`], and [`stderr`] of a child process can be
//! configured by passing an [`Stdio`] to the corresponding method on
//! [`Command`]. Once spawned, they can be accessed from the [`Child`]. For
//! example, piping output from one command into another command can be done
//! like so:
//!
//! ```no_run
//! use std::process::{Command, Stdio};
//!
//! // stdout must be configured with `Stdio::piped` in order to use
//! // `echo_child.stdout`
//! let echo_child = Command::new("echo")
//!     .arg("Oh no, a tpyo!")
//!     .stdout(Stdio::piped())
//!     .spawn()
//!     .expect("Failed to start echo process");
//!
//! // Note that `echo_child` is moved here, but we won't be needing
//! // `echo_child` anymore
//! let echo_out = echo_child.stdout.expect("Failed to open echo stdout");
//!
//! let mut sed_child = Command::new("sed")
//!     .arg("s/tpyo/typo/")
//!     .stdin(Stdio::from(echo_out))
//!     .stdout(Stdio::piped())
//!     .spawn()
//!     .expect("Failed to start sed process");
//!
//! let output = sed_child.wait_with_output().expect("Failed to wait on sed");
//! assert_eq!(b"Oh no, a typo!\n", output.stdout.as_slice());
//! ```
//!
//! Note that [`ChildStderr`] and [`ChildStdout`] implement [`Read`] and
//! [`ChildStdin`] implements [`Write`]:
//!
//! ```no_run
//! use std::process::{Command, Stdio};
//! use std::io::Write;
//!
//! let mut child = Command::new("/bin/cat")
//!     .stdin(Stdio::piped())
//!     .stdout(Stdio::piped())
//!     .spawn()
//!     .expect("failed to execute child");
//!
//! {
//!     // limited borrow of stdin
//!     let stdin = child.stdin.as_mut().expect("failed to get stdin");
//!     stdin.write_all(b"test").expect("failed to write to stdin");
//! }
//!
//! let output = child
//!     .wait_with_output()
//!     .expect("failed to wait on child");
//!
//! assert_eq!(b"test", output.stdout.as_slice());
//! ```
//!
//! [`abort`]: fn.abort.html
//! [`exit`]: fn.exit.html
//!
//! [`Command`]: struct.Command.html
//! [`spawn`]: struct.Command.html#method.spawn
//! [`output`]: struct.Command.html#method.output
//!
//! [`Child`]: struct.Child.html
//! [`ChildStdin`]: struct.ChildStdin.html
//! [`ChildStdout`]: struct.ChildStdout.html
//! [`ChildStderr`]: struct.ChildStderr.html
//! [`Stdio`]: struct.Stdio.html
//!
//! [`stdout`]: struct.Command.html#method.stdout
//! [`stdin`]: struct.Command.html#method.stdin
//! [`stderr`]: struct.Command.html#method.stderr
//!
//! [`Write`]: ../io/trait.Write.html
//! [`Read`]: ../io/trait.Read.html

#![stable(feature = "process", since = "1.0.0")]

use io::prelude::*;

use ffi::OsStr;
use fmt;
use fs;
use io::{self, Initializer};
use path::Path;
use str;
use sys::pipe::{read2, AnonPipe};
use sys::process as imp;
use sys_common::{AsInner, AsInnerMut, FromInner, IntoInner};

/// Representation of a running or exited child process.
///
/// This structure is used to represent and manage child processes. A child
/// process is created via the [`Command`] struct, which configures the
/// spawning process and can itself be constructed using a builder-style
/// interface.
///
/// There is no implementation of [`Drop`] for child processes,
/// so if you do not ensure the `Child` has exited then it will continue to
/// run, even after the `Child` handle to the child process has gone out of
/// scope.
///
/// Calling [`wait`](#method.wait) (or other functions that wrap around it) will make
/// the parent process wait until the child has actually exited before
/// continuing.
///
/// # Examples
///
/// ```should_panic
/// use std::process::Command;
///
/// let mut child = Command::new("/bin/cat")
///                         .arg("file.txt")
///                         .spawn()
///                         .expect("failed to execute child");
///
/// let ecode = child.wait()
///                  .expect("failed to wait on child");
///
/// assert!(ecode.success());
/// ```
///
/// [`Command`]: struct.Command.html
/// [`Drop`]: ../../core/ops/trait.Drop.html
/// [`wait`]: #method.wait
#[stable(feature = "process", since = "1.0.0")]
pub struct Child {
    handle: imp::Process,

    /// The handle for writing to the child's standard input (stdin), if it has
    /// been captured.
    #[stable(feature = "process", since = "1.0.0")]
    pub stdin: Option<ChildStdin>,

    /// The handle for reading from the child's standard output (stdout), if it
    /// has been captured.
    #[stable(feature = "process", since = "1.0.0")]
    pub stdout: Option<ChildStdout>,

    /// The handle for reading from the child's standard error (stderr), if it
    /// has been captured.
    #[stable(feature = "process", since = "1.0.0")]
    pub stderr: Option<ChildStderr>,
}

impl AsInner<imp::Process> for Child {
    fn as_inner(&self) -> &imp::Process { &self.handle }
}

impl FromInner<(imp::Process, imp::StdioPipes)> for Child {
    fn from_inner((handle, io): (imp::Process, imp::StdioPipes)) -> Child {
        Child {
            handle,
            stdin: io.stdin.map(ChildStdin::from_inner),
            stdout: io.stdout.map(ChildStdout::from_inner),
            stderr: io.stderr.map(ChildStderr::from_inner),
        }
    }
}

impl IntoInner<imp::Process> for Child {
    fn into_inner(self) -> imp::Process { self.handle }
}

#[stable(feature = "std_debug", since = "1.16.0")]
impl fmt::Debug for Child {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Child")
            .field("stdin", &self.stdin)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

/// A handle to a child process's standard input (stdin).
///
/// This struct is used in the [`stdin`] field on [`Child`].
///
/// When an instance of `ChildStdin` is [dropped], the `ChildStdin`'s underlying
/// file handle will be closed. If the child process was blocked on input prior
/// to being dropped, it will become unblocked after dropping.
///
/// [`Child`]: struct.Child.html
/// [`stdin`]: struct.Child.html#structfield.stdin
/// [dropped]: ../ops/trait.Drop.html
#[stable(feature = "process", since = "1.0.0")]
pub struct ChildStdin {
    inner: AnonPipe
}

#[stable(feature = "process", since = "1.0.0")]
impl Write for ChildStdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsInner<AnonPipe> for ChildStdin {
    fn as_inner(&self) -> &AnonPipe { &self.inner }
}

impl IntoInner<AnonPipe> for ChildStdin {
    fn into_inner(self) -> AnonPipe { self.inner }
}

impl FromInner<AnonPipe> for ChildStdin {
    fn from_inner(pipe: AnonPipe) -> ChildStdin {
        ChildStdin { inner: pipe }
    }
}

#[stable(feature = "std_debug", since = "1.16.0")]
impl fmt::Debug for ChildStdin {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("ChildStdin { .. }")
    }
}

/// A handle to a child process's standard output (stdout).
///
/// This struct is used in the [`stdout`] field on [`Child`].
///
/// When an instance of `ChildStdout` is [dropped], the `ChildStdout`'s
/// underlying file handle will be closed.
///
/// [`Child`]: struct.Child.html
/// [`stdout`]: struct.Child.html#structfield.stdout
/// [dropped]: ../ops/trait.Drop.html
#[stable(feature = "process", since = "1.0.0")]
pub struct ChildStdout {
    inner: AnonPipe
}

#[stable(feature = "process", since = "1.0.0")]
impl Read for ChildStdout {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
    #[inline]
    unsafe fn initializer(&self) -> Initializer {
        Initializer::nop()
    }
}

impl AsInner<AnonPipe> for ChildStdout {
    fn as_inner(&self) -> &AnonPipe { &self.inner }
}

impl IntoInner<AnonPipe> for ChildStdout {
    fn into_inner(self) -> AnonPipe { self.inner }
}

impl FromInner<AnonPipe> for ChildStdout {
    fn from_inner(pipe: AnonPipe) -> ChildStdout {
        ChildStdout { inner: pipe }
    }
}

#[stable(feature = "std_debug", since = "1.16.0")]
impl fmt::Debug for ChildStdout {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("ChildStdout { .. }")
    }
}

/// A handle to a child process's stderr.
///
/// This struct is used in the [`stderr`] field on [`Child`].
///
/// When an instance of `ChildStderr` is [dropped], the `ChildStderr`'s
/// underlying file handle will be closed.
///
/// [`Child`]: struct.Child.html
/// [`stderr`]: struct.Child.html#structfield.stderr
/// [dropped]: ../ops/trait.Drop.html
#[stable(feature = "process", since = "1.0.0")]
pub struct ChildStderr {
    inner: AnonPipe
}

#[stable(feature = "process", since = "1.0.0")]
impl Read for ChildStderr {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
    #[inline]
    unsafe fn initializer(&self) -> Initializer {
        Initializer::nop()
    }
}

impl AsInner<AnonPipe> for ChildStderr {
    fn as_inner(&self) -> &AnonPipe { &self.inner }
}

impl IntoInner<AnonPipe> for ChildStderr {
    fn into_inner(self) -> AnonPipe { self.inner }
}

impl FromInner<AnonPipe> for ChildStderr {
    fn from_inner(pipe: AnonPipe) -> ChildStderr {
        ChildStderr { inner: pipe }
    }
}

#[stable(feature = "std_debug", since = "1.16.0")]
impl fmt::Debug for ChildStderr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("ChildStderr { .. }")
    }
}

/// A process builder, providing fine-grained control
/// over how a new process should be spawned.
///
/// A default configuration can be
/// generated using `Command::new(program)`, where `program` gives a path to the
/// program to be executed. Additional builder methods allow the configuration
/// to be changed (for example, by adding arguments) prior to spawning:
///
/// ```
/// use std::process::Command;
///
/// let output = if cfg!(target_os = "windows") {
///     Command::new("cmd")
///             .args(&["/C", "echo hello"])
///             .output()
///             .expect("failed to execute process")
/// } else {
///     Command::new("sh")
///             .arg("-c")
///             .arg("echo hello")
///             .output()
///             .expect("failed to execute process")
/// };
///
/// let hello = output.stdout;
/// ```
#[stable(feature = "process", since = "1.0.0")]
pub struct Command {
    inner: imp::Command,
}

impl Command {
    /// Constructs a new `Command` for launching the program at
    /// path `program`, with the following default configuration:
    ///
    /// * No arguments to the program
    /// * Inherit the current process's environment
    /// * Inherit the current process's working directory
    /// * Inherit stdin/stdout/stderr for `spawn` or `status`, but create pipes for `output`
    ///
    /// Builder methods are provided to change these defaults and
    /// otherwise configure the process.
    ///
    /// If `program` is not an absolute path, the `PATH` will be searched in
    /// an OS-defined way.
    ///
    /// The search path to be used may be controlled by setting the
    /// `PATH` environment variable on the Command,
    /// but this has some implementation limitations on Windows
    /// (see <https://github.com/rust-lang/rust/issues/37519>).
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("sh")
    ///         .spawn()
    ///         .expect("sh command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn new<S: AsRef<OsStr>>(program: S) -> Command {
        Command { inner: imp::Command::new(program.as_ref()) }
    }

    /// Add an argument to pass to the program.
    ///
    /// Only one argument can be passed per use. So instead of:
    ///
    /// ```no_run
    /// # std::process::Command::new("sh")
    /// .arg("-C /path/to/repo")
    /// # ;
    /// ```
    ///
    /// usage would be:
    ///
    /// ```no_run
    /// # std::process::Command::new("sh")
    /// .arg("-C")
    /// .arg("/path/to/repo")
    /// # ;
    /// ```
    ///
    /// To pass multiple arguments see [`args`].
    ///
    /// [`args`]: #method.args
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .arg("-l")
    ///         .arg("-a")
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Command {
        self.inner.arg(arg.as_ref());
        self
    }

    /// Add multiple arguments to pass to the program.
    ///
    /// To pass a single argument see [`arg`].
    ///
    /// [`arg`]: #method.arg
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .args(&["-l", "-a"])
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn args<I, S>(&mut self, args: I) -> &mut Command
        where I: IntoIterator<Item=S>, S: AsRef<OsStr>
    {
        for arg in args {
            self.arg(arg.as_ref());
        }
        self
    }

    /// Inserts or updates an environment variable mapping.
    ///
    /// Note that environment variable names are case-insensitive (but case-preserving) on Windows,
    /// and case-sensitive on all other platforms.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .env("PATH", "/bin")
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Command
        where K: AsRef<OsStr>, V: AsRef<OsStr>
    {
        self.inner.env_mut().set(key.as_ref(), val.as_ref());
        self
    }

    /// Add or update multiple environment variable mappings.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    /// use std::env;
    /// use std::collections::HashMap;
    ///
    /// let filtered_env : HashMap<String, String> =
    ///     env::vars().filter(|&(ref k, _)|
    ///         k == "TERM" || k == "TZ" || k == "LANG" || k == "PATH"
    ///     ).collect();
    ///
    /// Command::new("printenv")
    ///         .stdin(Stdio::null())
    ///         .stdout(Stdio::inherit())
    ///         .env_clear()
    ///         .envs(&filtered_env)
    ///         .spawn()
    ///         .expect("printenv failed to start");
    /// ```
    #[stable(feature = "command_envs", since = "1.19.0")]
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Command
        where I: IntoIterator<Item=(K, V)>, K: AsRef<OsStr>, V: AsRef<OsStr>
    {
        for (ref key, ref val) in vars {
            self.inner.env_mut().set(key.as_ref(), val.as_ref());
        }
        self
    }

    /// Removes an environment variable mapping.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .env_remove("PATH")
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Command {
        self.inner.env_mut().remove(key.as_ref());
        self
    }

    /// Clears the entire environment map for the child process.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .env_clear()
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn env_clear(&mut self) -> &mut Command {
        self.inner.env_mut().clear();
        self
    }

    /// Sets the working directory for the child process.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .current_dir("/bin")
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Command {
        self.inner.cwd(dir.as_ref().as_ref());
        self
    }

    /// Configuration for the child process's standard input (stdin) handle.
    ///
    /// Defaults to [`inherit`] when used with `spawn` or `status`, and
    /// defaults to [`piped`] when used with `output`.
    ///
    /// [`inherit`]: struct.Stdio.html#method.inherit
    /// [`piped`]: struct.Stdio.html#method.piped
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// Command::new("ls")
    ///         .stdin(Stdio::null())
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn stdin<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Command {
        self.inner.stdin(cfg.into().0);
        self
    }

    /// Configuration for the child process's standard output (stdout) handle.
    ///
    /// Defaults to [`inherit`] when used with `spawn` or `status`, and
    /// defaults to [`piped`] when used with `output`.
    ///
    /// [`inherit`]: struct.Stdio.html#method.inherit
    /// [`piped`]: struct.Stdio.html#method.piped
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// Command::new("ls")
    ///         .stdout(Stdio::null())
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn stdout<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Command {
        self.inner.stdout(cfg.into().0);
        self
    }

    /// Configuration for the child process's standard error (stderr) handle.
    ///
    /// Defaults to [`inherit`] when used with `spawn` or `status`, and
    /// defaults to [`piped`] when used with `output`.
    ///
    /// [`inherit`]: struct.Stdio.html#method.inherit
    /// [`piped`]: struct.Stdio.html#method.piped
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// Command::new("ls")
    ///         .stderr(Stdio::null())
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn stderr<T: Into<Stdio>>(&mut self, cfg: T) -> &mut Command {
        self.inner.stderr(cfg.into().0);
        self
    }

    /// Executes the command as a child process, returning a handle to it.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// Command::new("ls")
    ///         .spawn()
    ///         .expect("ls command failed to start");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn spawn(&mut self) -> io::Result<Child> {
        self.inner.spawn(imp::Stdio::Inherit, true).map(Child::from_inner)
    }

    /// Executes the command as a child process, waiting for it to finish and
    /// collecting all of its output.
    ///
    /// By default, stdout and stderr are captured (and used to provide the
    /// resulting output). Stdin is not inherited from the parent and any
    /// attempt by the child process to read from the stdin stream will result
    /// in the stream immediately closing.
    ///
    /// # Examples
    ///
    /// ```should_panic
    /// use std::process::Command;
    /// let output = Command::new("/bin/cat")
    ///                      .arg("file.txt")
    ///                      .output()
    ///                      .expect("failed to execute process");
    ///
    /// println!("status: {}", output.status);
    /// println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    /// println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    ///
    /// assert!(output.status.success());
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn output(&mut self) -> io::Result<Output> {
        self.inner.spawn(imp::Stdio::MakePipe, false).map(Child::from_inner)
            .and_then(|p| p.wait_with_output())
    }

    /// Executes a command as a child process, waiting for it to finish and
    /// collecting its exit status.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent.
    ///
    /// # Examples
    ///
    /// ```should_panic
    /// use std::process::Command;
    ///
    /// let status = Command::new("/bin/cat")
    ///                      .arg("file.txt")
    ///                      .status()
    ///                      .expect("failed to execute process");
    ///
    /// println!("process exited with: {}", status);
    ///
    /// assert!(status.success());
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.inner.spawn(imp::Stdio::Inherit, true).map(Child::from_inner)
                  .and_then(|mut p| p.wait())
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl fmt::Debug for Command {
    /// Format the program and arguments of a Command for display. Any
    /// non-utf8 data is lossily converted using the utf8 replacement
    /// character.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl AsInner<imp::Command> for Command {
    fn as_inner(&self) -> &imp::Command { &self.inner }
}

impl AsInnerMut<imp::Command> for Command {
    fn as_inner_mut(&mut self) -> &mut imp::Command { &mut self.inner }
}

/// The output of a finished process.
///
/// This is returned in a Result by either the [`output`] method of a
/// [`Command`], or the [`wait_with_output`] method of a [`Child`]
/// process.
///
/// [`Command`]: struct.Command.html
/// [`Child`]: struct.Child.html
/// [`output`]: struct.Command.html#method.output
/// [`wait_with_output`]: struct.Child.html#method.wait_with_output
#[derive(PartialEq, Eq, Clone)]
#[stable(feature = "process", since = "1.0.0")]
pub struct Output {
    /// The status (exit code) of the process.
    #[stable(feature = "process", since = "1.0.0")]
    pub status: ExitStatus,
    /// The data that the process wrote to stdout.
    #[stable(feature = "process", since = "1.0.0")]
    pub stdout: Vec<u8>,
    /// The data that the process wrote to stderr.
    #[stable(feature = "process", since = "1.0.0")]
    pub stderr: Vec<u8>,
}

// If either stderr or stdout are valid utf8 strings it prints the valid
// strings, otherwise it prints the byte sequence instead
#[stable(feature = "process_output_debug", since = "1.7.0")]
impl fmt::Debug for Output {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {

        let stdout_utf8 = str::from_utf8(&self.stdout);
        let stdout_debug: &fmt::Debug = match stdout_utf8 {
            Ok(ref str) => str,
            Err(_) => &self.stdout
        };

        let stderr_utf8 = str::from_utf8(&self.stderr);
        let stderr_debug: &fmt::Debug = match stderr_utf8 {
            Ok(ref str) => str,
            Err(_) => &self.stderr
        };

        fmt.debug_struct("Output")
            .field("status", &self.status)
            .field("stdout", stdout_debug)
            .field("stderr", stderr_debug)
            .finish()
    }
}

/// Describes what to do with a standard I/O stream for a child process when
/// passed to the [`stdin`], [`stdout`], and [`stderr`] methods of [`Command`].
///
/// [`stdin`]: struct.Command.html#method.stdin
/// [`stdout`]: struct.Command.html#method.stdout
/// [`stderr`]: struct.Command.html#method.stderr
/// [`Command`]: struct.Command.html
#[stable(feature = "process", since = "1.0.0")]
pub struct Stdio(imp::Stdio);

impl Stdio {
    /// A new pipe should be arranged to connect the parent and child processes.
    ///
    /// # Examples
    ///
    /// With stdout:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// let output = Command::new("echo")
    ///     .arg("Hello, world!")
    ///     .stdout(Stdio::piped())
    ///     .output()
    ///     .expect("Failed to execute command");
    ///
    /// assert_eq!(String::from_utf8_lossy(&output.stdout), "Hello, world!\n");
    /// // Nothing echoed to console
    /// ```
    ///
    /// With stdin:
    ///
    /// ```no_run
    /// use std::io::Write;
    /// use std::process::{Command, Stdio};
    ///
    /// let mut child = Command::new("rev")
    ///     .stdin(Stdio::piped())
    ///     .stdout(Stdio::piped())
    ///     .spawn()
    ///     .expect("Failed to spawn child process");
    ///
    /// {
    ///     let mut stdin = child.stdin.as_mut().expect("Failed to open stdin");
    ///     stdin.write_all("Hello, world!".as_bytes()).expect("Failed to write to stdin");
    /// }
    ///
    /// let output = child.wait_with_output().expect("Failed to read stdout");
    /// assert_eq!(String::from_utf8_lossy(&output.stdout), "!dlrow ,olleH\n");
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn piped() -> Stdio { Stdio(imp::Stdio::MakePipe) }

    /// The child inherits from the corresponding parent descriptor.
    ///
    /// # Examples
    ///
    /// With stdout:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// let output = Command::new("echo")
    ///     .arg("Hello, world!")
    ///     .stdout(Stdio::inherit())
    ///     .output()
    ///     .expect("Failed to execute command");
    ///
    /// assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    /// // "Hello, world!" echoed to console
    /// ```
    ///
    /// With stdin:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// let output = Command::new("rev")
    ///     .stdin(Stdio::inherit())
    ///     .stdout(Stdio::piped())
    ///     .output()
    ///     .expect("Failed to execute command");
    ///
    /// println!("You piped in the reverse of: {}", String::from_utf8_lossy(&output.stdout));
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn inherit() -> Stdio { Stdio(imp::Stdio::Inherit) }

    /// This stream will be ignored. This is the equivalent of attaching the
    /// stream to `/dev/null`
    ///
    /// # Examples
    ///
    /// With stdout:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// let output = Command::new("echo")
    ///     .arg("Hello, world!")
    ///     .stdout(Stdio::null())
    ///     .output()
    ///     .expect("Failed to execute command");
    ///
    /// assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    /// // Nothing echoed to console
    /// ```
    ///
    /// With stdin:
    ///
    /// ```no_run
    /// use std::process::{Command, Stdio};
    ///
    /// let output = Command::new("rev")
    ///     .stdin(Stdio::null())
    ///     .stdout(Stdio::piped())
    ///     .output()
    ///     .expect("Failed to execute command");
    ///
    /// assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    /// // Ignores any piped-in input
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn null() -> Stdio { Stdio(imp::Stdio::Null) }
}

impl FromInner<imp::Stdio> for Stdio {
    fn from_inner(inner: imp::Stdio) -> Stdio {
        Stdio(inner)
    }
}

#[stable(feature = "std_debug", since = "1.16.0")]
impl fmt::Debug for Stdio {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("Stdio { .. }")
    }
}

#[stable(feature = "stdio_from", since = "1.20.0")]
impl From<ChildStdin> for Stdio {
    fn from(child: ChildStdin) -> Stdio {
        Stdio::from_inner(child.into_inner().into())
    }
}

#[stable(feature = "stdio_from", since = "1.20.0")]
impl From<ChildStdout> for Stdio {
    fn from(child: ChildStdout) -> Stdio {
        Stdio::from_inner(child.into_inner().into())
    }
}

#[stable(feature = "stdio_from", since = "1.20.0")]
impl From<ChildStderr> for Stdio {
    fn from(child: ChildStderr) -> Stdio {
        Stdio::from_inner(child.into_inner().into())
    }
}

#[stable(feature = "stdio_from", since = "1.20.0")]
impl From<fs::File> for Stdio {
    fn from(file: fs::File) -> Stdio {
        Stdio::from_inner(file.into_inner().into())
    }
}

/// Describes the result of a process after it has terminated.
///
/// This `struct` is used to represent the exit status of a child process.
/// Child processes are created via the [`Command`] struct and their exit
/// status is exposed through the [`status`] method.
///
/// [`Command`]: struct.Command.html
/// [`status`]: struct.Command.html#method.status
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
#[stable(feature = "process", since = "1.0.0")]
pub struct ExitStatus(imp::ExitStatus);

impl ExitStatus {
    /// Was termination successful? Signal termination is not considered a
    /// success, and success is defined as a zero exit status.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use std::process::Command;
    ///
    /// let status = Command::new("mkdir")
    ///                      .arg("projects")
    ///                      .status()
    ///                      .expect("failed to execute mkdir");
    ///
    /// if status.success() {
    ///     println!("'projects/' directory created");
    /// } else {
    ///     println!("failed to create 'projects/' directory");
    /// }
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn success(&self) -> bool {
        self.0.success()
    }

    /// Returns the exit code of the process, if any.
    ///
    /// On Unix, this will return `None` if the process was terminated
    /// by a signal; `std::os::unix` provides an extension trait for
    /// extracting the signal and other details from the `ExitStatus`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// let status = Command::new("mkdir")
    ///                      .arg("projects")
    ///                      .status()
    ///                      .expect("failed to execute mkdir");
    ///
    /// match status.code() {
    ///     Some(code) => println!("Exited with status code: {}", code),
    ///     None       => println!("Process terminated by signal")
    /// }
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn code(&self) -> Option<i32> {
        self.0.code()
    }
}

impl AsInner<imp::ExitStatus> for ExitStatus {
    fn as_inner(&self) -> &imp::ExitStatus { &self.0 }
}

impl FromInner<imp::ExitStatus> for ExitStatus {
    fn from_inner(s: imp::ExitStatus) -> ExitStatus {
        ExitStatus(s)
    }
}

#[stable(feature = "process", since = "1.0.0")]
impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// This is ridiculously unstable, as it's a completely-punted-upon part
/// of the `?`-in-`main` RFC.  It's here only to allow experimenting with
/// returning a code directly from main.  It will definitely change
/// drastically before being stabilized, if it doesn't just get deleted.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
#[unstable(feature = "process_exitcode_placeholder", issue = "43301")]
pub struct ExitCode(pub i32);

impl Child {
    /// Forces the child to exit. This is equivalent to sending a
    /// SIGKILL on unix platforms.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// let mut command = Command::new("yes");
    /// if let Ok(mut child) = command.spawn() {
    ///     child.kill().expect("command wasn't running");
    /// } else {
    ///     println!("yes command didn't start");
    /// }
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn kill(&mut self) -> io::Result<()> {
        self.handle.kill()
    }

    /// Returns the OS-assigned process identifier associated with this child.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// let mut command = Command::new("ls");
    /// if let Ok(child) = command.spawn() {
    ///     println!("Child's id is {}", child.id());
    /// } else {
    ///     println!("ls command didn't start");
    /// }
    /// ```
    #[stable(feature = "process_id", since = "1.3.0")]
    pub fn id(&self) -> u32 {
        self.handle.id()
    }

    /// Waits for the child to exit completely, returning the status that it
    /// exited with. This function will continue to have the same return value
    /// after it has been called at least once.
    ///
    /// The stdin handle to the child process, if any, will be closed
    /// before waiting. This helps avoid deadlock: it ensures that the
    /// child does not block waiting for input from the parent, while
    /// the parent waits for the child to exit.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// let mut command = Command::new("ls");
    /// if let Ok(mut child) = command.spawn() {
    ///     child.wait().expect("command wasn't running");
    ///     println!("Child has finished its execution!");
    /// } else {
    ///     println!("ls command didn't start");
    /// }
    /// ```
    #[stable(feature = "process", since = "1.0.0")]
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.handle.wait().map(ExitStatus)
    }

    /// Attempts to collect the exit status of the child if it has already
    /// exited.
    ///
    /// This function will not block the calling thread and will only advisorily
    /// check to see if the child process has exited or not. If the child has
    /// exited then on Unix the process id is reaped. This function is
    /// guaranteed to repeatedly return a successful exit status so long as the
    /// child has already exited.
    ///
    /// If the child has exited, then `Ok(Some(status))` is returned. If the
    /// exit status is not available at this time then `Ok(None)` is returned.
    /// If an error occurs, then that error is returned.
    ///
    /// Note that unlike `wait`, this function will not attempt to drop stdin.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```no_run
    /// use std::process::Command;
    ///
    /// let mut child = Command::new("ls").spawn().unwrap();
    ///
    /// match child.try_wait() {
    ///     Ok(Some(status)) => println!("exited with: {}", status),
    ///     Ok(None) => {
    ///         println!("status not ready yet, let's really wait");
    ///         let res = child.wait();
    ///         println!("result: {:?}", res);
    ///     }
    ///     Err(e) => println!("error attempting to wait: {}", e),
    /// }
    /// ```
    #[stable(feature = "process_try_wait", since = "1.18.0")]
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self.handle.try_wait()?.map(ExitStatus))
    }

    /// Simultaneously waits for the child to exit and collect all remaining
    /// output on the stdout/stderr handles, returning an `Output`
    /// instance.
    ///
    /// The stdin handle to the child process, if any, will be closed
    /// before waiting. This helps avoid deadlock: it ensures that the
    /// child does not block waiting for input from the parent, while
    /// the parent waits for the child to exit.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent.
    /// In order to capture the output into this `Result<Output>` it is
    /// necessary to create new pipes between parent and child. Use
    /// `stdout(Stdio::piped())` or `stderr(Stdio::piped())`, respectively.
    ///
    /// # Examples
    ///
    /// ```should_panic
    /// use std::process::{Command, Stdio};
    ///
    /// let child = Command::new("/bin/cat")
    ///     .arg("file.txt")
    ///     .stdout(Stdio::piped())
    ///     .spawn()
    ///     .expect("failed to execute child");
    ///
    /// let output = child
    ///     .wait_with_output()
    ///     .expect("failed to wait on child");
    ///
    /// assert!(output.status.success());
    /// ```
    ///
    #[stable(feature = "process", since = "1.0.0")]
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        drop(self.stdin.take());

        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        match (self.stdout.take(), self.stderr.take()) {
            (None, None) => {}
            (Some(mut out), None) => {
                let res = out.read_to_end(&mut stdout);
                res.unwrap();
            }
            (None, Some(mut err)) => {
                let res = err.read_to_end(&mut stderr);
                res.unwrap();
            }
            (Some(out), Some(err)) => {
                let res = read2(out.inner, &mut stdout, err.inner, &mut stderr);
                res.unwrap();
            }
        }

        let status = self.wait()?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

/// Terminates the current process with the specified exit code.
///
/// This function will never return and will immediately terminate the current
/// process. The exit code is passed through to the underlying OS and will be
/// available for consumption by another process.
///
/// Note that because this function never returns, and that it terminates the
/// process, no destructors on the current stack or any other thread's stack
/// will be run. If a clean shutdown is needed it is recommended to only call
/// this function at a known point where there are no more destructors left
/// to run.
///
/// ## Platform-specific behavior
///
/// **Unix**: On Unix-like platforms, it is unlikely that all 32 bits of `exit`
/// will be visible to a parent process inspecting the exit code. On most
/// Unix-like platforms, only the eight least-significant bits are considered.
///
/// # Examples
///
/// Due to this function’s behavior regarding destructors, a conventional way
/// to use the function is to extract the actual computation to another
/// function and compute the exit code from its return value:
///
/// ```
/// fn run_app() -> Result<(), ()> {
///     // Application logic here
///     Ok(())
/// }
///
/// fn main() {
///     ::std::process::exit(match run_app() {
///        Ok(_) => 0,
///        Err(err) => {
///            eprintln!("error: {:?}", err);
///            1
///        }
///     });
/// }
/// ```
///
/// Due to [platform-specific behavior], the exit code for this example will be
/// `0` on Linux, but `256` on Windows:
///
/// ```no_run
/// use std::process;
///
/// process::exit(0x0100);
/// ```
///
/// [platform-specific behavior]: #platform-specific-behavior
#[stable(feature = "rust1", since = "1.0.0")]
pub fn exit(code: i32) -> ! {
    ::sys_common::cleanup();
    ::sys::os::exit(code)
}

/// Terminates the process in an abnormal fashion.
///
/// The function will never return and will immediately terminate the current
/// process in a platform specific "abnormal" manner.
///
/// Note that because this function never returns, and that it terminates the
/// process, no destructors on the current stack or any other thread's stack
/// will be run.
///
/// This is in contrast to the default behaviour of [`panic!`] which unwinds
/// the current thread's stack and calls all destructors.
/// When `panic="abort"` is set, either as an argument to `rustc` or in a
/// crate's Cargo.toml, [`panic!`] and `abort` are similar. However,
/// [`panic!`] will still call the [panic hook] while `abort` will not.
///
/// If a clean shutdown is needed it is recommended to only call
/// this function at a known point where there are no more destructors left
/// to run.
///
/// # Examples
///
/// ```no_run
/// use std::process;
///
/// fn main() {
///     println!("aborting");
///
///     process::abort();
///
///     // execution never gets here
/// }
/// ```
///
/// The `abort` function terminates the process, so the destructor will not
/// get run on the example below:
///
/// ```no_run
/// use std::process;
///
/// struct HasDrop;
///
/// impl Drop for HasDrop {
///     fn drop(&mut self) {
///         println!("This will never be printed!");
///     }
/// }
///
/// fn main() {
///     let _x = HasDrop;
///     process::abort();
///     // the destructor implemented for HasDrop will never get run
/// }
/// ```
///
/// [`panic!`]: ../../std/macro.panic.html
/// [panic hook]: ../../std/panic/fn.set_hook.html
#[stable(feature = "process_abort", since = "1.17.0")]
pub fn abort() -> ! {
    unsafe { ::sys::abort_internal() };
}

/// Returns the OS-assigned process identifier associated with this process.
///
/// # Examples
///
/// Basic usage:
///
/// ```no_run
/// #![feature(getpid)]
/// use std::process;
///
/// println!("My pid is {}", process::id());
/// ```
///
///
#[unstable(feature = "getpid", issue = "44971", reason = "recently added")]
pub fn id() -> u32 {
    ::sys::os::getpid()
}

#[cfg(target_arch = "wasm32")]
mod exit {
    pub const SUCCESS: i32 = 0;
    pub const FAILURE: i32 = 1;
}
#[cfg(not(target_arch = "wasm32"))]
mod exit {
    use libc;
    pub const SUCCESS: i32 = libc::EXIT_SUCCESS;
    pub const FAILURE: i32 = libc::EXIT_FAILURE;
}

/// A trait for implementing arbitrary return types in the `main` function.
///
/// The c-main function only supports to return integers as return type.
/// So, every type implementing the `Termination` trait has to be converted
/// to an integer.
///
/// The default implementations are returning `libc::EXIT_SUCCESS` to indicate
/// a successful execution. In case of a failure, `libc::EXIT_FAILURE` is returned.
#[cfg_attr(not(test), lang = "termination")]
#[unstable(feature = "termination_trait_lib", issue = "43301")]
#[rustc_on_unimplemented =
  "`main` can only return types that implement {Termination}, not `{Self}`"]
pub trait Termination {
    /// Is called to get the representation of the value as status code.
    /// This status code is returned to the operating system.
    fn report(self) -> i32;
}

#[unstable(feature = "termination_trait_lib", issue = "43301")]
impl Termination for () {
    fn report(self) -> i32 { exit::SUCCESS }
}

#[unstable(feature = "termination_trait_lib", issue = "43301")]
impl<E: fmt::Debug> Termination for Result<(), E> {
    fn report(self) -> i32 {
        match self {
            Ok(val) => val.report(),
            Err(err) => {
                eprintln!("Error: {:?}", err);
                exit::FAILURE
            }
        }
    }
}

#[unstable(feature = "termination_trait_lib", issue = "43301")]
impl Termination for ! {
    fn report(self) -> i32 { self }
}

#[unstable(feature = "termination_trait_lib", issue = "43301")]
impl<E: fmt::Debug> Termination for Result<!, E> {
    fn report(self) -> i32 {
        let Err(err) = self;
        eprintln!("Error: {:?}", err);
        exit::FAILURE
    }
}

#[unstable(feature = "termination_trait_lib", issue = "43301")]
impl Termination for ExitCode {
    fn report(self) -> i32 {
        let ExitCode(code) = self;
        code
    }
}

#[cfg(all(test, not(any(target_os = "cloudabi", target_os = "emscripten"))))]
mod tests {
    use io::prelude::*;

    use io::ErrorKind;
    use str;
    use super::{Command, Output, Stdio};

    // FIXME(#10380) these tests should not all be ignored on android.

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn smoke() {
        let p = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "exit 0"]).spawn()
        } else {
            Command::new("true").spawn()
        };
        assert!(p.is_ok());
        let mut p = p.unwrap();
        assert!(p.wait().unwrap().success());
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn smoke_failure() {
        match Command::new("if-this-is-a-binary-then-the-world-has-ended").spawn() {
            Ok(..) => panic!(),
            Err(..) => {}
        }
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn exit_reported_right() {
        let p = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "exit 1"]).spawn()
        } else {
            Command::new("false").spawn()
        };
        assert!(p.is_ok());
        let mut p = p.unwrap();
        assert!(p.wait().unwrap().code() == Some(1));
        drop(p.wait());
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(target_os = "android", ignore)]
    fn signal_reported_right() {
        use os::unix::process::ExitStatusExt;

        let mut p = Command::new("/bin/sh")
                            .arg("-c").arg("read a")
                            .stdin(Stdio::piped())
                            .spawn().unwrap();
        p.kill().unwrap();
        match p.wait().unwrap().signal() {
            Some(9) => {},
            result => panic!("not terminated by signal 9 (instead, {:?})",
                             result),
        }
    }

    pub fn run_output(mut cmd: Command) -> String {
        let p = cmd.spawn();
        assert!(p.is_ok());
        let mut p = p.unwrap();
        assert!(p.stdout.is_some());
        let mut ret = String::new();
        p.stdout.as_mut().unwrap().read_to_string(&mut ret).unwrap();
        assert!(p.wait().unwrap().success());
        return ret;
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn stdout_works() {
        if cfg!(target_os = "windows") {
            let mut cmd = Command::new("cmd");
            cmd.args(&["/C", "echo foobar"]).stdout(Stdio::piped());
            assert_eq!(run_output(cmd), "foobar\r\n");
        } else {
            let mut cmd = Command::new("echo");
            cmd.arg("foobar").stdout(Stdio::piped());
            assert_eq!(run_output(cmd), "foobar\n");
        }
    }

    #[test]
    #[cfg_attr(any(windows, target_os = "android"), ignore)]
    fn set_current_dir_works() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("pwd")
           .current_dir("/")
           .stdout(Stdio::piped());
        assert_eq!(run_output(cmd), "/\n");
    }

    #[test]
    #[cfg_attr(any(windows, target_os = "android"), ignore)]
    fn stdin_works() {
        let mut p = Command::new("/bin/sh")
                            .arg("-c").arg("read line; echo $line")
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .spawn().unwrap();
        p.stdin.as_mut().unwrap().write("foobar".as_bytes()).unwrap();
        drop(p.stdin.take());
        let mut out = String::new();
        p.stdout.as_mut().unwrap().read_to_string(&mut out).unwrap();
        assert!(p.wait().unwrap().success());
        assert_eq!(out, "foobar\n");
    }


    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    #[cfg(unix)]
    fn uid_works() {
        use os::unix::prelude::*;
        use libc;
        let mut p = Command::new("/bin/sh")
                            .arg("-c").arg("true")
                            .uid(unsafe { libc::getuid() })
                            .gid(unsafe { libc::getgid() })
                            .spawn().unwrap();
        assert!(p.wait().unwrap().success());
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    #[cfg(unix)]
    fn uid_to_root_fails() {
        use os::unix::prelude::*;
        use libc;

        // if we're already root, this isn't a valid test. Most of the bots run
        // as non-root though (android is an exception).
        if unsafe { libc::getuid() == 0 } { return }
        assert!(Command::new("/bin/ls").uid(0).gid(0).spawn().is_err());
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn test_process_status() {
        let mut status = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "exit 1"]).status().unwrap()
        } else {
            Command::new("false").status().unwrap()
        };
        assert!(status.code() == Some(1));

        status = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "exit 0"]).status().unwrap()
        } else {
            Command::new("true").status().unwrap()
        };
        assert!(status.success());
    }

    #[test]
    fn test_process_output_fail_to_start() {
        match Command::new("/no-binary-by-this-name-should-exist").output() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::NotFound),
            Ok(..) => panic!()
        }
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn test_process_output_output() {
        let Output {status, stdout, stderr}
             = if cfg!(target_os = "windows") {
                 Command::new("cmd").args(&["/C", "echo hello"]).output().unwrap()
             } else {
                 Command::new("echo").arg("hello").output().unwrap()
             };
        let output_str = str::from_utf8(&stdout).unwrap();

        assert!(status.success());
        assert_eq!(output_str.trim().to_string(), "hello");
        assert_eq!(stderr, Vec::new());
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn test_process_output_error() {
        let Output {status, stdout, stderr}
             = if cfg!(target_os = "windows") {
                 Command::new("cmd").args(&["/C", "mkdir ."]).output().unwrap()
             } else {
                 Command::new("mkdir").arg("./").output().unwrap()
             };

        assert!(status.code() == Some(1));
        assert_eq!(stdout, Vec::new());
        assert!(!stderr.is_empty());
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn test_finish_once() {
        let mut prog = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "exit 1"]).spawn().unwrap()
        } else {
            Command::new("false").spawn().unwrap()
        };
        assert!(prog.wait().unwrap().code() == Some(1));
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn test_finish_twice() {
        let mut prog = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "exit 1"]).spawn().unwrap()
        } else {
            Command::new("false").spawn().unwrap()
        };
        assert!(prog.wait().unwrap().code() == Some(1));
        assert!(prog.wait().unwrap().code() == Some(1));
    }

    #[test]
    #[cfg_attr(target_os = "android", ignore)]
    fn test_wait_with_output_once() {
        let prog = if cfg!(target_os = "windows") {
            Command::new("cmd").args(&["/C", "echo hello"]).stdout(Stdio::piped()).spawn().unwrap()
        } else {
            Command::new("echo").arg("hello").stdout(Stdio::piped()).spawn().unwrap()
        };

        let Output {status, stdout, stderr} = prog.wait_with_output().unwrap();
        let output_str = str::from_utf8(&stdout).unwrap();

        assert!(status.success());
        assert_eq!(output_str.trim().to_string(), "hello");
        assert_eq!(stderr, Vec::new());
    }

    #[cfg(all(unix, not(target_os="android")))]
    pub fn env_cmd() -> Command {
        Command::new("env")
    }
    #[cfg(target_os="android")]
    pub fn env_cmd() -> Command {
        let mut cmd = Command::new("/system/bin/sh");
        cmd.arg("-c").arg("set");
        cmd
    }

    #[cfg(windows)]
    pub fn env_cmd() -> Command {
        let mut cmd = Command::new("cmd");
        cmd.arg("/c").arg("set");
        cmd
    }

    #[test]
    fn test_inherit_env() {
        use env;

        let result = env_cmd().output().unwrap();
        let output = String::from_utf8(result.stdout).unwrap();

        for (ref k, ref v) in env::vars() {
            // Don't check android RANDOM variable which seems to change
            // whenever the shell runs, and our `env_cmd` is indeed running a
            // shell which means it'll get a different RANDOM than we probably
            // have.
            //
            // Also skip env vars with `-` in the name on android because, well,
            // I'm not sure. It appears though that the `set` command above does
            // not print env vars with `-` in the name, so we just skip them
            // here as we won't find them in the output. Note that most env vars
            // use `_` instead of `-`, but our build system sets a few env vars
            // with `-` in the name.
            if cfg!(target_os = "android") &&
               (*k == "RANDOM" || k.contains("-")) {
                continue
            }

            // Windows has hidden environment variables whose names start with
            // equals signs (`=`). Those do not show up in the output of the
            // `set` command.
            assert!((cfg!(windows) && k.starts_with("=")) ||
                    k.starts_with("DYLD") ||
                    output.contains(&format!("{}={}", *k, *v)) ||
                    output.contains(&format!("{}='{}'", *k, *v)),
                    "output doesn't contain `{}={}`\n{}",
                    k, v, output);
        }
    }

    #[test]
    fn test_override_env() {
        use env;

        // In some build environments (such as chrooted Nix builds), `env` can
        // only be found in the explicitly-provided PATH env variable, not in
        // default places such as /bin or /usr/bin. So we need to pass through
        // PATH to our sub-process.
        let mut cmd = env_cmd();
        cmd.env_clear().env("RUN_TEST_NEW_ENV", "123");
        if let Some(p) = env::var_os("PATH") {
            cmd.env("PATH", &p);
        }
        let result = cmd.output().unwrap();
        let output = String::from_utf8_lossy(&result.stdout).to_string();

        assert!(output.contains("RUN_TEST_NEW_ENV=123"),
                "didn't find RUN_TEST_NEW_ENV inside of:\n\n{}", output);
    }

    #[test]
    fn test_add_to_env() {
        let result = env_cmd().env("RUN_TEST_NEW_ENV", "123").output().unwrap();
        let output = String::from_utf8_lossy(&result.stdout).to_string();

        assert!(output.contains("RUN_TEST_NEW_ENV=123"),
                "didn't find RUN_TEST_NEW_ENV inside of:\n\n{}", output);
    }

    #[test]
    fn test_capture_env_at_spawn() {
        use env;

        let mut cmd = env_cmd();
        cmd.env("RUN_TEST_NEW_ENV1", "123");

        // This variable will not be present if the environment has already
        // been captured above.
        env::set_var("RUN_TEST_NEW_ENV2", "456");
        let result = cmd.output().unwrap();
        env::remove_var("RUN_TEST_NEW_ENV2");

        let output = String::from_utf8_lossy(&result.stdout).to_string();

        assert!(output.contains("RUN_TEST_NEW_ENV1=123"),
                "didn't find RUN_TEST_NEW_ENV1 inside of:\n\n{}", output);
        assert!(output.contains("RUN_TEST_NEW_ENV2=456"),
                "didn't find RUN_TEST_NEW_ENV2 inside of:\n\n{}", output);
    }

    // Regression tests for #30858.
    #[test]
    fn test_interior_nul_in_progname_is_error() {
        match Command::new("has-some-\0\0s-inside").spawn() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::InvalidInput),
            Ok(_) => panic!(),
        }
    }

    #[test]
    fn test_interior_nul_in_arg_is_error() {
        match Command::new("echo").arg("has-some-\0\0s-inside").spawn() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::InvalidInput),
            Ok(_) => panic!(),
        }
    }

    #[test]
    fn test_interior_nul_in_args_is_error() {
        match Command::new("echo").args(&["has-some-\0\0s-inside"]).spawn() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::InvalidInput),
            Ok(_) => panic!(),
        }
    }

    #[test]
    fn test_interior_nul_in_current_dir_is_error() {
        match Command::new("echo").current_dir("has-some-\0\0s-inside").spawn() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::InvalidInput),
            Ok(_) => panic!(),
        }
    }

    // Regression tests for #30862.
    #[test]
    fn test_interior_nul_in_env_key_is_error() {
        match env_cmd().env("has-some-\0\0s-inside", "value").spawn() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::InvalidInput),
            Ok(_) => panic!(),
        }
    }

    #[test]
    fn test_interior_nul_in_env_value_is_error() {
        match env_cmd().env("key", "has-some-\0\0s-inside").spawn() {
            Err(e) => assert_eq!(e.kind(), ErrorKind::InvalidInput),
            Ok(_) => panic!(),
        }
    }

    /// Test that process creation flags work by debugging a process.
    /// Other creation flags make it hard or impossible to detect
    /// behavioral changes in the process.
    #[test]
    #[cfg(windows)]
    fn test_creation_flags() {
        use os::windows::process::CommandExt;
        use sys::c::{BOOL, DWORD, INFINITE};
        #[repr(C, packed)]
        struct DEBUG_EVENT {
            pub event_code: DWORD,
            pub process_id: DWORD,
            pub thread_id: DWORD,
            // This is a union in the real struct, but we don't
            // need this data for the purposes of this test.
            pub _junk: [u8; 164],
        }

        extern "system" {
            fn WaitForDebugEvent(lpDebugEvent: *mut DEBUG_EVENT, dwMilliseconds: DWORD) -> BOOL;
            fn ContinueDebugEvent(dwProcessId: DWORD, dwThreadId: DWORD,
                                  dwContinueStatus: DWORD) -> BOOL;
        }

        const DEBUG_PROCESS: DWORD = 1;
        const EXIT_PROCESS_DEBUG_EVENT: DWORD = 5;
        const DBG_EXCEPTION_NOT_HANDLED: DWORD = 0x80010001;

        let mut child = Command::new("cmd")
            .creation_flags(DEBUG_PROCESS)
            .stdin(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(b"exit\r\n").unwrap();
        let mut events = 0;
        let mut event = DEBUG_EVENT {
            event_code: 0,
            process_id: 0,
            thread_id: 0,
            _junk: [0; 164],
        };
        loop {
            if unsafe { WaitForDebugEvent(&mut event as *mut DEBUG_EVENT, INFINITE) } == 0 {
                panic!("WaitForDebugEvent failed!");
            }
            events += 1;

            if event.event_code == EXIT_PROCESS_DEBUG_EVENT {
                break;
            }

            if unsafe { ContinueDebugEvent(event.process_id,
                                           event.thread_id,
                                           DBG_EXCEPTION_NOT_HANDLED) } == 0 {
                panic!("ContinueDebugEvent failed!");
            }
        }
        assert!(events > 0);
    }

    #[test]
    fn test_command_implements_send() {
        fn take_send_type<T: Send>(_: T) {}
        take_send_type(Command::new(""))
    }
}
