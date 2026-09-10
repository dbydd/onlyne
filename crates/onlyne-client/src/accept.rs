use anyhow::{Context, Result, anyhow};
use onlyne_proto::{Delivery, Envelope, Report};
use onlyne_session::{SessionBackend, SessionRef, SpawnSpec};
use onlyne_store::ClientStore;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use crate::dispatch::{DispatchState, dispatch};

#[derive(Clone)]
pub struct AcceptPath {
    pub dispatch: DispatchState,
    pub prose: String,
}

impl AcceptPath {
    pub fn new(dispatch: DispatchState, prose: impl Into<String>) -> Self {
        Self { dispatch, prose: prose.into() }
    }

    pub fn accept_new(&self, delivery: &Delivery, accept_new: bool) -> Result<Option<SessionRef>> {
        if !accept_new { return Ok(None); }
        delivery.envelope.validate().map_err(|e| anyhow!(e.to_string()))?;
        let task = delivery.envelope.task_id().context("delivery missing causality.task")?;
        Ok(Some(dispatch(&self.dispatch, &delivery.envelope)?))
    }

    pub fn spawn_spec(
        &self,
        workspace: &Path,
        role: &str,
        session_command: &[String],
        session_id: &str,
        task_id: &str,
        env: &BTreeMap<String, String>,
    ) -> SpawnSpec {
        let mut merged = env.clone();
        merged.insert("ONLYNE_SESSION_ID".into(), session_id.into());
        merged.insert("ONLYNE_TASK_ID".into(), task_id.into());
        merged.insert("ONLYNE_ROLE".into(), role.into());
        SpawnSpec {
            cwd: workspace.to_path_buf(),
            task_id: task_id.into(),
            command: session_command.iter().map(|item| item.replace("{session}", session_id).replace("{task}", task_id)).collect(),
            env: merged,
            focus: None,
            rename: None,
        }
    }

    pub fn ready_report(&self, task_id: &str, session_id: &str, generation: u64, seq: u64) -> Report {
        Report::Ready { task_id: task_id.into(), session_id: session_id.into(), generation, seq }
    }
}

pub fn validate_delivery(delivery: &Delivery) -> Result<&Envelope> {
    delivery.envelope.validate().map_err(|e| anyhow!(e.to_string()))?;
    Ok(&delivery.envelope)
}

pub fn resolve_session(path: &AcceptPath, delivery: &Delivery, accept_new: bool) -> Result<Option<SessionRef>> {
    validate_delivery(delivery)?;
    path.accept_new(delivery, accept_new)
}

pub async fn acknowledge_completion(
    report: &Report,
    _store: &ClientStore,
) -> Result<Option<(String, onlyne_proto::Outcome)>> {
    match report {
        Report::Complete { task_id, outcome, .. } => Ok(Some((task_id.clone(), *outcome))),
        _ => Ok(None),
    }
}

pub fn backend_for_accept(backend: Arc<dyn SessionBackend>) -> Arc<dyn SessionBackend> { backend }
