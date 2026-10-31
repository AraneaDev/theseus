use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// Environment inheritance is explicit, including a truly empty replacement.
#[derive(Clone, Debug)]
pub enum ChildEnvironment {
    Inherit,
    Replace(Vec<(OsString, OsString)>),
}

/// Native executable and ordinary argv, without shell or batch-file parsing.
#[derive(Clone, Debug)]
pub struct NativeCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub environment: ChildEnvironment,
}

impl NativeCommand {
    pub fn new(program: impl AsRef<std::ffi::OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            cwd: None,
            environment: ChildEnvironment::Inherit,
        }
    }
    pub fn arg(&mut self, argument: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.args.push(argument.as_ref().to_owned());
        self
    }
    pub fn args<I, S>(&mut self, arguments: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.args.extend(
            arguments
                .into_iter()
                .map(|argument| argument.as_ref().to_owned()),
        );
        self
    }
    pub fn current_dir(&mut self, directory: impl AsRef<std::path::Path>) -> &mut Self {
        self.cwd = Some(directory.as_ref().to_owned());
        self
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        let nul = |s: &std::ffi::OsStr| s.as_encoded_bytes().contains(&0);
        if self.program.is_empty()
            || nul(&self.program)
            || self.args.iter().any(|arg| nul(arg))
            || self.cwd.as_ref().is_some_and(|cwd| nul(cwd.as_os_str()))
        {
            return Err(invalid("empty program or NUL in native command"));
        }
        if std::path::Path::new(&self.program)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
        {
            return Err(invalid("batch files are not native executables"));
        }
        if let ChildEnvironment::Replace(entries) = &self.environment {
            for (key, value) in entries {
                if key.is_empty()
                    || nul(key)
                    || key.as_encoded_bytes().contains(&b'=')
                    || nul(value)
                {
                    return Err(invalid("invalid replacement environment entry"));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
