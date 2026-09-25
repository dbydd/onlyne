//! onlyne-client role runtime.

pub mod host;
pub mod ops;
pub mod runtime;
pub mod session;

pub use runtime::runloop::ClientInit;

pub async fn run(init: ClientInit) -> anyhow::Result<()> {
    runtime::runloop::run(init).await
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
