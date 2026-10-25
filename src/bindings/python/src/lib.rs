//! Python drives host effects on its existing task; Rust owns turn transitions.
//!
//! Keeping host objects outside this boundary preserves dataclasses, arbitrary
//! metadata, custom State context managers, and the original exception traceback.

use std::sync::Arc;

use agentchat_core::{MemoryStore, Turn, TurnEffect, TurnResult};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use pyo3::{PyTraverseError, PyVisit};

mod activity;
mod apps;
mod control;
mod convert;
mod delivery;
mod host;
mod integrations;
mod prompts;
mod runtime;
mod state;
mod stream;
mod providers {
    pub mod discord;
    pub mod imessage;
    pub mod slack;
    pub mod slack_files;
    pub mod slack_host;
    pub mod slack_policy;
    pub mod slack_socket;
    pub mod teams;
    pub mod telegram;
    pub mod transport;
    pub mod typing;
    pub mod whatsapp;
    pub mod ycloud;
}

#[pyclass(eq, eq_int, skip_from_py_object, module = "agentchat._native")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Claim,
    AcquireConversation,
    AppendIncoming,
    InvokeHandler,
    SendReply,
    AppendReply,
    Complete,
}

#[pyclass(module = "agentchat._native")]
struct NativeTurn {
    turn: Turn,
}

impl NativeTurn {
    fn resolve(&mut self, result: TurnResult) -> PyResult<()> {
        self.turn
            .resolve(result)
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
    }
}

#[pymethods]
impl NativeTurn {
    #[new]
    fn new() -> Self {
        Self { turn: Turn::new() }
    }

    #[getter]
    fn effect(&self) -> Effect {
        match self.turn.effect() {
            TurnEffect::Claim => Effect::Claim,
            TurnEffect::AcquireConversation => Effect::AcquireConversation,
            TurnEffect::AppendIncoming => Effect::AppendIncoming,
            TurnEffect::InvokeHandler => Effect::InvokeHandler,
            TurnEffect::SendReply(_) => Effect::SendReply,
            TurnEffect::AppendReply => Effect::AppendReply,
            TurnEffect::Complete => Effect::Complete,
        }
    }

    #[getter]
    fn reply_text(&self) -> Option<String> {
        match self.turn.effect() {
            TurnEffect::SendReply(text) => Some(text.clone()),
            _ => None,
        }
    }

    fn claimed(&mut self, accepted: bool) -> PyResult<()> {
        self.resolve(TurnResult::Claimed(accepted))
    }

    fn acquired(&mut self) -> PyResult<()> {
        self.resolve(TurnResult::ConversationAcquired)
    }

    fn incoming_appended(&mut self) -> PyResult<()> {
        self.resolve(TurnResult::IncomingAppended)
    }

    #[pyo3(signature = (content=None))]
    fn handler_completed(&mut self, content: Option<String>) -> PyResult<()> {
        self.resolve(TurnResult::HandlerCompleted(content))
    }

    fn reply_sent(&mut self) -> PyResult<()> {
        self.resolve(TurnResult::ReplySent)
    }

    fn reply_appended(&mut self) -> PyResult<()> {
        self.resolve(TurnResult::ReplyAppended)
    }
}

#[pyfunction]
fn thread_history_indices(length: usize, limit: usize) -> PyResult<Vec<usize>> {
    agentchat_core::thread_history_indices(length, limit)
        .map_err(|error| PyValueError::new_err(error.to_string()))
}

/// The core retains owned Python objects without converting their metadata.
/// The dictionary is a live compatibility view used by MemoryState.messages.
#[pyclass(module = "agentchat._native")]
pub(crate) struct NativeMemoryStore {
    store: MemoryStore<Arc<Py<PyAny>>>,
    view: Option<Py<PyDict>>,
}

#[pymethods]
impl NativeMemoryStore {
    #[new]
    fn new(py: Python<'_>) -> Self {
        Self {
            store: MemoryStore::new(),
            view: Some(PyDict::new(py).unbind()),
        }
    }

    fn claim(&mut self, message_id: &str) -> bool {
        self.store.claim(&message_id.into())
    }

    fn append(&mut self, py: Python<'_>, conversation_id: &str, value: Py<PyAny>) -> PyResult<()> {
        let conversation = conversation_id.into();
        self.store.append(&conversation, Arc::new(value));
        let values = self.store.history(&conversation, None);
        let tuple = PyTuple::new(py, values.iter().map(|value| value.bind(py)))?;
        self.view
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Memory state has been cleared"))?
            .bind(py)
            .set_item(conversation_id, tuple)
    }

    #[getter]
    fn messages(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        self.view
            .as_ref()
            .map(|view| view.clone_ref(py))
            .ok_or_else(|| PyRuntimeError::new_err("Memory state has been cleared"))
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> std::result::Result<(), PyTraverseError> {
        visit.call(&self.view)?;
        for value in self.store.values() {
            visit.call(value.as_ref())?;
        }
        Ok(())
    }

    fn __clear__(&mut self) {
        self.store = MemoryStore::new();
        self.view = None;
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    host::register(module)?;
    providers::slack_socket::register(module)?;
    delivery::register(module)?;
    apps::register(module)?;
    state::register(module)?;
    integrations::register(module)?;
    activity::register(module)?;
    control::register(module)?;
    stream::register(module)?;
    prompts::register(module)?;
    runtime::register(module)?;
    providers::slack::register(module)?;
    providers::slack_files::register(module)?;
    providers::slack_host::register(module)?;
    providers::slack_policy::register(module)?;
    providers::discord::register(module)?;
    providers::imessage::register(module)?;
    providers::teams::register(module)?;
    providers::telegram::register(module)?;
    providers::typing::register(module)?;
    providers::whatsapp::register(module)?;
    providers::ycloud::register(module)?;
    module.add_class::<Effect>()?;
    module.add_class::<NativeTurn>()?;
    module.add_class::<NativeMemoryStore>()?;
    module.add_function(wrap_pyfunction!(thread_history_indices, module)?)?;
    module.add("core_version", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
