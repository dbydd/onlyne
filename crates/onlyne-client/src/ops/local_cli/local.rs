//! Local CLI handlers for the socket verb surface.

use crate::runtime::intent::{IntentMachine, stamp_op_id};
use anyhow::{Result, anyhow};
use onlyne_proto::{
    AdapterMsg, ClientOp, ControlArgs, Envelope, HistoryArgs, LedgerQuery, PluginOp,
    QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, ResBody, Subscribe,
};

/// Local CLI handlers share the intent queue with plugin-originated sends.
#[derive(Clone)]
pub struct LocalCli {
    pub intents: IntentMachine,
    pub role: String,
}

impl LocalCli {
    pub fn new(intents: IntentMachine) -> Self {
        let role = intents
            .store
            .config("role")
            .ok()
            .flatten()
            .unwrap_or_default();
        Self { intents, role }
    }

    pub fn with_role(intents: IntentMachine, role: impl Into<String>) -> Self {
        Self {
            intents,
            role: role.into(),
        }
    }

    /// Queue one outbound send and answer the stamped envelope.
    ///
    /// The queue keys its row with an `op_id`; a note arrives without one, so
    /// the stamp lands before the envelope becomes the `ClientOp` the caller
    /// writes, and the frame on the wire carries the id the row stored.
    pub fn send(&self, mut envelope: Envelope) -> Result<ClientOp> {
        stamp_op_id(&mut envelope);
        self.intents.enqueue(&envelope)?;
        Ok(ClientOp::Send(Box::new(envelope)))
    }

    pub fn reply(&self, envelope: Envelope) -> Result<ClientOp> {
        self.send(envelope)
    }
    pub fn complete(&self, envelope: Envelope) -> Result<ClientOp> {
        self.send(envelope)
    }
    pub fn handoff(&self, envelope: Envelope) -> Result<ClientOp> {
        self.send(envelope)
    }
    pub fn control(&self, args: ControlArgs) -> ClientOp {
        ClientOp::Control(args)
    }
    pub fn query_sessions(&self, args: QuerySessionsArgs) -> ClientOp {
        ClientOp::QuerySessions(args)
    }

    /// Build the server query shape for callers that need a wire request.
    pub fn query_roles(&self, args: QueryRolesArgs) -> ClientOp {
        ClientOp::QueryRoles(args)
    }

    /// Answer the role query from durable local prose cache. `QueryRolesArgs`
    /// is the only role-query type in onlyne-proto and has the field `role`.
    pub fn query_roles_local(&self, args: &QueryRolesArgs) -> Result<ResBody> {
        let role = args.role.as_deref().unwrap_or(&self.role);
        let Some((prose, spec_hash)) = self.intents.store.prose(role)? else {
            return Ok(ResBody::ok(serde_json::json!({"roles": []})));
        };
        Ok(ResBody::ok(serde_json::json!({
            "roles": [{"name": role, "role": role, "prose": prose, "spec_hash": spec_hash}]
        })))
    }

    /// Client-surface export used by `cluster export-prose`.
    pub fn export_prose(&self) -> Result<ResBody> {
        let query = QueryRolesArgs {
            role: Some(self.role.clone()),
        };
        self.query_roles_local(&query)
    }

    pub fn query_ledger(&self, args: LedgerQuery) -> ClientOp {
        ClientOp::QueryLedger(args)
    }
    pub fn subscribe(&self, args: Subscribe) -> ClientOp {
        ClientOp::Subscribe(args)
    }
    pub fn history(&self, args: HistoryArgs) -> Result<ClientOp> {
        if args.limit == 0 {
            return Err(anyhow!("history limit must be positive"));
        }
        Ok(ClientOp::QueryFaults(QueryFaultsArgs {
            role: None,
            task_id: args.task_id,
            kind: args.kind,
            open_only: false,
            limit: args.limit,
        }))
    }

    /// Queue one plugin `send` and answer the key its intent row carries.
    ///
    /// A note arrives without an `op_id`, so the stamp happens before the
    /// reply is built: the plugin reads the id the queue stored rather than
    /// `null`, and the stored envelope is the one that replays.
    pub fn offline_send(&self, envelope: &Envelope) -> Result<ResBody> {
        let mut stamped = envelope.clone();
        let op_id = stamp_op_id(&mut stamped);
        self.intents.enqueue(&stamped)?;
        Ok(ResBody::ok(
            serde_json::json!({"queued": true, "op_id": op_id}),
        ))
    }

    pub async fn handle(&self, message: AdapterMsg) -> Result<ResBody> {
        match message {
            AdapterMsg::Plugin(PluginOp::Send(envelope)) => self.offline_send(&envelope),
            AdapterMsg::Plugin(PluginOp::Detach(_)) => Ok(ResBody::ok(serde_json::Value::Null)),
            AdapterMsg::Plugin(PluginOp::Hello(_)) => Ok(ResBody::err(
                onlyne_proto::ErrorCode::Invalid,
                "hello already completed",
                Some("op".into()),
            )),
            _ => Ok(ResBody::err(
                onlyne_proto::ErrorCode::UnknownOp,
                "unsupported local cli operation",
                Some("op".into()),
            )),
        }
    }
}

pub fn map_send(envelope: Envelope) -> Result<ClientOp> {
    envelope.validate().map_err(|e| anyhow!(e.to_string()))?;
    Ok(ClientOp::Send(Box::new(envelope)))
}
pub fn map_reply(envelope: Envelope) -> Result<ClientOp> {
    map_send(envelope)
}
pub fn map_complete(envelope: Envelope) -> Result<ClientOp> {
    map_send(envelope)
}
pub fn map_handoff(envelope: Envelope) -> Result<ClientOp> {
    map_send(envelope)
}
pub fn map_control(args: ControlArgs) -> ClientOp {
    ClientOp::Control(args)
}
pub fn map_query_sessions(args: QuerySessionsArgs) -> ClientOp {
    ClientOp::QuerySessions(args)
}
pub fn map_query_roles(args: QueryRolesArgs) -> ClientOp {
    ClientOp::QueryRoles(args)
}
pub fn map_query_ledger(args: LedgerQuery) -> ClientOp {
    ClientOp::QueryLedger(args)
}
pub fn map_subscribe(args: Subscribe) -> ClientOp {
    ClientOp::Subscribe(args)
}
pub fn map_history(args: HistoryArgs) -> ClientOp {
    ClientOp::QueryFaults(QueryFaultsArgs {
        role: None,
        task_id: args.task_id,
        kind: args.kind,
        open_only: false,
        limit: args.limit,
    })
}

#[cfg(test)]
mod tests;
