//! Local socket verbs and the workspace `config.toml` plugin array.

mod config;
mod local;
mod plugin;

#[cfg(test)]
mod fixtures;

pub use config::{heal_workspace_config, migrate_plugin_blocks};
pub use local::{
    LocalCli, map_complete, map_control, map_handoff, map_history, map_query_ledger,
    map_query_roles, map_query_sessions, map_reply, map_send, map_subscribe,
};
pub use plugin::{
    CLIENT_PROBE_TIMEOUT, PluginAction, RESTART_HINT, agent_install, agent_mount_frame,
    agent_package_dir, agent_stop_frame, agent_uninstall, install_verb, notify_client,
    plugin_exit_code, uninstall_verb, valid_plugin_id,
};
