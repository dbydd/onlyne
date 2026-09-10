# v1 dependency inventory

This inventory reads every `Cargo.toml` under `crates/` except `crates/onlyne-legacy/` and every plugin manifest under `plugins/`. Dependency rows include normal, development, and build dependencies. `workspace` records a requirement inherited from the root manifest. Feature lists report features written in the declaring manifest; `default-features=false` is shown explicitly.

## External dependencies

| dependency | version requirement | feature list | declaring crates |
|---|---|---|---|
| `aes-gcm` | `0.10` | [] | onlyne-gateway |
| `anyhow` | `1.0` | [] | onlyne-client |
| `anyhow` | `1` | [] | onlyne-server, onlyne-store, onlyne-testkit |
| `anyhow` | workspace | [] | onlyne-session |
| `async-stream` | `0.3` | [] | onlyne-adapter |
| `async-trait` | `0.1` | [] | onlyne-adapter, onlyne-gateway, onlyne-testkit |
| `async-trait` | workspace | [] | onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `async-trait` | workspace + `0.1` | [] | onlyne-gateway-telegram |
| `base64` | `0.22.1` | [] | onlyne-net |
| `base64` | `0.22` | [] | onlyne-client, onlyne-config, onlyne-gateway, onlyne-server (dev-dependencies), onlyne-testkit |
| `base64` | workspace | [] | onlyne-cli, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto |
| `base64` | workspace + `0.22` | [] | onlyne-gateway-telegram |
| `chrono` | `0.4` | [`serde`] | onlyne-adapter (dev-dependencies), onlyne-client, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit |
| `chrono` | workspace | [] | onlyne-proto, onlyne-session |
| `clap` | `4.5` | [`derive`] | onlyne-client |
| `clap` | `4` | [`derive`] | onlyne-gateway, onlyne-server, onlyne-testkit |
| `clap` | workspace | [] | onlyne-cli |
| `clap_complete` | workspace | [] | onlyne-cli |
| `ed25519-dalek` | `2.1.1` | [`std`, `rand_core`]; default-features=false | onlyne-net |
| `ed25519-dalek` | `2.1` | [`std`, `rand_core`]; default-features=false | onlyne-client |
| `futures-util` | `0.3` | [] | onlyne-adapter, onlyne-testkit |
| `parking_lot` | `0.12` | [] | onlyne-client |
| `pulldown-cmark` | `0.12` | [] | onlyne-gateway |
| `qrcode` | `0.14` | []; default-features=false | onlyne-gateway |
| `rand` | `0.8.6` | [] | onlyne-net |
| `rand` | `0.8` | [] | onlyne-client |
| `rcgen` | `0.13.2` | [`ring`, `pem`]; default-features=false | onlyne-net |
| `reqwest` | workspace | [] | onlyne-gateway-qqbot |
| `resvg` | `0.45` | [`text`, `system-fonts`]; default-features=false | onlyne-gateway |
| `rusqlite` | `0.32` | [`bundled`, `chrono`, `serde_json`] | onlyne-client, onlyne-store |
| `rusqlite` | `0.32` | [`bundled`] | onlyne-server |
| `rustls` | `0.23.40` | [`ring`, `std`]; default-features=false | onlyne-net |
| `rustls-pemfile` | `2.2.0` | [] | onlyne-net |
| `rustls-pki-types` | `1.14.1` | [] | onlyne-net |
| `schemars` | `0.8` | [] | onlyne-config |
| `schemars` | workspace | [`chrono`, `uuid1`] | onlyne-proto |
| `serde` | `1.0.228` | [`derive`] | onlyne-net |
| `serde` | `1.0` | [`derive`] | onlyne-client |
| `serde` | `1` | [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit |
| `serde` | workspace | [] | onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde` | workspace + `1` | [`derive`] | onlyne-gateway-telegram |
| `serde_json` | `1.0.150` | [] | onlyne-net |
| `serde_json` | `1.0` | [] | onlyne-client |
| `serde_json` | `1` | [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit |
| `serde_json` | workspace | [] | onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde_json` | workspace + `1` | [] | onlyne-gateway-telegram |
| `sha2` | `0.10.9` | [] | onlyne-net |
| `sha2` | `0.10` | [] | onlyne-config, onlyne-gateway |
| `sha2` | workspace | [] | onlyne-proto |
| `teloxide` | `0.17.0` | [`rustls`]; default-features=false | onlyne-gateway-telegram |
| `tempfile` | `3.27.0` | [] | onlyne-net (dev-dependencies) |
| `tempfile` | `3` | [] | onlyne-config (dev-dependencies), onlyne-gateway (dev-dependencies), onlyne-layout (dev-dependencies), onlyne-server (dev-dependencies), onlyne-store (dev-dependencies), onlyne-testkit |
| `tempfile` | workspace | [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) |
| `time` | `0.3.49` | [`std`, `formatting`]; default-features=false | onlyne-net |
| `tokio` | `1.0` | [`rt-multi-thread`, `macros`, `net`, `io-util`, `fs`, `sync`, `time`, `process`] | onlyne-client |
| `tokio` | `1.52.3` | [`macros`, `net`, `io-util`, `fs`, `time`, `rt-multi-thread`] | onlyne-net |
| `tokio` | `1` | [`fs`, `macros`, `net`, `process`, `rt-multi-thread`, `time`] | onlyne-gateway |
| `tokio` | `1` | [`io-util`, `io-std`, `net`, `process`, `rt-multi-thread`, `macros`, `sync`, `time`, `fs`] | onlyne-testkit |
| `tokio` | `1` | [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter |
| `tokio` | `1` | [`rt-multi-thread`, `macros`, `net`, `io-util`, `sync`, `time`, `fs`] | onlyne-server |
| `tokio` | workspace | [] | onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `tokio` | workspace | [`rt-multi-thread`, `macros`] | onlyne-frame (dev-dependencies) |
| `tokio-rustls` | `0.26.4` | [`ring`]; default-features=false | onlyne-net |
| `tokio-stream` | `0.1` | [] | onlyne-testkit |
| `tokio-tungstenite` | workspace | [] | onlyne-gateway-qqbot |
| `toml` | `0.8` | [] | onlyne-client, onlyne-config |
| `tracing` | `0.1` | [] | onlyne-adapter, onlyne-client, onlyne-server, onlyne-store, onlyne-testkit |
| `tracing` | workspace | [] | onlyne-session |
| `unicode-display-width` | `0.2` | [] | onlyne-gateway |
| `uuid` | `1.0` | [`v4`, `serde`] | onlyne-client |
| `uuid` | `1` | [`v4`, `serde`] | onlyne-server |
| `uuid` | workspace | [] | onlyne-proto |
| `wechat-ilink` | workspace | [] | onlyne-gateway-weixin |
| `x509-parser` | `0.16.0` | [] | onlyne-net |

A crate in parentheses declares the dependency in `[dev-dependencies]` or `[build-dependencies]`. A plain crate name declares it in `[dependencies]`.

## Internal edges

| declaring crate | dependency | section | version requirement | path/reference |
|---|---|---|---|---|
| `onlyne-adapter` | `onlyne-frame` | `dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-adapter` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-cli` | `onlyne-frame` | `dependencies` | workspace | `None` |
| `onlyne-cli` | `onlyne-proto` | `dependencies` | workspace | `None` |
| `onlyne-client` | `onlyne-adapter` | `dependencies` | unspecified | `../onlyne-adapter` |
| `onlyne-client` | `onlyne-config` | `dependencies` | unspecified | `../onlyne-config` |
| `onlyne-client` | `onlyne-frame` | `dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-client` | `onlyne-layout` | `dependencies` | unspecified | `../onlyne-layout` |
| `onlyne-client` | `onlyne-net` | `dependencies` | unspecified | `../onlyne-net` |
| `onlyne-client` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-client` | `onlyne-session` | `dependencies` | unspecified | `../onlyne-session` |
| `onlyne-client` | `onlyne-store` | `dependencies` | unspecified | `../onlyne-store` |
| `onlyne-gateway` | `onlyne-adapter` | `dependencies` | `1.0.0` | `../onlyne-adapter` |
| `onlyne-gateway` | `onlyne-config` | `dependencies` | `1.0.0` | `../onlyne-config` |
| `onlyne-gateway` | `onlyne-gateway-feishu` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-feishu` |
| `onlyne-gateway` | `onlyne-gateway-qqbot` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-qqbot` |
| `onlyne-gateway` | `onlyne-gateway-telegram` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-telegram` |
| `onlyne-gateway` | `onlyne-gateway-weixin` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-weixin` |
| `onlyne-gateway` | `onlyne-net` | `dependencies` | `1.0.0` | `../onlyne-net` |
| `onlyne-gateway` | `onlyne-proto` | `dependencies` | `1.0.0` | `../onlyne-proto` |
| `onlyne-gateway-qqbot` | `onlyne-adapter` | `dependencies` | workspace | `None` |
| `onlyne-gateway-qqbot` | `onlyne-proto` | `dependencies` | workspace | `None` |
| `onlyne-gateway-telegram` | `onlyne-adapter` | `dependencies` | workspace | `None` |
| `onlyne-gateway-telegram` | `onlyne-proto` | `dependencies` | workspace | `None` |
| `onlyne-gateway-weixin` | `onlyne-adapter` | `dependencies` | workspace | `None` |
| `onlyne-gateway-weixin` | `onlyne-proto` | `dependencies` | workspace | `None` |
| `onlyne-net` | `onlyne-frame` | `dependencies` | `1.0.0` | `../onlyne-frame` |
| `onlyne-server` | `onlyne-adapter` | `dependencies` | `1.0.0` | `../onlyne-adapter` |
| `onlyne-server` | `onlyne-config` | `dependencies` | `1.0.0` | `../onlyne-config` |
| `onlyne-server` | `onlyne-frame` | `dependencies` | `1.0.0` | `../onlyne-frame` |
| `onlyne-server` | `onlyne-layout` | `dependencies` | `1.0.0` | `../onlyne-layout` |
| `onlyne-server` | `onlyne-net` | `dependencies` | `1.0.0` | `../onlyne-net` |
| `onlyne-server` | `onlyne-proto` | `dependencies` | `1.0.0` | `../onlyne-proto` |
| `onlyne-server` | `onlyne-store` | `dependencies` | `1.0.0` | `../onlyne-store` |
| `onlyne-store` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-store` | `onlyne-session` | `dependencies` | unspecified | `../onlyne-session` |
| `onlyne-testkit` | `onlyne-adapter` | `dependencies` | unspecified | `../onlyne-adapter` |
| `onlyne-testkit` | `onlyne-frame` | `dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-testkit` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-testkit` | `onlyne-session` | `dependencies` | unspecified | `../onlyne-session` |

Firewall review against `docs/v1-PLAN.md` §1 lines 74–77:

- `onlyne-proto` declares no `tokio` dependency.
- `onlyne-session` declares no `onlyne-store`, `onlyne-net`, or `onlyne-proto` dependency.
- `onlyne-client` and `onlyne-server` do not declare each other.
- Plugins declare only the allowed internal edges (`onlyne-adapter` and `onlyne-proto`) where they declare internal crates; no plugin declares a server-internal crate.
- No firewall violation appears in the manifests reviewed.

## Conflicts

A conflict is any dependency with more than one manifest-level version requirement or feature/default-feature set. Workspace requirements are kept verbatim because this inventory is based on declaring manifests.

| dependency | variant A | crates | variant B or additional variant | crates |
|---|---|---|---|---|
| `anyhow` | `1.0` / [] | onlyne-client | `1` / [] | onlyne-server, onlyne-store, onlyne-testkit |
| `anyhow` | `1.0` / [] | onlyne-client | workspace / [] | onlyne-session |
| `async-trait` | `0.1` / [] | onlyne-adapter, onlyne-gateway, onlyne-testkit | workspace / [] | onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `async-trait` | `0.1` / [] | onlyne-adapter, onlyne-gateway, onlyne-testkit | workspace + `0.1` / [] | onlyne-gateway-telegram |
| `base64` | workspace / [] | onlyne-cli, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto | `0.22` / [] | onlyne-client, onlyne-config, onlyne-gateway, onlyne-server (dev-dependencies), onlyne-testkit |
| `base64` | workspace / [] | onlyne-cli, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto | `0.22.1` / [] | onlyne-net |
| `base64` | workspace / [] | onlyne-cli, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto | workspace + `0.22` / [] | onlyne-gateway-telegram |
| `chrono` | `0.4` / [`serde`] | onlyne-adapter (dev-dependencies), onlyne-client, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-proto, onlyne-session |
| `clap` | workspace / [] | onlyne-cli | `4.5` / [`derive`] | onlyne-client |
| `clap` | workspace / [] | onlyne-cli | `4` / [`derive`] | onlyne-gateway, onlyne-server, onlyne-testkit |
| `ed25519-dalek` | `2.1` / [`std`, `rand_core`]; default-features=false | onlyne-client | `2.1.1` / [`std`, `rand_core`]; default-features=false | onlyne-net |
| `rand` | `0.8` / [] | onlyne-client | `0.8.6` / [] | onlyne-net |
| `rusqlite` | `0.32` / [`bundled`, `chrono`, `serde_json`] | onlyne-client, onlyne-store | `0.32` / [`bundled`] | onlyne-server |
| `schemars` | `0.8` / [] | onlyne-config | workspace / [`chrono`, `uuid1`] | onlyne-proto |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | `1.0` / [`derive`] | onlyne-client |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | `1.0.228` / [`derive`] | onlyne-net |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | workspace + `1` / [`derive`] | onlyne-gateway-telegram |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | `1.0` / [] | onlyne-client |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | `1.0.150` / [] | onlyne-net |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit | workspace + `1` / [] | onlyne-gateway-telegram |
| `sha2` | `0.10` / [] | onlyne-config, onlyne-gateway | `0.10.9` / [] | onlyne-net |
| `sha2` | `0.10` / [] | onlyne-config, onlyne-gateway | workspace / [] | onlyne-proto |
| `tempfile` | workspace / [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) | `3` / [] | onlyne-config (dev-dependencies), onlyne-gateway (dev-dependencies), onlyne-layout (dev-dependencies), onlyne-server (dev-dependencies), onlyne-store (dev-dependencies), onlyne-testkit |
| `tempfile` | workspace / [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) | `3.27.0` / [] | onlyne-net (dev-dependencies) |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | workspace / [] | onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1.0` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `fs`, `sync`, `time`, `process`] | onlyne-client |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | workspace / [`rt-multi-thread`, `macros`] | onlyne-frame (dev-dependencies) |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1` / [`fs`, `macros`, `net`, `process`, `rt-multi-thread`, `time`] | onlyne-gateway |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1.52.3` / [`macros`, `net`, `io-util`, `fs`, `time`, `rt-multi-thread`] | onlyne-net |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `sync`, `time`, `fs`] | onlyne-server |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1` / [`io-util`, `io-std`, `net`, `process`, `rt-multi-thread`, `macros`, `sync`, `time`, `fs`] | onlyne-testkit |
| `tracing` | `0.1` / [] | onlyne-adapter, onlyne-client, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-session |
| `uuid` | `1.0` / [`v4`, `serde`] | onlyne-client | workspace / [] | onlyne-proto |
| `uuid` | `1.0` / [`v4`, `serde`] | onlyne-client | `1` / [`v4`, `serde`] | onlyne-server |

Conflicting dependencies by name:

- `anyhow`: `1.0` / [] (onlyne-client); `1` / [] (onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-session).
- `async-trait`: `0.1` / [] (onlyne-adapter, onlyne-gateway, onlyne-testkit); workspace / [] (onlyne-gateway-qqbot, onlyne-gateway-weixin); workspace + `0.1` / [] (onlyne-gateway-telegram).
- `base64`: workspace / [] (onlyne-cli, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto); `0.22` / [] (onlyne-client, onlyne-config, onlyne-gateway, onlyne-server (dev-dependencies), onlyne-testkit); `0.22.1` / [] (onlyne-net); workspace + `0.22` / [] (onlyne-gateway-telegram).
- `chrono`: `0.4` / [`serde`] (onlyne-adapter (dev-dependencies), onlyne-client, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-proto, onlyne-session).
- `clap`: workspace / [] (onlyne-cli); `4.5` / [`derive`] (onlyne-client); `4` / [`derive`] (onlyne-gateway, onlyne-server, onlyne-testkit).
- `ed25519-dalek`: `2.1` / [`std`, `rand_core`]; default-features=false (onlyne-client); `2.1.1` / [`std`, `rand_core`]; default-features=false (onlyne-net).
- `rand`: `0.8` / [] (onlyne-client); `0.8.6` / [] (onlyne-net).
- `rusqlite`: `0.32` / [`bundled`, `chrono`, `serde_json`] (onlyne-client, onlyne-store); `0.32` / [`bundled`] (onlyne-server).
- `schemars`: `0.8` / [] (onlyne-config); workspace / [`chrono`, `uuid1`] (onlyne-proto).
- `serde`: `1` / [`derive`] (onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session); `1.0` / [`derive`] (onlyne-client); `1.0.228` / [`derive`] (onlyne-net); workspace + `1` / [`derive`] (onlyne-gateway-telegram).
- `serde_json`: `1` / [] (onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session); `1.0` / [] (onlyne-client); `1.0.150` / [] (onlyne-net); workspace + `1` / [] (onlyne-gateway-telegram).
- `sha2`: `0.10` / [] (onlyne-config, onlyne-gateway); `0.10.9` / [] (onlyne-net); workspace / [] (onlyne-proto).
- `tempfile`: workspace / [] (onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies)); `3` / [] (onlyne-config (dev-dependencies), onlyne-gateway (dev-dependencies), onlyne-layout (dev-dependencies), onlyne-server (dev-dependencies), onlyne-store (dev-dependencies), onlyne-testkit); `3.27.0` / [] (onlyne-net (dev-dependencies)).
- `tokio`: `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] (onlyne-adapter); workspace / [] (onlyne-cli, onlyne-frame, onlyne-gateway-qqbot, onlyne-gateway-weixin); `1.0` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `fs`, `sync`, `time`, `process`] (onlyne-client); workspace / [`rt-multi-thread`, `macros`] (onlyne-frame (dev-dependencies)); `1` / [`fs`, `macros`, `net`, `process`, `rt-multi-thread`, `time`] (onlyne-gateway); `1.52.3` / [`macros`, `net`, `io-util`, `fs`, `time`, `rt-multi-thread`] (onlyne-net); `1` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `sync`, `time`, `fs`] (onlyne-server); `1` / [`io-util`, `io-std`, `net`, `process`, `rt-multi-thread`, `macros`, `sync`, `time`, `fs`] (onlyne-testkit).
- `tracing`: `0.1` / [] (onlyne-adapter, onlyne-client, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-session).
- `uuid`: `1.0` / [`v4`, `serde`] (onlyne-client); workspace / [] (onlyne-proto); `1` / [`v4`, `serde`] (onlyne-server).

## Manifest source set

| manifest | package |
|---|---|
| `crates/onlyne-adapter/Cargo.toml` | `onlyne-adapter` |
| `crates/onlyne-cli/Cargo.toml` | `onlyne-cli` |
| `crates/onlyne-client/Cargo.toml` | `onlyne-client` |
| `crates/onlyne-config/Cargo.toml` | `onlyne-config` |
| `crates/onlyne-frame/Cargo.toml` | `onlyne-frame` |
| `crates/onlyne-gateway/Cargo.toml` | `onlyne-gateway` |
| `crates/onlyne-layout/Cargo.toml` | `onlyne-layout` |
| `crates/onlyne-net/Cargo.toml` | `onlyne-net` |
| `crates/onlyne-proto/Cargo.toml` | `onlyne-proto` |
| `crates/onlyne-server/Cargo.toml` | `onlyne-server` |
| `crates/onlyne-session/Cargo.toml` | `onlyne-session` |
| `crates/onlyne-store/Cargo.toml` | `onlyne-store` |
| `crates/onlyne-testkit/Cargo.toml` | `onlyne-testkit` |
| `plugins/onlyne-gateway-feishu/Cargo.toml` | `onlyne-gateway-feishu` |
| `plugins/onlyne-gateway-qqbot/Cargo.toml` | `onlyne-gateway-qqbot` |
| `plugins/onlyne-gateway-telegram/Cargo.toml` | `onlyne-gateway-telegram` |
| `plugins/onlyne-gateway-weixin/Cargo.toml` | `onlyne-gateway-weixin` |
