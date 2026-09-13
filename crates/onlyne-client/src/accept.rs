use crate::dispatch::{DispatchState, dispatch};
use anyhow::{Context, Result, anyhow};
use onlyne_proto::{Delivery, Envelope, Report};
use onlyne_session::{SessionBackend, SessionRef};
use onlyne_store::ClientStore;
use std::sync::Arc;

#[derive(Clone)]
pub struct AcceptPath {
    pub dispatch: DispatchState,
    pub prose: String,
}

impl AcceptPath {
    pub fn new(dispatch: DispatchState, prose: impl Into<String>) -> Self {
        Self {
            dispatch,
            prose: prose.into(),
        }
    }

    pub fn accept_new(&self, delivery: &Delivery, accept_new: bool) -> Result<Option<SessionRef>> {
        if !accept_new {
            return Ok(None);
        }
        delivery
            .envelope
            .validate()
            .map_err(|e| anyhow!(e.to_string()))?;
        let task_id = delivery
            .envelope
            .task_id()
            .context("delivery missing causality.task")?;
        let _ = task_id;
        Ok(Some(dispatch(&self.dispatch, &delivery.envelope)?))
    }

    pub fn ready_report(
        &self,
        task_id: &str,
        session_id: &str,
        generation: u64,
        seq: u64,
    ) -> Report {
        Report::Ready {
            task_id: task_id.into(),
            session_id: session_id.into(),
            generation,
            seq,
            cluster_ref: None,
        }
    }
}

pub fn validate_delivery(delivery: &Delivery) -> Result<&Envelope> {
    delivery
        .envelope
        .validate()
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(&delivery.envelope)
}

pub fn resolve_session(
    path: &AcceptPath,
    delivery: &Delivery,
    accept_new: bool,
) -> Result<Option<SessionRef>> {
    validate_delivery(delivery)?;
    path.accept_new(delivery, accept_new)
}

pub async fn acknowledge_completion(
    report: &Report,
    _store: &ClientStore,
) -> Result<Option<(String, onlyne_proto::Outcome)>> {
    match report {
        Report::Complete {
            task_id, outcome, ..
        } => Ok(Some((task_id.clone(), *outcome))),
        _ => Ok(None),
    }
}

pub fn backend_for_accept(backend: Arc<dyn SessionBackend>) -> Arc<dyn SessionBackend> {
    backend
}
