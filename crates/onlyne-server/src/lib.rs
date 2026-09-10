pub mod admin;
pub mod cli;
pub mod generate;
pub mod state;

pub use state::{GatewayConnection, GatewayRegistry, ListenerHandles, RoleConnection, RoleRegistry, Server, ServerInit};
pub use generate::{GenerateArgs, GenerateError, GenerateReport, GeneratedRole, generate};

pub async fn run(init: ServerInit) -> anyhow::Result<()> {
    admin::run(init).await
}

pub async fn entrypoint() -> i32 {
    cli::entrypoint().await
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
