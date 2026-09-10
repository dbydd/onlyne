//! onlyne-gateway-weixin — weixin platform gateway plugin (S10).

#[cfg(test)]
mod tests {
    #[test]
    fn plugin_crate_exists() {
        assert!(env!("CARGO_PKG_NAME").ends_with("weixin"));
    }
}
