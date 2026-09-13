//! Host detection JSON for `onlyne-client doctor`.

use serde_json::Value;
use std::collections::BTreeMap;

pub fn doctor_report(env: &BTreeMap<String, String>) -> Value {
    onlyne_session::doctor_report(env)
}
