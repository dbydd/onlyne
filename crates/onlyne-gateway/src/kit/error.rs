use std::{error::Error, fmt, io};

#[derive(Debug)]
pub enum KitError {
    MissingProgram { program: String },
    OversizedPayload { max: usize, actual: usize },
    RenderFailure { platform: String, detail: String },
    Io(io::Error),
    Unsupported(String),
}

impl KitError {
    pub fn render_failure(platform: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::RenderFailure {
            platform: platform.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for KitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProgram { program } => write!(f, "missing required program: {program}"),
            Self::OversizedPayload { max, actual } => {
                write!(f, "payload is {actual} bytes; maximum is {max} bytes")
            }
            Self::RenderFailure { platform, detail } => {
                write!(f, "render failure for {platform}: {detail}")
            }
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::Unsupported(detail) => write!(f, "unsupported operation: {detail}"),
        }
    }
}

impl Error for KitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::MissingProgram { .. }
            | Self::OversizedPayload { .. }
            | Self::RenderFailure { .. }
            | Self::Unsupported(_) => None,
        }
    }
}

impl From<io::Error> for KitError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
