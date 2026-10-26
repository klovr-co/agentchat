//! Show an answer while it is written, then confirm it as ordinary messages.
//!
//! `AnswerStream` buffers appended text and sends the whole text so far at most
//! once per `stream_interval` seconds, so a channel can edit one message in place
//! or append to a native stream. Text past the channel's `stream_limit` continues
//! in a new message. Channels without streaming get one reply when the stream
//! finishes. Stream errors propagate; nothing is retried or sent twice.
//!
//! On a `TaskProgressChannel`, `task()` and `plan()` show progress next to the
//! answer, also on channels that send the answer once at the end; other channels
//! ignore them.
use crate::host::coroutine;
use crate::runtime::{call, copy, none};
use agentchat_core::{StreamUpdate, TaskStatus, TaskUpdate};
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::PyTuple,
    PyTraverseError, PyVisit,
};
use std::time::{Duration, Instant};

type Object = Py<PyAny>;
const DEFAULT_INTERVAL: f64 = 1.0;

#[pyclass(module = "agentchat._native")]
pub struct AnswerStream {
    app: Option<Object>,
    channel: Option<Object>,
    source: Option<Object>,
    handle: Option<Object>,
    messages: Vec<Object>,
    /// All appended text; `offset` characters of it are already confirmed messages.
    text: String,
    offset: usize,
    streaming: bool,
    /// Whether the channel shows task and plan updates, and the unsent ones.
    tasks: bool,
    updates: Vec<StreamUpdate>,
    interval: Duration,
    limit: Option<usize>,
    last_flush: Option<Instant>,
    done: bool,
}

fn attribute<T: for<'a, 'py> FromPyObject<'a, 'py>>(
    channel: &Bound<'_, PyAny>,
    name: &str,
) -> Option<T> {
    channel
        .getattr(name)
        .ok()
        .filter(|value| !value.is_none())
        .and_then(|value| value.extract().ok())
}

impl AnswerStream {
    pub fn create(
        py: Python<'_>,
        app: Object,
        channel: Object,
        source: Object,
        interval: Option<f64>,
    ) -> PyResult<Self> {
        let bound = channel.bind(py);
        let streaming = bound.is_instance(
            &py.import("agentchat.channels.base")?
                .getattr("StreamingChannel")?,
        )?;
        let interval = interval
            .or_else(|| attribute(bound, "stream_interval"))
            .unwrap_or(DEFAULT_INTERVAL);
        if !(interval.is_finite() && interval >= 0.0) {
            return Err(PyValueError::new_err(
                "Stream interval must be zero or more seconds",
            ));
        }
        let limit = attribute::<usize>(bound, "stream_limit").filter(|&limit| limit > 0);
        let tasks = bound.is_instance(
            &py.import("agentchat.channels.base")?
                .getattr("TaskProgressChannel")?,
        )?;
        Ok(Self {
            app: Some(app),
            channel: Some(channel),
            source: Some(source),
            handle: None,
            messages: Vec::new(),
            text: String::new(),
            offset: 0,
            streaming,
            tasks,
            updates: Vec::new(),
            interval: Duration::from_secs_f64(interval),
            limit,
            last_flush: None,
            done: false,
        })
    }
    fn parts(&self, py: Python<'_>) -> PyResult<(Object, Object, Object)> {
        match (&self.app, &self.channel, &self.source) {
            (Some(app), Some(channel), Some(source)) => Ok((
                app.clone_ref(py),
                channel.clone_ref(py),
                source.clone_ref(py),
            )),
            _ => Err(PyRuntimeError::new_err("AnswerStream has been cleared")),
        }
    }
}

fn text_object(py: Python<'_>, text: &str) -> PyResult<Object> {
    Ok(text.into_pyobject(py)?.into_any().unbind())
}

/// Send pending task and plan updates, starting the stream with them if needed.
async fn send_updates(slf: &Py<AnswerStream>) -> PyResult<()> {
    let pending = Python::attach(|py| -> PyResult<_> {
        let mut this = slf.borrow_mut(py);
        if this.updates.is_empty() {
            return Ok(None);
        }
        let updates = std::mem::take(&mut this.updates);
        let (_, channel, source) = this.parts(py)?;
        let handle = this
            .handle
            .as_ref()
            .map_or_else(|| py.None(), |h| h.clone_ref(py));
        let updates =
            serde_json::to_value(updates).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let updates = PyTuple::new(
            py,
            updates
                .as_array()
                .into_iter()
                .flatten()
                .map(|update| crate::convert::from_json(py, update))
                .collect::<PyResult<Vec<_>>>()?,
        )?
        .into_any()
        .unbind();
        Ok(Some((channel, source, handle, updates)))
    })?;
    if let Some((channel, source, handle, updates)) = pending {
        let handle = call(
            channel,
            "update_stream_tasks",
            vec![source, handle, updates],
            vec![],
        )
        .await?;
        Python::attach(|py| slf.borrow_mut(py).handle = Some(handle));
    }
    Ok(())
}

/// Send an update when the interval has passed: the answer so far and its
/// tasks, or only the tasks on a channel that sends the answer at the end.
async fn tick(slf: &Py<AnswerStream>) -> PyResult<()> {
    let due = Python::attach(|py| {
        let mut this = slf.borrow_mut(py);
        let due = (this.streaming || this.tasks)
            && this
                .last_flush
                .is_none_or(|last| last.elapsed() >= this.interval);
        if due {
            this.last_flush = Some(Instant::now());
        }
        due.then_some(this.streaming)
    });
    match due {
        Some(true) => flush(slf, false).await?,
        Some(false) => send_updates(slf).await?,
        None => {}
    }
    Ok(())
}

fn open(this: &AnswerStream) -> PyResult<()> {
    if this.done {
        return Err(PyRuntimeError::new_err(
            "This answer stream is already finished",
        ));
    }
    Ok(())
}

/// Send the unconfirmed text: finish full messages, then start or update the last one.
async fn flush(slf: &Py<AnswerStream>, finishing: bool) -> PyResult<()> {
    send_updates(slf).await?;
    loop {
        let (app, channel, source, handle, part, limit) = Python::attach(|py| {
            let this = slf.borrow(py);
            let (app, channel, source) = this.parts(py)?;
            let rest = this.text[this.offset..].to_owned();
            Ok::<_, PyErr>((
                app,
                channel,
                source,
                this.handle.as_ref().map(|h| h.clone_ref(py)),
                rest,
                this.limit,
            ))
        })?;
        let full = limit.is_some_and(|limit| part.encode_utf16().count() > limit);
        if part.is_empty() && !finishing {
            return Ok(());
        }
        if !full {
            if finishing {
                if handle.is_none() && part.is_empty() {
                    return Ok(());
                }
                let text = Python::attach(|py| text_object(py, &part))?;
                let handle = match handle {
                    Some(handle) => handle,
                    None => {
                        call(
                            copy(&channel),
                            "start_stream",
                            vec![copy(&source), copy(&text)],
                            vec![],
                        )
                        .await?
                    }
                };
                let message =
                    call(channel, "stop_stream", vec![source, handle, text], vec![]).await?;
                confirm(slf, &app, message, part.len()).await?;
                return Ok(());
            }
            let text = Python::attach(|py| text_object(py, &part))?;
            let handle = match handle {
                None => call(channel, "start_stream", vec![source, text], vec![]).await?,
                Some(handle) => {
                    call(channel, "append_stream", vec![source, handle, text], vec![]).await?
                }
            };
            Python::attach(|py| slf.borrow_mut(py).handle = Some(handle));
            return Ok(());
        }
        // Close this message at a natural break and continue in the next one.
        let cut = agentchat_core::stream_split(&part, limit.unwrap_or(usize::MAX));
        let head = Python::attach(|py| text_object(py, &part[..cut]))?;
        let handle = match handle {
            Some(handle) => handle,
            None => {
                call(
                    copy(&channel),
                    "start_stream",
                    vec![copy(&source), copy(&head)],
                    vec![],
                )
                .await?
            }
        };
        let message = call(channel, "stop_stream", vec![source, handle, head], vec![]).await?;
        confirm(slf, &app, message, cut).await?;
    }
}

/// Record a confirmed message in history and move past the text it holds.
async fn confirm(
    slf: &Py<AnswerStream>,
    app: &Object,
    message: Object,
    bytes: usize,
) -> PyResult<()> {
    // A message that only showed task progress has no text for history.
    let text = Python::attach(|py| -> PyResult<bool> {
        Ok(!message
            .bind(py)
            .getattr("text")?
            .extract::<String>()?
            .trim()
            .is_empty())
    })?;
    if text {
        let state = Python::attach(|py| app.bind(py).getattr("state").map(Bound::unbind))?;
        call(state, "append", vec![copy(&message)], vec![]).await?;
    }
    Python::attach(|py| {
        let mut this = slf.borrow_mut(py);
        this.offset += bytes;
        this.handle = None;
        this.messages.push(message);
    });
    Ok(())
}

async fn finish(slf: &Py<AnswerStream>) -> PyResult<Object> {
    let (streaming, app, channel, source, text) = Python::attach(|py| {
        let mut this = slf.borrow_mut(py);
        if this.done {
            return Err(PyRuntimeError::new_err(
                "This answer stream is already finished",
            ));
        }
        this.done = true;
        let (app, channel, source) = this.parts(py)?;
        Ok((this.streaming, app, channel, source, this.text.clone()))
    })?;
    if streaming {
        flush(slf, true).await?;
    } else {
        send_updates(slf).await?;
    }
    if !streaming && !text.trim().is_empty() {
        let text = Python::attach(|py| text_object(py, &text))?;
        let message = call(app, "reply", vec![channel, source, text], vec![]).await?;
        Python::attach(|py| slf.borrow_mut(py).messages.push(message));
    }
    Python::attach(|py| {
        Ok(slf
            .borrow(py)
            .messages
            .last()
            .map_or_else(|| py.None(), |m| m.clone_ref(py)))
    })
}

#[pymethods]
impl AnswerStream {
    /// Add text to the answer; sends an update when the interval has passed.
    fn append(slf: Py<Self>, py: Python<'_>, text: String) -> PyResult<Object> {
        coroutine(py, async move {
            Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                open(&this)?;
                this.text.push_str(&text);
                Ok::<_, PyErr>(())
            })?;
            tick(&slf).await?;
            Ok(none())
        })
    }
    /// Show a task's progress with the answer: `status` is "pending",
    /// "in_progress", "complete" or "error". A later call with the same `id`
    /// updates it; `details` and `output` add text. Channels that cannot show
    /// tasks ignore it. On Slack, call `plan()` or `task()` before the first
    /// `append()`: a stream started with text alone cannot show tasks later.
    #[pyo3(signature=(id,title,status="in_progress",*,details=None,output=None))]
    fn task(
        slf: Py<Self>,
        py: Python<'_>,
        id: String,
        title: String,
        status: &str,
        details: Option<String>,
        output: Option<String>,
    ) -> PyResult<Object> {
        let status: TaskStatus = status
            .parse()
            .map_err(|e: agentchat_core::Error| PyValueError::new_err(e.to_string()))?;
        let update = TaskUpdate {
            id,
            title,
            status,
            details,
            output,
        };
        coroutine(py, async move {
            let tasks = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                open(&this)?;
                let mut ignored = Vec::new();
                let tasks = this.tasks;
                let updates = if tasks {
                    &mut this.updates
                } else {
                    &mut ignored
                };
                agentchat_core::queue_task(updates, update)
                    .map_err(|e| PyValueError::new_err(e.to_string()))?;
                Ok::<_, PyErr>(tasks)
            })?;
            if tasks {
                tick(&slf).await?;
            }
            Ok(none())
        })
    }
    /// Group this answer's tasks under a plan with `title`; ignored by channels
    /// that cannot show tasks.
    fn plan(slf: Py<Self>, py: Python<'_>, title: String) -> PyResult<Object> {
        coroutine(py, async move {
            let tasks = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                open(&this)?;
                let mut ignored = Vec::new();
                let tasks = this.tasks;
                let updates = if tasks {
                    &mut this.updates
                } else {
                    &mut ignored
                };
                agentchat_core::queue_plan(updates, &title)
                    .map_err(|e| PyValueError::new_err(e.to_string()))?;
                Ok::<_, PyErr>(tasks)
            })?;
            if tasks {
                tick(&slf).await?;
            }
            Ok(none())
        })
    }
    /// Send the rest and confirm the answer; returns its last message, or None if empty.
    fn finish(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        coroutine(py, async move { finish(&slf).await })
    }
    /// Everything appended so far.
    #[getter]
    fn text(&self) -> &str {
        &self.text
    }
    /// The confirmed messages, in order; long answers span several.
    #[getter]
    fn messages(&self, py: Python<'_>) -> PyResult<Object> {
        Ok(PyTuple::new(py, self.messages.iter().map(|m| m.bind(py)))?
            .into_any()
            .unbind())
    }
    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        let this = slf.clone_ref(py).into_any();
        coroutine(py, async move { Ok(this) })
    }
    fn __aexit__(
        slf: Py<Self>,
        py: Python<'_>,
        _kind: Object,
        error: Object,
        _traceback: Object,
    ) -> PyResult<Object> {
        let failed = !error.is_none(py);
        coroutine(py, async move {
            if !failed {
                finish(&slf).await?;
                return falsy();
            }
            // Close a started stream with what was written; the handler's error wins.
            let started = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                let done = this.done;
                this.done = true;
                let (_, channel, source) = this.parts(py)?;
                Ok::<_, PyErr>(
                    (!done)
                        .then(|| this.handle.as_ref().map(|h| h.clone_ref(py)))
                        .flatten()
                        .map(|handle| {
                            (channel, source, handle, this.text[this.offset..].to_owned())
                        }),
                )
            })?;
            if let Some((channel, source, handle, rest)) = started {
                let text = Python::attach(|py| text_object(py, &rest))?;
                let _ = call(channel, "stop_stream", vec![source, handle, text], vec![]).await;
            }
            falsy()
        })
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.app)?;
        visit.call(&self.channel)?;
        visit.call(&self.source)?;
        visit.call(&self.handle)?;
        for message in &self.messages {
            visit.call(message)?;
        }
        Ok(())
    }
    fn __clear__(&mut self) {
        self.app = None;
        self.channel = None;
        self.source = None;
        self.handle = None;
        self.messages.clear();
    }
}

fn falsy() -> PyResult<Object> {
    Python::attach(|py| Ok(false.into_pyobject(py)?.to_owned().into_any().unbind()))
}

/// Most task messages one answer sends on a channel that cannot edit them.
pub const MAX_TASK_MESSAGES: usize = 5;

/// `update_stream_tasks` for a channel that cannot edit a sent message: when
/// `enabled`, each task that finishes is a short reply, at most
/// `MAX_TASK_MESSAGES` per answer, kept out of history. A failed message is logged
/// and never fails the answer.
pub async fn finished_task_replies(
    channel: Object,
    enabled: bool,
    source: Object,
    stream: Object,
    updates: Object,
    logger: &str,
) -> PyResult<Object> {
    let (sent, lines) = Python::attach(|py| -> PyResult<_> {
        let handle = if stream.is_none(py) {
            serde_json::Value::Null
        } else {
            crate::convert::to_json(stream.bind(py))?
        };
        if !enabled || handle["stopped"] == true {
            return Ok((None, Vec::new()));
        }
        let updates: Vec<StreamUpdate> =
            serde_json::from_value(crate::convert::to_json(updates.bind(py))?)
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut sent: Vec<String> =
            serde_json::from_value(handle["sent"].clone()).unwrap_or_default();
        let lines = agentchat_core::finished_tasks(&updates, &mut sent, MAX_TASK_MESSAGES);
        Ok((Some(sent), lines))
    })?;
    let Some(sent) = sent else {
        return Ok(stream);
    };
    let mut stopped = false;
    for line in lines {
        let text = Python::attach(|py| text_object(py, &line))?;
        let result = call(copy(&channel), "reply", vec![copy(&source), text], vec![]).await;
        if let Err(error) = result {
            Python::attach(|py| -> PyResult<()> {
                py.import("logging")?
                    .call_method1("getLogger", (logger,))?
                    .call_method1(
                        "warning",
                        ("Task progress message failed (%s)", error.to_string()),
                    )?;
                Ok(())
            })?;
            // Send no more for this answer rather than skip some.
            stopped = true;
            break;
        }
    }
    Python::attach(|py| {
        crate::convert::from_json(py, &serde_json::json!({"sent": sent, "stopped": stopped}))
    })
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AnswerStream>()
}
