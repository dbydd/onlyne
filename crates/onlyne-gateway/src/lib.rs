//! onlyne-gateway — Onlyne v1 gateway host: platform adapters, one process per platform.
pub mod host;
pub mod kit;

/// Process entry used by the binary; returns the process exit code.
pub fn run() -> i32 {
    eprintln!("onlyne-gateway: not yet implemented");
    2
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_package_version() {
        assert!(!super::version().is_empty());
    }
}
