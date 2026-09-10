//! onlyne-client role runtime.

pub mod accept;
pub mod adapter_socket;
pub mod daemon;
pub mod dispatch;
pub mod init;
pub mod intent;
pub mod local_cli;
pub mod runloop;

pub use runloop::ClientInit;

pub async fn run(init: ClientInit) -> anyhow::Result<()> {
    runloop::run(init).await
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
