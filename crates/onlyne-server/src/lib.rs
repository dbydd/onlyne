pub mod admin;
pub mod cli;
pub mod events;
pub mod faults;
pub mod gateway_host;
pub mod projection;
pub mod relay;
pub mod router;
pub mod state;

pub use state::{GatewayConnection, GatewayRegistry, ListenerHandles, RoleConnection, RoleRegistry, Server, ServerInit};

pub async fn run(init: ServerInit) -> anyhow::Result<()> {
    admin::run(init).await
}

pub async fn entrypoint() -> i32 {
    cli::entrypoint().await
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
