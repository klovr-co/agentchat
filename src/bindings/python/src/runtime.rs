use crate::control::{messages_tuple, Busy, Running, RunningTurn};
use crate::host::{await_py, coroutine};
use crate::prompts::{self, Prompts};
use agentchat_core::{HistoryLimit, Turn, TurnEffect, TurnResult};
use pyo3::{
    exceptions::{PyRuntimeError, PyTypeError, PyValueError},
    prelude::*,
    types::{PyDict, PyTuple},
    PyTraverseError, PyVisit,
};
type Object = Py<PyAny>;
pub fn copy(value: &Object) -> Object {
    Python::attach(|py| value.clone_ref(py))
}
pub fn none() -> Object {
    Python::attach(|py| py.None())
}
pub async fn call(
    target: Object,
    method: &str,
    args: Vec<Object>,
    kwargs: Vec<(&str, Object)>,
) -> PyResult<Object> {
    let pending = Python::attach(|py| -> PyResult<_> {
        let kw = PyDict::new(py);
        for (key, value) in kwargs {
            kw.set_item(key, value)?;
        }
        Ok(target
            .bind(py)
            .call_method(method, PyTuple::new(py, args)?, Some(&kw))?
            .unbind())
    })?;
    await_py(pending).await
}
/// What a message hidden by `delete(fallback="edit")` says instead.
const REMOVED_TEXT: &str = "(This message was removed.)";
/// The reply `delete(fallback="notice")` sends where nothing can hide a message.
const NOTICE_TEXT: &str = "Please disregard my previous message.";
fn capability(py: Python<'_>, channel: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    channel.is_instance(&py.import("agentchat.channels.base")?.getattr(name)?)
}
pub async fn exit_scope(scope: Object, error: Option<&PyErr>) -> PyResult<bool> {
    let args = Python::attach(|py| match error {
        Some(error) => vec![
            error.get_type(py).into_any().unbind(),
            error.value(py).clone().into_any().unbind(),
            error
                .traceback(py)
                .map(|v| v.into_any().unbind())
                .unwrap_or_else(|| py.None()),
        ],
        None => vec![py.None(), py.None(), py.None()],
    });
    let result = call(scope, "__aexit__", args, vec![]).await?;
    Python::attach(|py| result.bind(py).is_truthy())
}
#[pyclass(
    name = "_AgentChatBase",
    dict,
    subclass,
    weakref,
    module = "agentchat._native"
)]
pub struct AgentChat {
    channels: Option<Object>,
    state: Option<Object>,
    handler: Option<Object>,
    busy: Busy,
    running: Option<Py<PyDict>>,
    prompts: Option<Py<PyDict>>,
}
impl AgentChat {
    fn running(&self, py: Python<'_>) -> PyResult<Running> {
        self.running
            .as_ref()
            .map(|running| Running(running.clone_ref(py)))
            .ok_or_else(|| PyRuntimeError::new_err("AgentChat is uninitialized"))
    }
    fn prompts(&self, py: Python<'_>) -> PyResult<Prompts> {
        self.prompts
            .as_ref()
            .map(|prompts| Prompts(prompts.clone_ref(py)))
            .ok_or_else(|| PyRuntimeError::new_err("AgentChat is uninitialized"))
    }
}
#[pymethods]
impl AgentChat {
    #[new]
    #[pyo3(signature=(*_args,**_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self {
            channels: None,
            state: None,
            handler: None,
            busy: Busy::Steer,
            running: None,
            prompts: None,
        }
    }
    #[pyo3(signature=(*,channels,state=None,backend=None,busy="steer"))]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        channels: &Bound<'_, PyAny>,
        state: Option<Object>,
        backend: Option<&str>,
        busy: &str,
    ) -> PyResult<()> {
        crate::state::backend(backend)?;
        let busy = Busy::parse(busy)?;
        let channels = PyTuple::new(py, channels.try_iter()?.collect::<PyResult<Vec<_>>>()?)?;
        if channels.is_empty() {
            return Err(PyValueError::new_err(
                "AgentChat requires at least one channel",
            ));
        }
        let state = match state {
            Some(state) if state.bind(py).is_truthy()? => state,
            _ => py.get_type::<crate::state::MemoryState>().call0()?.unbind(),
        };
        {
            let mut this = slf.borrow_mut(py);
            this.channels = Some(channels.clone().unbind().into_any());
            this.state = Some(state);
            this.handler = None;
            this.busy = busy;
            this.running = Some(PyDict::new(py).unbind());
            this.prompts = Some(PyDict::new(py).unbind());
        }
        let receiver = slf.bind(py).getattr("_receive")?;
        for channel in channels.iter() {
            channel.call_method1("bind", (&receiver,))?;
        }
        Ok(())
    }
    #[getter]
    fn channels(&self, py: Python<'_>) -> PyResult<Object> {
        self.channels
            .as_ref()
            .map(|v| v.clone_ref(py))
            .ok_or_else(|| PyRuntimeError::new_err("AgentChat is uninitialized"))
    }
    #[setter(channels)]
    fn set_channels(&mut self, channels: Object) {
        self.channels = Some(channels);
    }
    #[getter]
    fn state(&self, py: Python<'_>) -> PyResult<Object> {
        self.state
            .as_ref()
            .map(|v| v.clone_ref(py))
            .ok_or_else(|| PyRuntimeError::new_err("AgentChat is uninitialized"))
    }
    #[setter(state)]
    fn set_state(&mut self, state: Object) {
        self.state = Some(state);
    }
    #[getter]
    fn backend(&self) -> &'static str {
        "rust"
    }
    /// What a message does while its conversation's handler runs: steer, queue, or reject.
    #[getter]
    fn busy(&self) -> &'static str {
        self.busy.name()
    }
    /// Show `text` with one button per choice and wait for an allowed person to press
    /// one. Raises TypeError on channels without buttons and TimeoutError when
    /// nobody answers, so an unanswered approval is never taken as a yes.
    #[pyo3(signature=(channel,source,text,choices,*,timeout=600.0,allowed=None))]
    #[allow(clippy::too_many_arguments)]
    fn ask(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        text: String,
        choices: Object,
        timeout: f64,
        allowed: Option<Object>,
    ) -> PyResult<Object> {
        let registry = slf.borrow(py).prompts(py)?;
        coroutine(
            py,
            prompts::ask(registry, channel, source, text, choices, timeout, allowed),
        )
    }
    /// Show a Stop button for the conversation's running handler; returns the shown
    /// message, or None on channels without buttons. It goes away when the turn ends.
    #[pyo3(signature=(channel,source,*,text="Working on it…".to_owned(),label="Stop".to_owned(),allowed=None))]
    #[allow(clippy::too_many_arguments)]
    fn offer_stop(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        text: String,
        label: String,
        allowed: Option<Object>,
    ) -> PyResult<Object> {
        let this = slf.borrow(py);
        let conversation = source.bind(py).getattr("conversation_id")?.unbind();
        let turn = this.running(py)?.get(py, &conversation)?.ok_or_else(|| {
            PyRuntimeError::new_err("No handler is running for this conversation")
        })?;
        let registry = this.prompts(py)?;
        coroutine(
            py,
            prompts::offer_stop(registry, channel, source, turn, text, label, allowed),
        )
    }
    /// Ask the conversation's running handler to stop; False when none is running.
    fn stop(&self, py: Python<'_>, conversation_id: Object) -> PyResult<bool> {
        match self.running(py)?.get(py, &conversation_id)? {
            Some(turn) => {
                turn.borrow(py).request_stop(py)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
    #[getter(_handler)]
    fn handler(&self, py: Python<'_>) -> Object {
        self.handler
            .as_ref()
            .map(|v| v.clone_ref(py))
            .unwrap_or_else(|| py.None())
    }
    fn on_message(&mut self, py: Python<'_>, handler: Object) -> PyResult<Object> {
        if self.handler.is_some() {
            return Err(PyRuntimeError::new_err(
                "AgentChat supports one message handler",
            ));
        }
        self.handler = Some(handler.clone_ref(py));
        Ok(handler)
    }
    fn run(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        coroutine(py, async move {
            let pending = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                if this.handler.is_none() {
                    return Err(PyRuntimeError::new_err(
                        "Register a message handler with @app.on_message",
                    ));
                }
                let channels = this.channels(py)?;
                drop(this);
                let futures: Vec<_> = channels
                    .bind(py)
                    .try_iter()?
                    .map(|channel| channel?.call_method0("run"))
                    .collect::<PyResult<_>>()?;
                Ok(py
                    .import("asyncio")?
                    .call_method1("gather", PyTuple::new(py, futures)?)?
                    .unbind())
            })?;
            await_py(pending).await?;
            Ok(none())
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        coroutine(py, async move {
            let pending = Python::attach(|py| -> PyResult<_> {
                let channels = slf.borrow(py).channels(py)?;
                let futures: Vec<_> = channels
                    .bind(py)
                    .try_iter()?
                    .map(|channel| channel?.call_method0("close"))
                    .collect::<PyResult<_>>()?;
                Ok(py
                    .import("asyncio")?
                    .call_method1("gather", PyTuple::new(py, futures)?)?
                    .unbind())
            })?;
            await_py(pending).await?;
            Ok(none())
        })
    }
    fn reply(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        content: Object,
    ) -> PyResult<Object> {
        send_reply(py, slf.into_any(), channel, source, content, false)
    }
    fn reply_rich(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        content: Object,
    ) -> PyResult<Object> {
        send_reply(py, slf.into_any(), channel, source, content, true)
    }
    fn upload_files(
        &self,
        py: Python<'_>,
        channel: Object,
        source: Object,
        files: Object,
    ) -> PyResult<Object> {
        coroutine(py, async move {
            Python::attach(|py| -> PyResult<()> {
                if !capability(py, channel.bind(py), "FileUploadChannel")? {
                    return Err(PyTypeError::new_err(
                        "This channel does not support file uploads",
                    ));
                }
                Ok(())
            })?;
            call(channel, "upload_files", vec![source, files], vec![]).await
        })
    }
    fn set_status(
        &self,
        py: Python<'_>,
        channel: Object,
        source: Object,
        status: Object,
    ) -> PyResult<Object> {
        coroutine(py, async move {
            if !Python::attach(|py| capability(py, channel.bind(py), "StatusChannel"))? {
                return Python::attach(|py| {
                    Ok(false.into_pyobject(py)?.to_owned().unbind().into_any())
                });
            }
            call(channel, "set_status", vec![source, status], vec![]).await
        })
    }
    #[pyo3(signature=(channel,source,attachment,*,max_bytes=10*1024*1024))]
    fn download_attachment(
        &self,
        py: Python<'_>,
        channel: Object,
        source: Object,
        attachment: Object,
        max_bytes: usize,
    ) -> PyResult<Object> {
        coroutine(py, async move {
            Python::attach(|py| -> PyResult<()> {
                if !capability(py, channel.bind(py), "FileDownloadChannel")? {
                    return Err(PyTypeError::new_err(
                        "This channel does not support attachment downloads",
                    ));
                }
                if !source
                    .bind(py)
                    .getattr("attachments")?
                    .contains(attachment.bind(py))?
                {
                    return Err(PyValueError::new_err(
                        "Attachment does not belong to the source message",
                    ));
                }
                Ok(())
            })?;
            let max = Python::attach(|py| -> PyResult<_> {
                Ok(max_bytes.into_pyobject(py)?.unbind().into_any())
            })?;
            call(
                channel,
                "download_attachment",
                vec![source, attachment],
                vec![("max_bytes", max)],
            )
            .await
        })
    }
    /// Post a notice in `source`'s conversation that only `user_id` can see. It is
    /// not added to history. Channels without private notices raise `TypeError`;
    /// the notice is never posted publicly instead. With `fallback="dm"`, channels
    /// that support it send a notice they cannot show privately to `user_id`'s
    /// direct chat instead.
    #[pyo3(signature=(channel,source,user_id,text,*,blocks=None,fallback=None))]
    #[allow(clippy::too_many_arguments)]
    fn send_ephemeral(
        &self,
        py: Python<'_>,
        channel: Object,
        source: Object,
        user_id: Object,
        text: Object,
        blocks: Option<Object>,
        fallback: Option<Object>,
    ) -> PyResult<Object> {
        let blocks = blocks.unwrap_or_else(|| py.None());
        let mut kwargs = vec![("blocks", blocks)];
        if let Some(fallback) = fallback.filter(|f| !f.is_none(py)) {
            kwargs.push(("fallback", fallback));
        }
        coroutine(py, async move {
            Python::attach(|py| -> PyResult<()> {
                if !capability(py, channel.bind(py), "EphemeralChannel")? {
                    return Err(PyTypeError::new_err(
                        "This channel does not support ephemeral messages",
                    ));
                }
                if user_id.bind(py).extract::<String>()?.trim().is_empty() {
                    return Err(PyValueError::new_err(
                        "An ephemeral message needs a recipient",
                    ));
                }
                Ok(())
            })?;
            call(
                channel,
                "send_ephemeral",
                vec![source, user_id, text],
                kwargs,
            )
            .await
        })
    }
    /// Delete a message this application sent, as well as the channel allows,
    /// and return what happened; it never raises for a message that cannot be
    /// deleted. History keeps the message. Returns `"deleted"`, or `"failed"`
    /// (logged) when the channel cannot or will not delete it. `fallback` hides
    /// such a message instead: `"edit"` replaces its text with `text` on channels
    /// that can edit (`"edited"`), and `"notice"` also replies to it with `text` on
    /// channels that can neither delete nor edit (`"noticed"`). A notice is never
    /// sent after a failed delete or edit, whose outcome may be uncertain.
    #[pyo3(signature=(channel,message,*,fallback=None,text=None))]
    fn delete(
        &self,
        py: Python<'_>,
        channel: Object,
        message: Object,
        fallback: Option<String>,
        text: Option<String>,
    ) -> PyResult<Object> {
        let fallback = match fallback.as_deref() {
            None => None,
            Some(mode @ ("edit" | "notice")) => Some(mode.to_owned()),
            Some(_) => {
                return Err(PyValueError::new_err(
                    "fallback must be None, \"edit\" or \"notice\"",
                ))
            }
        };
        let (deleting, editing) = (
            capability(py, channel.bind(py), "DeletingChannel")?,
            capability(py, channel.bind(py), "EditingChannel")?,
        );
        coroutine(py, async move {
            let replacement = |default: &str| {
                Python::attach(|py| {
                    text.as_deref()
                        .unwrap_or(default)
                        .into_pyobject(py)
                        .map(|t| t.into_any().unbind())
                })
            };
            let attempt = async {
                if deleting {
                    let deleted =
                        call(copy(&channel), "delete", vec![copy(&message)], vec![]).await;
                    return match deleted {
                        Ok(_) => Ok("deleted"),
                        // Editing a message that was deleted after all only fails.
                        Err(error) if fallback.is_some() && editing => {
                            let text = replacement(REMOVED_TEXT)?;
                            match call(copy(&channel), "edit", vec![copy(&message), text], vec![])
                                .await
                            {
                                Ok(_) => Ok("edited"),
                                Err(_) => Err(error),
                            }
                        }
                        Err(error) => Err(error),
                    };
                }
                match fallback.as_deref() {
                    Some(_) if editing => {
                        let text = replacement(REMOVED_TEXT)?;
                        call(copy(&channel), "edit", vec![copy(&message), text], vec![]).await?;
                        Ok("edited")
                    }
                    Some("notice") => {
                        let text = replacement(NOTICE_TEXT)?;
                        call(copy(&channel), "reply", vec![copy(&message), text], vec![]).await?;
                        Ok("noticed")
                    }
                    _ => Err(PyTypeError::new_err(
                        "This channel does not support deleting messages",
                    )),
                }
            };
            let outcome = match attempt.await {
                Ok(outcome) => outcome,
                Err(error) => Python::attach(|py| {
                    // Cancellation and other non-errors still propagate.
                    if !error.is_instance_of::<pyo3::exceptions::PyException>(py) {
                        return Err(error);
                    }
                    py.import("logging")?
                        .call_method1("getLogger", ("agentchat",))?
                        .call_method1("warning", ("Message not deleted (%s)", error.to_string()))?;
                    Ok("failed")
                })?,
            };
            Python::attach(|py| Ok(outcome.into_pyobject(py)?.into_any().unbind()))
        })
    }
    /// Replace the text of a message this application sent; returns the edited
    /// message. History keeps the original. Channels that cannot edit raise
    /// `TypeError`.
    fn edit(
        &self,
        py: Python<'_>,
        channel: Object,
        message: Object,
        text: Object,
    ) -> PyResult<Object> {
        if !capability(py, channel.bind(py), "EditingChannel")? {
            return Err(PyTypeError::new_err(
                "This channel does not support editing messages",
            ));
        }
        coroutine(py, async move {
            call(channel, "edit", vec![message, text], vec![]).await
        })
    }
    /// Ask `form` of `source`'s sender in its conversation, one field at a time,
    /// on any channel: buttons for choices where the channel has them, numbered
    /// options otherwise. Their next messages answer it. The values reach the
    /// channel's `on_view(form["callback_id"])` handler, as `{"type":
    /// "form_submission", "values": ...}`, or `{"type": "form_closed", "reason":
    /// "cancelled" | "timeout"}`. A form already open for that person is replaced.
    fn open_form(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        form: Object,
    ) -> PyResult<Object> {
        let app = slf.into_any();
        coroutine(py, async move {
            crate::forms::open(app, channel, source, form).await?;
            Ok(none())
        })
    }
    #[pyo3(signature=(channel,source,message,*,origin="web"))]
    fn mirror(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        message: Object,
        origin: &str,
    ) -> PyResult<Object> {
        let origin = origin.to_owned();
        coroutine(py, async move {
            Python::attach(|py| -> PyResult<()> {
                if message.bind(py).getattr("role")?.extract::<String>()? != "user" {
                    return Err(PyValueError::new_err("Only user inputs can be mirrored"));
                }
                if !capability(py, channel.bind(py), "MirroringChannel")? {
                    return Err(PyTypeError::new_err(
                        "This channel does not support mirroring",
                    ));
                }
                Ok(())
            })?;
            let origin = Python::attach(|py| -> PyResult<_> {
                Ok(origin.into_pyobject(py)?.unbind().into_any())
            })?;
            let response = call(
                channel,
                "mirror",
                vec![source, message],
                vec![("origin", origin)],
            )
            .await?;
            let state = Python::attach(|py| slf.borrow(py).state(py))?;
            call(state, "append", vec![copy(&response)], vec![]).await?;
            Ok(response)
        })
    }
    /// Show an answer in `source`'s conversation while it is written.
    #[pyo3(signature=(channel,source,*,interval=None))]
    fn stream(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        interval: Option<f64>,
    ) -> PyResult<Object> {
        let stream =
            crate::stream::AnswerStream::create(py, slf.into_any(), channel, source, interval)?;
        Ok(Py::new(py, stream)?.into_any())
    }
    #[pyo3(signature=(channel,source,status="is working…",*,refresh_interval=60.0))]
    fn working(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        source: Object,
        status: &str,
        refresh_interval: f64,
    ) -> PyResult<Object> {
        let setter = py.import("functools")?.getattr("partial")?.call1((
            slf.bind(py).getattr("set_status")?,
            channel,
            source,
        ))?;
        let kw = PyDict::new(py);
        kw.set_item("refresh_interval", refresh_interval)?;
        Ok(py
            .get_type::<crate::activity::WorkingStatus>()
            .call((setter, status), Some(&kw))?
            .unbind())
    }
    fn _receive(
        slf: Py<Self>,
        py: Python<'_>,
        channel: Object,
        message: Object,
    ) -> PyResult<Object> {
        let app = slf.into_any();
        coroutine(py, receive(app, channel, message))
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.channels)?;
        visit.call(&self.state)?;
        visit.call(&self.handler)?;
        visit.call(&self.running)?;
        visit.call(&self.prompts)
    }
    fn __clear__(&mut self) {
        self.channels = None;
        self.state = None;
        self.handler = None;
        self.running = None;
        self.prompts = None;
    }
}
fn send_reply(
    py: Python<'_>,
    app: Object,
    channel: Object,
    source: Object,
    content: Object,
    rich: bool,
) -> PyResult<Object> {
    coroutine(py, async move {
        if rich {
            Python::attach(|py| -> PyResult<()> {
                if !capability(py, channel.bind(py), "RichReplyChannel")? {
                    return Err(PyTypeError::new_err(
                        "This channel does not support rich replies",
                    ));
                }
                Ok(())
            })?;
        }
        let response = call(
            channel,
            if rich { "reply_rich" } else { "reply" },
            vec![source, content],
            vec![],
        )
        .await?;
        let state = Python::attach(|py| app.bind(py).getattr("state").map(Bound::unbind))?;
        call(state, "append", vec![copy(&response)], vec![]).await?;
        Ok(response)
    })
}
fn cancelled(error: &PyErr) -> bool {
    Python::attach(|py| {
        py.import("asyncio")
            .and_then(|m| m.getattr("CancelledError"))
            .is_ok_and(|c| error.matches(py, &c).unwrap_or(false))
    })
}
fn native_app(app: &Object) -> PyResult<(Busy, Running, Prompts)> {
    Python::attach(|py| {
        let native = app.bind(py).cast::<AgentChat>()?.borrow();
        Ok((native.busy, native.running(py)?, native.prompts(py)?))
    })
}
enum Arrival {
    Run,
    Steered,
    Rejected,
    Asking(Py<RunningTurn>),
}
async fn receive(app: Object, channel: Object, message: Object) -> PyResult<Object> {
    let (state, handler, id, conversation, press, stop) = Python::attach(|py| -> PyResult<_> {
        let press = prompts::answer_of(message.bind(py))?;
        let stop = prompts::stop_request(message.bind(py))?;
        let handler = app.bind(py).getattr("_handler")?;
        if handler.is_none()
            && press.is_none()
            && !stop
            && !crate::forms::is_open(app.bind(py), message.bind(py))?
        {
            return Err(PyRuntimeError::new_err(
                "Register a message handler with @app.on_message",
            ));
        }
        Ok((
            app.bind(py).getattr("state")?.unbind(),
            handler.unbind(),
            message.bind(py).getattr("id")?.unbind(),
            message.bind(py).getattr("conversation_id")?.unbind(),
            press,
            stop,
        ))
    })?;
    let (busy, running, registry) = native_app(&app)?;
    let claimed = call(copy(&state), "claim", vec![copy(&id)], vec![]).await?;
    if !Python::attach(|py| claimed.bind(py).is_truthy())? {
        return Ok(none());
    }
    // A form waiting for this person takes their messages and its buttons.
    if crate::forms::intercept(&app, &channel, &message, press.as_ref(), stop).await? {
        return Ok(none());
    }
    // A button press answers a prompt; it never starts or joins a turn.
    if let Some((prompt, choice)) = press {
        prompts::resolve(registry, copy(&state), message, prompt, choice).await?;
        return Ok(none());
    }
    // So does a provider's own stop control, which stops only its starter's turn.
    if stop {
        Python::attach(|py| -> PyResult<()> {
            let sender = message
                .bind(py)
                .getattr("sender")?
                .getattr("id")?
                .extract::<String>()?;
            if let Some(turn) = running.get(py, &conversation)? {
                let turn = turn.borrow(py);
                if turn.starter == sender {
                    turn.request_stop(py)?;
                }
            }
            Ok(())
        })?;
        return Ok(none());
    }
    // Decide before awaiting anything, so the running turn cannot finish in between.
    let arrival = Python::attach(|py| -> PyResult<Arrival> {
        let Some(turn) = running.get(py, &conversation)? else {
            return Ok(Arrival::Run);
        };
        let asks = busy == Busy::Ask
            && channel.bind(py).is_instance(
                &py.import("agentchat.channels.base")?
                    .getattr("InteractiveChannel")?,
            )?;
        match busy {
            Busy::Queue => Ok(Arrival::Run),
            Busy::Ask if asks => Ok(Arrival::Asking(turn)),
            Busy::Steer | Busy::Ask => {
                turn.borrow(py).steer(py, message.bind(py))?;
                Ok(Arrival::Steered)
            }
            Busy::Reject => Ok(Arrival::Rejected),
        }
    })?;
    match arrival {
        Arrival::Steered => {
            call(copy(&state), "append", vec![copy(&message)], vec![]).await?;
            return Ok(none());
        }
        Arrival::Rejected => return Ok(none()),
        Arrival::Asking(turn) => {
            prompts::offer_busy(registry, channel, message, turn).await?;
            return Ok(none());
        }
        Arrival::Run => {}
    }
    let lock = Python::attach(|py| {
        state
            .bind(py)
            .call_method1("lock", (conversation.bind(py),))
            .map(Bound::unbind)
    })?;
    call(copy(&lock), "__aenter__", vec![], vec![]).await?;
    let result = run_turns(
        &app,
        &channel,
        message,
        &state,
        &handler,
        &conversation,
        &running,
        &registry,
    )
    .await;
    if exit_scope(lock, result.as_ref().err()).await? {
        return Ok(none());
    }
    result
}
/// Run `message`, then any steered messages its handler left unread, one turn each.
#[allow(clippy::too_many_arguments)]
async fn run_turns(
    app: &Object,
    channel: &Object,
    message: Object,
    state: &Object,
    handler: &Object,
    conversation: &Object,
    running: &Running,
    registry: &Prompts,
) -> PyResult<Object> {
    let mut current = message;
    let mut waiting = Vec::new();
    let mut appended = false;
    let mut first_error = None;
    loop {
        let turn = Python::attach(|py| -> PyResult<_> {
            let starter = current
                .bind(py)
                .getattr("sender")?
                .getattr("id")?
                .extract::<String>()?;
            running.start(py, conversation, std::mem::take(&mut waiting), starter)
        })?;
        let result = run_turn(app, channel, &current, state, handler, &turn, appended).await;
        let (mut unread, owned) = Python::attach(|py| -> PyResult<_> {
            let unread = running.finish(py, conversation, &turn)?;
            Ok((unread, prompts::take_owned(py, registry, &turn)?))
        })?;
        unread.extend(prompts::close_owned(state, owned).await?);
        if let Err(error) = result {
            if cancelled(&error) {
                return Err(error);
            }
            first_error.get_or_insert(error);
        }
        let mut unread = unread.into_iter();
        let Some(next) = unread.next() else { break };
        current = next;
        waiting = unread.collect();
        appended = true;
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(none()),
    }
}
async fn run_turn(
    app: &Object,
    channel: &Object,
    message: &Object,
    state: &Object,
    handler: &Object,
    running: &Py<RunningTurn>,
    appended: bool,
) -> PyResult<()> {
    let mut turn = Turn::new();
    // The message was claimed before the lock was taken.
    turn.resolve(TurnResult::Claimed(true))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    loop {
        let result = match turn.effect() {
            TurnEffect::Claim => TurnResult::Claimed(true),
            TurnEffect::AcquireConversation => TurnResult::ConversationAcquired,
            TurnEffect::AppendIncoming => {
                if !appended {
                    call(copy(state), "append", vec![copy(message)], vec![]).await?;
                }
                TurnResult::IncomingAppended
            }
            TurnEffect::InvokeHandler => {
                let pending = Python::attach(|py| -> PyResult<_> {
                    let mut context =
                        MessageContext::create(py, copy(app), copy(channel), copy(message))?;
                    context.turn = Some(running.clone_ref(py));
                    let context = Py::new(py, context)?;
                    Ok(handler.bind(py).call1((context,))?.unbind())
                })?;
                let answer = await_py(pending).await?;
                TurnResult::HandlerCompleted(Python::attach(|py| {
                    answer.bind(py).extract::<Option<String>>()
                })?)
            }
            TurnEffect::SendReply(text) => {
                let text = Python::attach(|py| {
                    text.clone()
                        .into_pyobject(py)
                        .map(|v| v.unbind().into_any())
                })?;
                call(
                    copy(app),
                    "reply",
                    vec![copy(channel), copy(message), text],
                    vec![],
                )
                .await?;
                TurnResult::ReplySent
            }
            TurnEffect::AppendReply => TurnResult::ReplyAppended,
            TurnEffect::Complete => return Ok(()),
        };
        turn.resolve(result)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    }
}
#[pyclass(dict, subclass, weakref, module = "agentchat._native")]
pub struct Conversation {
    inner: Option<Box<ConversationState>>,
}
pub struct ConversationState {
    id: Object,
    app: Object,
}
impl std::ops::Deref for Conversation {
    type Target = ConversationState;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_deref()
            .expect("native object has been cleared")
    }
}
impl std::ops::DerefMut for Conversation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("native object has been cleared")
    }
}

#[pymethods]
impl Conversation {
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self { inner: None }
    }
    fn initialize(&mut self, app: Object, conversation_id: Object) -> PyResult<()> {
        *self = Self::build(app, conversation_id);
        Ok(())
    }
    #[pyo3(signature=(*,limit=None))]
    fn history(&self, py: Python<'_>, limit: Option<Object>) -> PyResult<Object> {
        let app = self.app.clone_ref(py);
        let id = self.id.clone_ref(py);
        coroutine(py, async move {
            let state = Python::attach(|py| app.bind(py).getattr("state").map(Bound::unbind))?;
            call(
                state,
                "history",
                vec![id],
                vec![("limit", limit.unwrap_or_else(none))],
            )
            .await
        })
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        let Some(state) = &self.inner else {
            return Ok(());
        };

        visit.call(&state.id)?;
        visit.call(&state.app)
    }

    fn __clear__(&mut self) {
        self.inner = None;
    }

    #[getter(id)]
    fn get_id(&self, py: Python<'_>) -> Object {
        self.id.clone_ref(py)
    }
    #[setter(id)]
    fn set_id(&mut self, value: Object) {
        self.id = value;
    }
}
#[pyclass(dict, subclass, weakref, module = "agentchat._native")]
pub struct MessageContext {
    inner: Option<Box<MessageContextState>>,
    turn: Option<Py<RunningTurn>>,
}
pub struct MessageContextState {
    message: Object,
    sender: Object,
    conversation: Object,
    app: Object,
    channel: Object,
}
impl std::ops::Deref for MessageContext {
    type Target = MessageContextState;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_deref()
            .expect("native object has been cleared")
    }
}
impl std::ops::DerefMut for MessageContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("native object has been cleared")
    }
}

impl MessageContext {
    fn create(py: Python<'_>, app: Object, channel: Object, message: Object) -> PyResult<Self> {
        Ok(Self {
            turn: None,
            inner: Some(Box::new(MessageContextState {
                sender: message.bind(py).getattr("sender")?.unbind(),
                conversation: Py::new(
                    py,
                    Conversation::build(
                        copy(&app),
                        message.bind(py).getattr("conversation_id")?.unbind(),
                    ),
                )?
                .into_any(),
                message,
                app,
                channel,
            })),
        })
    }
    fn forward(
        &self,
        py: Python<'_>,
        method: &str,
        tail: Vec<Object>,
        kwargs: Vec<(&str, Object)>,
    ) -> PyResult<Object> {
        let mut args = vec![copy(&self.channel), copy(&self.message)];
        args.extend(tail);
        let kw = PyDict::new(py);
        for (key, value) in kwargs {
            kw.set_item(key, value)?;
        }
        Ok(self
            .app
            .bind(py)
            .call_method(method, PyTuple::new(py, args)?, Some(&kw))?
            .unbind())
    }
}
#[pymethods]
impl MessageContext {
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self {
            inner: None,
            turn: None,
        }
    }
    fn initialize(
        &mut self,
        py: Python<'_>,
        app: Object,
        channel: Object,
        message: Object,
    ) -> PyResult<()> {
        *self = Self::build(py, app, channel, message)?;
        Ok(())
    }
    #[pyo3(signature=(*,limit=30))]
    fn history(&self, py: Python<'_>, limit: isize) -> PyResult<Object> {
        let channel = copy(&self.channel);
        let message = copy(&self.message);
        let conversation = self.conversation.clone_ref(py);
        coroutine(py, async move {
            let bound = HistoryLimit::new(usize::try_from(limit).unwrap_or_default())
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let native = Python::attach(|py| -> PyResult<_> {
                Ok(!message.bind(py).getattr("thread_id")?.is_none()
                    && capability(py, channel.bind(py), "ThreadHistoryChannel")?)
            })?;
            let limit_value =
                Python::attach(|py| limit.into_pyobject(py).map(|v| v.unbind().into_any()))?;
            if native {
                let history = call(
                    channel,
                    "thread_history",
                    vec![copy(&message)],
                    vec![("limit", limit_value)],
                )
                .await?;
                Python::attach(|py| {
                    let mut messages: Vec<_> = history
                        .bind(py)
                        .try_iter()?
                        .map(|v| v.map(Bound::unbind))
                        .collect::<PyResult<_>>()?;
                    messages.push(message);
                    Ok(PyTuple::new(
                        py,
                        bound
                            .indices(messages.len())
                            .into_iter()
                            .map(|index| messages[index].bind(py)),
                    )?
                    .unbind()
                    .into_any())
                })
            } else {
                call(
                    conversation.into_any(),
                    "history",
                    vec![],
                    vec![("limit", limit_value)],
                )
                .await
            }
        })
    }
    fn reply(&self, py: Python<'_>, content: Object) -> PyResult<Object> {
        self.forward(py, "reply", vec![content], vec![])
    }
    fn reply_rich(&self, py: Python<'_>, content: Object) -> PyResult<Object> {
        self.forward(py, "reply_rich", vec![content], vec![])
    }
    fn upload_files(&self, py: Python<'_>, files: Object) -> PyResult<Object> {
        self.forward(py, "upload_files", vec![files], vec![])
    }
    fn set_status(&self, py: Python<'_>, status: Object) -> PyResult<Object> {
        self.forward(py, "set_status", vec![status], vec![])
    }
    /// Post a notice in this conversation that only the sender (or `user_id`) can
    /// see; see `AgentChat.send_ephemeral`.
    #[pyo3(signature=(text,*,blocks=None,user_id=None,fallback=None))]
    fn reply_ephemeral(
        &self,
        py: Python<'_>,
        text: Object,
        blocks: Option<Object>,
        user_id: Option<Object>,
        fallback: Option<Object>,
    ) -> PyResult<Object> {
        let user_id = match user_id {
            Some(user_id) => user_id,
            None => self.sender.bind(py).getattr("id")?.unbind(),
        };
        self.forward(
            py,
            "send_ephemeral",
            vec![user_id, text],
            vec![
                ("blocks", blocks.unwrap_or_else(|| py.None())),
                ("fallback", fallback.unwrap_or_else(|| py.None())),
            ],
        )
    }
    /// Ask a form of this message's sender; see `AgentChat.open_form`.
    fn open_form(&self, py: Python<'_>, form: Object) -> PyResult<Object> {
        self.forward(py, "open_form", vec![form], vec![])
    }
    #[pyo3(signature=(attachment,*,max_bytes=10*1024*1024))]
    fn download_attachment(
        &self,
        py: Python<'_>,
        attachment: Object,
        max_bytes: usize,
    ) -> PyResult<Object> {
        self.forward(
            py,
            "download_attachment",
            vec![attachment],
            vec![(
                "max_bytes",
                max_bytes.into_pyobject(py)?.unbind().into_any(),
            )],
        )
    }
    #[pyo3(signature=(status="is working…",*,refresh_interval=60.0))]
    fn working(&self, py: Python<'_>, status: &str, refresh_interval: f64) -> PyResult<Object> {
        self.forward(
            py,
            "working",
            vec![status.into_pyobject(py)?.unbind().into_any()],
            vec![(
                "refresh_interval",
                refresh_interval.into_pyobject(py)?.unbind().into_any(),
            )],
        )
    }
    /// Ask the sender (by default) to press a button; see `AgentChat.ask`.
    #[pyo3(signature=(text,choices,*,timeout=600.0,allowed=None))]
    fn ask(
        &self,
        py: Python<'_>,
        text: Object,
        choices: Object,
        timeout: f64,
        allowed: Option<Object>,
    ) -> PyResult<Object> {
        self.forward(
            py,
            "ask",
            vec![text, choices],
            vec![
                ("timeout", timeout.into_pyobject(py)?.into_any().unbind()),
                ("allowed", allowed.unwrap_or_else(|| py.None())),
            ],
        )
    }
    /// Show a Stop button for this turn; see `AgentChat.offer_stop`.
    #[pyo3(signature=(*,text="Working on it…".to_owned(),label="Stop".to_owned(),allowed=None))]
    fn offer_stop(
        &self,
        py: Python<'_>,
        text: String,
        label: String,
        allowed: Option<Object>,
    ) -> PyResult<Object> {
        self.forward(
            py,
            "offer_stop",
            vec![],
            vec![
                ("text", text.into_pyobject(py)?.into_any().unbind()),
                ("label", label.into_pyobject(py)?.into_any().unbind()),
                ("allowed", allowed.unwrap_or_else(|| py.None())),
            ],
        )
    }
    /// Show the answer while it is written; see `AgentChat.stream`.
    #[pyo3(signature=(*,interval=None))]
    fn stream(&self, py: Python<'_>, interval: Option<f64>) -> PyResult<Object> {
        let interval = match interval {
            Some(v) => v.into_pyobject(py)?.into_any().unbind(),
            None => py.None(),
        };
        self.forward(py, "stream", vec![], vec![("interval", interval)])
    }
    /// True once someone asked this turn to stop, for example with `app.stop()`.
    #[getter]
    fn stopped(&self, py: Python<'_>) -> PyResult<bool> {
        match &self.turn {
            Some(turn) => turn.borrow(py).stopped(py),
            None => Ok(false),
        }
    }
    /// Wait until someone asks this turn to stop.
    fn wait_stopped(&self, py: Python<'_>) -> PyResult<Object> {
        match &self.turn {
            Some(turn) => turn.borrow(py).wait_stopped(py),
            None => Err(PyRuntimeError::new_err(
                "Only a context from AgentChat can be stopped",
            )),
        }
    }
    /// Messages sent to this conversation since the handler started or last asked.
    /// Each is returned once; any left unread run as their own turns afterwards.
    fn steered(&self, py: Python<'_>) -> PyResult<Object> {
        let messages = match &self.turn {
            Some(turn) => turn.borrow(py).take(py)?,
            None => Vec::new(),
        };
        messages_tuple(py, messages)
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.turn)?;
        let Some(state) = &self.inner else {
            return Ok(());
        };

        visit.call(&state.message)?;
        visit.call(&state.sender)?;
        visit.call(&state.conversation)?;
        visit.call(&state.app)?;
        visit.call(&state.channel)
    }

    fn __clear__(&mut self) {
        self.inner = None;
        self.turn = None;
    }

    #[getter(message)]
    fn get_message(&self, py: Python<'_>) -> Object {
        self.message.clone_ref(py)
    }
    #[setter(message)]
    fn set_message(&mut self, value: Object) {
        self.message = value;
    }

    #[getter(sender)]
    fn get_sender(&self, py: Python<'_>) -> Object {
        self.sender.clone_ref(py)
    }
    #[setter(sender)]
    fn set_sender(&mut self, value: Object) {
        self.sender = value;
    }

    #[getter(conversation)]
    fn get_conversation(&self, py: Python<'_>) -> Object {
        self.conversation.clone_ref(py)
    }
    #[setter(conversation)]
    fn set_conversation(&mut self, value: Object) {
        self.conversation = value;
    }
}
#[pyfunction]
#[pyo3(signature=(backend=None))]
fn resolve_backend(backend: Option<&str>) -> PyResult<&'static str> {
    crate::state::backend(backend)
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AgentChat>()?;
    crate::host::register_initialized_class::<MessageContext>(module)?;
    crate::host::register_initialized_class::<Conversation>(module)?;
    let py = module.py();
    let attrs = PyDict::new(py);
    attrs.set_item(
        "__init__",
        py.get_type::<AgentChat>().getattr("initialize")?,
    )?;
    attrs.set_item("__module__", "agentchat._native")?;
    let class = py.import("builtins")?.getattr("type")?.call1((
        "AgentChat",
        (py.get_type::<AgentChat>(),),
        attrs,
    ))?;
    module.add("AgentChat", class)?;
    module.add_function(wrap_pyfunction!(resolve_backend, module)?)?;
    Ok(())
}

impl Conversation {
    #[allow(clippy::too_many_arguments)]
    fn build(app: Object, conversation_id: Object) -> Self {
        Self {
            inner: Some(Box::new(ConversationState {
                id: conversation_id,
                app,
            })),
        }
    }
}

impl MessageContext {
    #[allow(clippy::too_many_arguments)]
    fn build(py: Python<'_>, app: Object, channel: Object, message: Object) -> PyResult<Self> {
        Self::create(py, app, channel, message)
    }
}
