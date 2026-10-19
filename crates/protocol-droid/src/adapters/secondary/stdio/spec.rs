//! The child program, as typed values rather than a command line.
//!
//! # Why a value and not a `Command`
//!
//! Everything that names a child arrives as text: an argv from a config file, a
//! string from a CLI flag, a pair of `KEY=VALUE` lines from an environment
//! file. None of that is a process, and the difference matters before the
//! process exists. A program name may be empty, an environment key may contain
//! the `=` that separates it from its value, and neither may contain a NUL —
//! three failures that are all silent at the point the text is accepted and all
//! fatal at the point the spawn is attempted.
//!
//! [`ChildSpec`] is where those refusals happen. It is built by constructors
//! that return [`ChildSpecError`], so a caller holding a `ChildSpec` is
//! holding something the operating system will accept, and the spawn in
//! [`child`](super::child) can report only *one* failure: the process did not
//! start.
//!
//! # `OsString`, not `String`
//!
//! An argument is a sequence of bytes to the operating system, not text. A
//! Windows path is UTF-16, a Unix path need not be UTF-8 at all, and a
//! round trip through `String` fails on both. So the typed value here is
//! [`OsString`], and the only check applied is the one the operating system
//! itself applies: no NUL, because a NUL terminates every C string on the way
//! in and there is no escaping it past that.
//!
//! # Inheriting the environment, deliberately
//!
//! A spawned MCP server is usually a command a user wrote for their own
//! machine, and it usually needs `PATH` to be worth anything. So the
//! environment is inherited unless a caller says otherwise with
//! [`without_inherited_env`](ChildSpec::without_inherited_env), and the
//! entries in [`env`](ChildSpec::env) are additions to that rather than a
//! replacement for it. Clearing it and re-adding only what the spec names is
//! the other mode, and it is a mode, not a default: a server that cannot find
//! its own `node` is a bug report nobody can act on.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use tokio::process::Command;

use thiserror::Error;

/// A child MCP server's program, arguments, and environment.
///
/// Built by [`new`](ChildSpec::new) or parsed from an argv with
/// [`from_argv`](ChildSpec::from_argv), both of which refuse a spec the
/// operating system would refuse. See the module docs for why.
///
/// # Examples
///
/// ```
/// use protocol_droid::adapters::secondary::stdio::ChildSpec;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let spec = ChildSpec::new("npx")?
///     .with_args(["-y", "@modelcontextprotocol/server-filesystem", "/tmp"])?
///     .with_env("RUST_LOG", "warn")?
///     .with_current_dir("/srv");
///
/// assert_eq!(spec.program(), std::ffi::OsStr::new("npx"));
/// assert_eq!(spec.args().len(), 3);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    current_dir: Option<PathBuf>,
    inherit_env: bool,
}

impl ChildSpec {
    /// A program with no arguments and an inherited environment.
    ///
    /// # Errors
    ///
    /// [`ChildSpecError::EmptyProgram`] when the program is empty, and
    /// [`ChildSpecError::Unrepresentable`] when it contains a NUL.
    pub fn new(program: impl Into<OsString>) -> Result<Self, ChildSpecError> {
        let program = program.into();
        if program.is_empty() {
            return Err(ChildSpecError::EmptyProgram);
        }
        reject_nul("the program name", &program)?;
        Ok(Self {
            program,
            args: Vec::new(),
            env: Vec::new(),
            current_dir: None,
            inherit_env: true,
        })
    }

    /// A program read from an argv, where the first element is the program.
    ///
    /// The convention every platform already uses — including `std::env::args`,
    /// which this is shaped for — so a caller holding a parsed command line
    /// hands it over whole rather than splitting it here and getting the split
    /// wrong.
    ///
    /// # Errors
    ///
    /// As [`ChildSpec::new`], applied to the first element, and as
    /// [`with_args`](ChildSpec::with_args), applied to the rest. An empty argv
    /// has no program to refuse, so it is [`ChildSpecError::EmptyArgv`].
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::adapters::secondary::stdio::ChildSpec;
    ///
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let spec = ChildSpec::from_argv(["my-server", "--stdio"])?;
    ///
    /// assert_eq!(spec.program(), std::ffi::OsStr::new("my-server"));
    /// assert_eq!(spec.args(), [std::ffi::OsString::from("--stdio")]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_argv<I, S>(argv: I) -> Result<Self, ChildSpecError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut argv = argv.into_iter().map(Into::into);
        let Some(program) = argv.next() else {
            return Err(ChildSpecError::EmptyArgv);
        };
        Self::new(program)?.with_args(argv)
    }

    /// Appends one argument.
    ///
    /// The single-argument form of [`with_args`](ChildSpec::with_args), for the
    /// call site that has one.
    ///
    /// # Errors
    ///
    /// [`ChildSpecError::Unrepresentable`] when the argument contains a NUL.
    pub fn with_arg(mut self, arg: impl Into<OsString>) -> Result<Self, ChildSpecError> {
        let arg = arg.into();
        reject_nul("an argument", &arg)?;
        self.args.push(arg);
        Ok(self)
    }

    /// Appends several arguments.
    ///
    /// # Errors
    ///
    /// [`ChildSpecError::Unrepresentable`] when any argument contains a NUL.
    /// Nothing is appended in that case, so a caller that ignores the error
    /// gets the spec it started with rather than one that is silently short at
    /// the spawn.
    pub fn with_args<I, S>(mut self, args: I) -> Result<Self, ChildSpecError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
        for arg in &args {
            reject_nul("an argument", arg)?;
        }
        self.args.extend(args);
        Ok(self)
    }

    /// Adds an environment variable for the child.
    ///
    /// An addition, not a replacement: see the module docs on inherited
    /// environments. A key already present is replaced, because that is what
    /// the second assignment to the same name means.
    ///
    /// # Errors
    ///
    /// [`ChildSpecError::EmptyEnvKey`] when the key is empty or contains the
    /// `=` that separates a key from its value, and
    /// [`ChildSpecError::Unrepresentable`] when either side contains a NUL.
    ///
    /// No `#[must_use]`: the return type is a `Result`, which is already
    /// marked, and saying it twice is noise rather than emphasis.
    pub fn with_env(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Result<Self, ChildSpecError> {
        let key = key.into();
        let value = value.into();
        if key.is_empty() {
            return Err(ChildSpecError::EmptyEnvKey);
        }
        if key.as_encoded_bytes().contains(&b'=') {
            return Err(ChildSpecError::EnvKeyContainsSeparator);
        }
        reject_nul("an environment key", &key)?;
        reject_nul("an environment value", &value)?;
        self.env.retain(|(existing, _)| existing != &key);
        self.env.push((key, value));
        Ok(self)
    }

    /// Runs the child in `directory`.
    #[must_use]
    pub fn with_current_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(directory.into());
        self
    }

    /// Starts the child with no inherited environment.
    ///
    /// Everything outside [`env`](ChildSpec::env) is then absent — including
    /// `PATH` on Unix, which usually means the program cannot be found. That is
    /// a real cost and the reason this is not the default; it is the right
    /// choice when a server is being started with a deliberately minimal
    /// environment, and the wrong one everywhere else.
    #[must_use]
    pub const fn without_inherited_env(mut self) -> Self {
        self.inherit_env = false;
        self
    }

    /// The program, as the operating system will be asked to find it.
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// The arguments, excluding the program.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// The environment entries added to whatever is inherited.
    #[must_use]
    pub fn env(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// The working directory, when one was set.
    #[must_use]
    pub fn current_dir(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }

    /// Whether the child starts from this process's environment.
    #[must_use]
    pub const fn inherits_env(&self) -> bool {
        self.inherit_env
    }

    /// The [`Command`] this spec describes.
    ///
    /// Public because a caller embedding this transport may want to add
    /// something the spec has no field for — a process group, a uid — and
    /// there is no reason for it to have to rebuild the spec to do it. The
    /// pipes and the kill-on-drop flag are *not* set here; they belong to
    /// `child::spawn`, which is the only thing in the
    /// crate that may claim to have set them.
    #[must_use]
    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        if !self.inherit_env {
            command.env_clear();
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }
        if let Some(directory) = &self.current_dir {
            command.current_dir(directory);
        }
        command
    }
}

/// Refuses a value containing a NUL, which no process can be given.
///
/// A NUL terminates every C string on the way into the kernel, on every
/// platform, so `spawn` would silently truncate the argument at it. Refusing
/// here is the difference between an error naming the field and a server
/// started with the wrong path.
fn reject_nul(what: &'static str, value: &OsStr) -> Result<(), ChildSpecError> {
    if value.as_encoded_bytes().contains(&0) {
        return Err(ChildSpecError::Unrepresentable { what });
    }
    Ok(())
}

/// Why a set of process arguments is not a program the operating system can run.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChildSpecError {
    /// The argv had no elements, so there was no program to run.
    #[error("the argv is empty, so there is no program to run")]
    EmptyArgv,

    /// The program name was empty.
    #[error("the program name is empty")]
    EmptyProgram,

    /// An environment key was empty.
    #[error("an environment key is empty")]
    EmptyEnvKey,

    /// An environment key contained `=`, which is the separator a key may not
    /// contain.
    #[error("an environment key contains '=', which separates a key from its value")]
    EnvKeyContainsSeparator,

    /// A value contained a NUL, which terminates the string before the kernel
    /// ever sees it.
    #[error("{what} contains a NUL, which no process can be given")]
    Unrepresentable {
        /// Which of the spec's fields was rejected.
        what: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use assert_matches::assert_matches;

    use super::{ChildSpec, ChildSpecError};

    /// What a `?` in a test here can produce. Both are the type under test, so
    /// there is nothing to collect and no reason to unwrap.
    type Failure = ChildSpecError;

    /// The program is the first thing a spec has, and a spec without one is
    /// not a program at all.
    #[test]
    fn a_program_becomes_a_spec() -> Result<(), Failure> {
        let spec = ChildSpec::new("my-server")?;

        assert_eq!(spec.program(), std::ffi::OsStr::new("my-server"));
        assert!(spec.args().is_empty());
        Ok(())
    }

    /// The environment is inherited unless a caller takes it away, because a
    /// child that cannot find `node` is not a useful default.
    #[test]
    fn the_environment_is_inherited_by_default() -> Result<(), Failure> {
        let inherited = ChildSpec::new("my-server")?;
        let cleared = ChildSpec::new("my-server")?.without_inherited_env();

        assert!(inherited.inherits_env());
        assert!(!cleared.inherits_env());
        Ok(())
    }

    /// An argv is split the way every platform already splits one: the first
    /// element is the program and the rest are arguments.
    #[test]
    fn an_argv_splits_at_the_first_element() -> Result<(), Failure> {
        let spec = ChildSpec::from_argv(["my-server", "--stdio", "--quiet"])?;

        assert_eq!(spec.program(), std::ffi::OsStr::new("my-server"));
        assert_eq!(
            spec.args(),
            [OsString::from("--stdio"), OsString::from("--quiet")]
        );
        Ok(())
    }

    /// An empty argv has nothing to run, which is a different failure from an
    /// argv whose program is empty, and worth telling apart: the first is a
    /// caller with nothing, the second is a caller with a broken name.
    #[test]
    fn an_empty_argv_is_refused() {
        let argv: [&str; 0] = [];

        assert_matches!(ChildSpec::from_argv(argv), Err(ChildSpecError::EmptyArgv));
    }

    /// An empty program reaches the kernel as "run nothing" on some platforms
    /// and as an error on others, so it is refused here where the answer is the
    /// same everywhere.
    #[test]
    fn an_empty_program_is_refused() {
        assert_matches!(ChildSpec::new(""), Err(ChildSpecError::EmptyProgram));
    }

    /// A NUL terminates the C string on its way into the kernel, so an argument
    /// carrying one is silently truncated at the spawn. Refusing it here is the
    /// difference between an error naming the field and a server started with
    /// the wrong path.
    #[test]
    fn a_nul_in_an_argument_is_refused() -> Result<(), Failure> {
        let spec = ChildSpec::new("my-server")?;

        assert_matches!(
            spec.with_arg("before\0after"),
            Err(ChildSpecError::Unrepresentable {
                what: "an argument"
            })
        );
        Ok(())
    }

    /// `=` is what separates a key from its value, so a key containing one has
    /// no value to separate it from.
    #[test]
    fn an_env_key_with_a_separator_is_refused() -> Result<(), Failure> {
        let spec = ChildSpec::new("my-server")?;

        assert_matches!(
            spec.with_env("A=B", "value"),
            Err(ChildSpecError::EnvKeyContainsSeparator)
        );
        Ok(())
    }

    /// The same key twice is a replacement, not a duplicate: two entries with
    /// one name would leave the child to pick, and which one it picked would
    /// depend on the platform's environment handling.
    #[test]
    fn a_repeated_env_key_is_replaced() -> Result<(), Failure> {
        let spec = ChildSpec::new("my-server")?
            .with_env("MODE", "first")?
            .with_env("MODE", "second")?;

        assert_eq!(spec.env().len(), 1);
        assert_eq!(
            spec.env().first(),
            Some(&(OsString::from("MODE"), OsString::from("second")))
        );
        Ok(())
    }

    /// A NUL anywhere in the argument list refuses the whole list rather than
    /// silently dropping the argument that carried it, so a caller that
    /// ignored the error would know it had lost an argument.
    #[test]
    fn a_nul_among_several_arguments_refuses_the_whole_list() -> Result<(), Failure> {
        let spec = ChildSpec::new("my-server")?;

        assert_matches!(
            spec.with_args(["--stdio", "bad\0argument"]),
            Err(ChildSpecError::Unrepresentable {
                what: "an argument"
            })
        );
        Ok(())
    }

    /// A command built from a spec carries the program, the arguments and the
    /// environment, and nothing else: the pipes and the kill-on-drop flag are
    /// the spawn's business, not the spec's.
    #[test]
    fn a_command_carries_the_spec() -> Result<(), Failure> {
        let spec = ChildSpec::new("my-server")?
            .with_args(["--stdio"])?
            .with_env("MODE", "test")?;

        let command = spec.to_command();

        assert_eq!(
            command.as_std().get_program(),
            std::ffi::OsStr::new("my-server")
        );
        // `get_args` hands back a `CommandArgs`, which iterates but is not
        // `PartialEq`, so the comparison is against what it yields.
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new("--stdio")]
        );
        Ok(())
    }
}
