#[path = "shared/common.rs"]
mod common;

fn main() -> anyhow::Result<()> {
    common::run_targets("plain")
}
