//! What happens to a conversation's messages while its handler is still running.
//!
//! Each running handler has a `RunningTurn`. A message that arrives meanwhile is
//! handled by the application's busy policy: steered into the running handler,
//! queued behind it, or ignored. A steered message the handler never reads runs
//! afterwards as its own turn, so steering never loses a message.
use pyo3::{
    exceptions::PyValueError,
    prelude::*,
    types::{PyDict, PyList, PyTuple},
    PyTraverseError, PyVisit,
};

type Object = Py<PyAny>;

/// How a message is handled when its conversation's handler is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Wait for the running handler, then run as the next turn.
    Queue,
    /// Hand the message to the running handler through `context.steered()`.
    Steer,
    /// Ask the message's sender with buttons whether to queue or steer it.
    Ask,
    /// Record nothing and run nothing.
    Reject,
}

impl Busy {
    pub fn parse(value: &str) -> PyResult<Self> {
        match value {
            "queue" => Ok(Self::Queue),
            "steer" => Ok(Self::Steer),
            "reject" => Ok(Self::Reject),
            "ask" => Ok(Self::Ask),
            _ => Err(PyValueError::new_err(
                "busy must be 'steer', 'queue', 'ask', or 'reject'",
            )),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Steer => "steer",
            Self::Reject => "reject",
            Self::Ask => "ask",
        }
    }
}

#[pyclass(module = "agentchat._native")]
pub struct RunningTurn {
    stop: Option<Object>,
    steered: Option<Py<PyList>>,
    /// Messages to run after this turn without showing them to its handler.
    queued: Option<Py<PyList>>,
    /// Prompts that end with this turn: its Stop button and Queue/Steer questions.
    pub prompts: Vec<String>,
}

impl RunningTurn {
    /// A turn for the running event loop; `waiting` are messages already steered to it.
    pub fn create(py: Python<'_>, waiting: Vec<Object>) -> PyResult<Py<Self>> {
        Py::new(
            py,
            Self {
                stop: Some(py.import("asyncio")?.getattr("Event")?.call0()?.unbind()),
                steered: Some(PyList::new(py, waiting)?.unbind()),
                queued: Some(PyList::empty(py).unbind()),
                prompts: Vec::new(),
            },
        )
    }
    fn event<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.stop
            .as_ref()
            .map(|event| event.bind(py).clone())
            .ok_or_else(|| PyValueError::new_err("RunningTurn has been cleared"))
    }
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        self.steered
            .as_ref()
            .map(|list| list.bind(py).clone())
            .ok_or_else(|| PyValueError::new_err("RunningTurn has been cleared"))
    }
    pub fn steer(&self, py: Python<'_>, message: &Bound<'_, PyAny>) -> PyResult<()> {
        self.list(py)?.append(message)
    }
    pub fn queue(&self, py: Python<'_>, message: &Bound<'_, PyAny>) -> PyResult<()> {
        self.queued
            .as_ref()
            .ok_or_else(|| PyValueError::new_err("RunningTurn has been cleared"))?
            .bind(py)
            .append(message)
    }
    fn take_queued(&self, py: Python<'_>) -> PyResult<Vec<Object>> {
        let Some(queued) = &self.queued else {
            return Ok(Vec::new());
        };
        let list = queued.bind(py);
        let messages = list.iter().map(Bound::unbind).collect();
        list.call_method1("clear", ())?;
        Ok(messages)
    }
    /// Messages steered to this turn and not yet read, removing them.
    pub fn take(&self, py: Python<'_>) -> PyResult<Vec<Object>> {
        let list = self.list(py)?;
        let messages = list.iter().map(Bound::unbind).collect();
        list.call_method1("clear", ())?;
        Ok(messages)
    }
    pub fn request_stop(&self, py: Python<'_>) -> PyResult<()> {
        self.event(py)?.call_method0("set").map(|_| ())
    }
    pub fn stopped(&self, py: Python<'_>) -> PyResult<bool> {
        self.event(py)?.call_method0("is_set")?.is_truthy()
    }
    pub fn wait_stopped(&self, py: Python<'_>) -> PyResult<Object> {
        Ok(self.event(py)?.call_method0("wait")?.unbind())
    }
}

#[pymethods]
impl RunningTurn {
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.stop)?;
        visit.call(&self.steered)?;
        visit.call(&self.queued)
    }
    fn __clear__(&mut self) {
        self.stop = None;
        self.steered = None;
        self.queued = None;
    }
}

/// The running turns of an application, by conversation ID.
pub struct Running(pub Py<PyDict>);

impl Running {
    pub fn get(&self, py: Python<'_>, conversation: &Object) -> PyResult<Option<Py<RunningTurn>>> {
        let turn = self.0.bind(py).get_item(conversation.bind(py))?;
        turn.map(|turn| turn.extract::<Py<RunningTurn>>().map_err(Into::into))
            .transpose()
    }
    pub fn start(
        &self,
        py: Python<'_>,
        conversation: &Object,
        waiting: Vec<Object>,
    ) -> PyResult<Py<RunningTurn>> {
        let turn = RunningTurn::create(py, waiting)?;
        self.0.bind(py).set_item(conversation.bind(py), &turn)?;
        Ok(turn)
    }
    /// Unregister `turn` if it is still the conversation's running turn. Returns the
    /// messages to run next: unread steered ones, then queued ones.
    pub fn finish(
        &self,
        py: Python<'_>,
        conversation: &Object,
        turn: &Py<RunningTurn>,
    ) -> PyResult<Vec<Object>> {
        let dict = self.0.bind(py);
        if let Some(current) = dict.get_item(conversation.bind(py))? {
            if current.is(turn.bind(py)) {
                dict.del_item(conversation.bind(py))?;
            }
        }
        let this = turn.borrow(py);
        let mut next = this.take(py)?;
        next.extend(this.take_queued(py)?);
        Ok(next)
    }
}

pub fn messages_tuple(py: Python<'_>, messages: Vec<Object>) -> PyResult<Object> {
    Ok(PyTuple::new(py, messages)?.into_any().unbind())
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<RunningTurn>()
}
