use std::{fmt, io, path::PathBuf};

/// Every rejection produced while reading a configuration document.
///
/// `Parse` and `Validate` render as `<file>:<line>: <message>`, which is the form
/// the CLI prints verbatim.
#[derive(Debug)]
pub enum SpecError {
    /// The document could not be read.
    Io {
        /// Path as passed to the loader.
        path: PathBuf,
        /// Underlying operating system failure.
        source: io::Error,
    },
    /// TOML syntax or schema rejection, located by line.
    Parse {
        /// File name of the offending document.
        file: String,
        /// One-based line number.
        line: usize,
        /// Detector message.
        message: String,
    },
    /// Semantic rejection across parsed entries, located by line.
    Validate {
        /// File name of the offending document.
        file: String,
        /// One-based line number.
        line: usize,
        /// Validation message.
        message: String,
    },
    /// A `$NAME` value named an environment variable that is unset.
    Secret {
        /// Environment variable name.
        var: String,
        /// Configuration field that asked for the value.
        label: String,
    },
}

impl SpecError {
    /// Build a `Parse` error.
    pub(crate) fn parse(file: impl Into<String>, line: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            file: file.into(),
            line,
            message: message.into(),
        }
    }

    /// Build a `Validate` error.
    pub(crate) fn validate(
        file: impl Into<String>,
        line: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::Validate {
            file: file.into(),
            line,
            message: message.into(),
        }
    }

    /// One-based line number for located errors.
    pub fn line(&self) -> Option<usize> {
        match self {
            Self::Io { .. } | Self::Secret { .. } => None,
            Self::Parse { line, .. } | Self::Validate { line, .. } => Some(*line),
        }
    }

    /// Message body with the location prefix removed.
    pub fn message(&self) -> String {
        match self {
            Self::Io { source, .. } => source.to_string(),
            Self::Parse { message, .. } | Self::Validate { message, .. } => message.clone(),
            Self::Secret { var, label } => {
                format!("missing secret ${var} for {label}; set the environment variable")
            }
        }
    }
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Parse {
                file,
                line,
                message,
            }
            | Self::Validate {
                file,
                line,
                message,
            } => {
                write!(f, "{file}:{line}: {message}")
            }
            Self::Secret { .. } => f.write_str(&self.message()),
        }
    }
}

impl std::error::Error for SpecError {}
