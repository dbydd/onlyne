#[path = "shared/common.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    common::list_channels()?;
    common::run_targets("plain")?;
    common::fetch_all_history(50)
}
