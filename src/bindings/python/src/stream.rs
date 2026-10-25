//! Show an answer while it is written, then confirm it as ordinary messages.
//!
//! `AnswerStream` buffers appended text and sends the whole text so far at most
//! once per `stream_interval` seconds, so a channel can edit one message in place
//! or append to a native stream. Text past the channel's `stream_limit` continues
//! in a new message. Channels without streaming get one reply when the stream
//! finishes. Stream errors propagate; nothing is retried or sent twice.
use crate::host::coroutine;
use crate::runtime::{call, copy, none};
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
        Ok(Self {
            app: Some(app),
            channel: Some(channel),
            source: Some(source),
            handle: None,
            messages: Vec::new(),
            text: String::new(),
            offset: 0,
            streaming,
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

/// Send the unconfirmed text: finish full messages, then start or update the last one.
async fn flush(slf: &Py<AnswerStream>, finishing: bool) -> PyResult<()> {
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
    let state = Python::attach(|py| app.bind(py).getattr("state").map(Bound::unbind))?;
    call(state, "append", vec![copy(&message)], vec![]).await?;
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
    } else if !text.trim().is_empty() {
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
            let due = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                if this.done {
                    return Err(PyRuntimeError::new_err(
                        "This answer stream is already finished",
                    ));
                }
                this.text.push_str(&text);
                let due = this.streaming
                    && this
                        .last_flush
                        .is_none_or(|last| last.elapsed() >= this.interval);
                if due {
                    this.last_flush = Some(Instant::now());
                }
                Ok(due)
            })?;
            if due {
                flush(&slf, false).await?;
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

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AnswerStream>()
}
