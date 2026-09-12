use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use onlyne_adapter::AdapterClient;
use onlyne_proto::{Capability, HelloArgs, MountKind, PROTOCOL_VERSION};
use onlyne_testkit::{FakeGateway, run_fake_gateway_render_printer};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::{Duration, sleep};

#[derive(Debug, Parser)]
#[command(name = "onlyne-gateway-fake")]
struct Args {
    #[arg(long, default_value = "fake")]
    platform: String,
    #[arg(long, default_value = "fg1")]
    gateway_id: String,
    #[arg(long)]
    socket: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let handle = loop {
        match AdapterClient::connect_gateway_unix(&args.socket).await {
            Ok(handle) => break handle,
            Err(err) => {
                eprintln!(
                    "onlyne-gateway-fake: waiting for {}: {err}",
                    args.socket.display()
                );
                sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let gateway = Arc::new(handle);
    gateway
        .hello(HelloArgs {
            protocol: PROTOCOL_VERSION,
            plugin: "onlyne-gateway-fake".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: MountKind::Gateway,
            capabilities: vec![
                Capability::Report,
                Capability::Typing,
                Capability::Conversations,
            ],
            mount: Some(onlyne_proto::Mount::Gateway(onlyne_proto::GatewayMount {
                gateway: args.gateway_id.clone(),
                platform: args.platform.clone(),
            })),
        })
        .await?;
    gateway
        .register_channel(onlyne_proto::RegisterChannelArgs {
            platform: args.platform.clone(),
            channel: args.gateway_id.clone(),
            conversations: None,
        })
        .await?;
    let render_gateway = gateway.clone();
    tokio::spawn(async move {
        if let Err(err) = run_fake_gateway_render_printer(render_gateway).await {
            eprintln!("onlyne-gateway-fake render loop: {err}");
        }
    });
    let fake = FakeGateway::new(args.platform, args.gateway_id);
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("op").and_then(serde_json::Value::as_str) == Some("inbound") {
            let conversation = value
                .get("conversation")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("inbound conversation required"))?;
            let text = value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("inbound text required"))?;
            gateway
                .deliver_inbound(fake.inbound_delivery(conversation, text)?)
                .await?;
        } else {
            anyhow::bail!("unknown gateway input")
        }
    }
    Ok(())
}
