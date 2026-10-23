#[tokio::main]
async fn main() {
    std::process::exit(onlyne_server::entrypoint().await);
}
