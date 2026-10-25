//! Slack SDK I/O adapter. Rust owns admission, acknowledgement, and shutdown.
use crate::{
    host::{await_py, coroutine},
    runtime::{call, copy, none},
};
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PySet, PyTuple},
    PyTraverseError, PyVisit,
};
type Object = Py<PyAny>;
const MODULE: &str = "agentchat.channels.slack.socket_mode";
#[pyclass(dict, subclass, weakref, module = "agentchat._native")]
pub struct SocketMode {
    inner: Option<Box<SocketModeState>>,
}
pub struct SocketModeState {
    app_token: String,
    web_client: Object,
    max_pending_events: usize,
    socket_client: Option<Object>,
    receiver: Option<Object>,
    closed: Object,
    tasks: Py<PySet>,
    connect_task: Option<Object>,
    close_task: Option<Object>,
    run_started: bool,
}
impl std::ops::Deref for SocketMode {
    type Target = SocketModeState;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_deref()
            .expect("native object has been cleared")
    }
}
impl std::ops::DerefMut for SocketMode {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("native object has been cleared")
    }
}

impl SocketMode {
    fn is_closed(&self, py: Python<'_>) -> PyResult<bool> {
        self.closed.bind(py).call_method0("is_set")?.is_truthy()
    }
}
#[pymethods]
impl SocketMode {
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self { inner: None }
    }
    #[pyo3(signature=(*,app_token,web_client=None,max_pending_events=64))]
    fn initialize(
        &mut self,
        py: Python<'_>,
        app_token: String,
        web_client: Option<Object>,
        max_pending_events: isize,
    ) -> PyResult<()> {
        *self = Self::build(py, app_token, web_client, max_pending_events)?;
        Ok(())
    }
    fn run(slf: Py<Self>, py: Python<'_>, receiver: Object) -> PyResult<Object> {
        coroutine(py, async move {
            Python::attach(|py| -> PyResult<()> {
                let mut this = slf.borrow_mut(py);
                if this.run_started || this.is_closed(py)? {
                    return Err(PyRuntimeError::new_err(
                        "Socket Mode has already been started or closed",
                    ));
                }
                this.run_started = true;
                this.receiver = Some(receiver);
                Ok(())
            })?;
            let result: PyResult<Object> = async {
                let task = Python::attach(|py| -> PyResult<_> {
                    let (token, web) = {
                        let this = slf.borrow(py);
                        (this.app_token.clone(), copy(&this.web_client))
                    };
                    let kwargs = PyDict::new(py);
                    kwargs.set_item("app_token", token)?;
                    kwargs.set_item("web_client", web)?;
                    let client = py
                        .import(MODULE)?
                        .getattr("SocketModeClient")?
                        .call((), Some(&kwargs))?;
                    client
                        .getattr("socket_mode_request_listeners")?
                        .call_method1(
                            "append",
                            (slf.bind(py).getattr("_handle_socket_request")?,),
                        )?;
                    let task = py
                        .import("asyncio")?
                        .call_method1("create_task", (client.call_method0("connect")?,))?
                        .unbind();
                    let mut this = slf.borrow_mut(py);
                    this.socket_client = Some(client.unbind());
                    this.connect_task = Some(copy(&task));
                    Ok(task)
                })?;
                if let Err(error) = await_py(task).await {
                    let stopped =
                        Python::attach(|py| -> PyResult<_> {
                            Ok(error
                                .is_instance(py, &py.import("asyncio")?.getattr("CancelledError")?)
                                && slf.borrow(py).is_closed(py)?)
                        })?;
                    if stopped {
                        return Ok(none());
                    }
                    return Err(error);
                }
                let closed = Python::attach(|py| copy(&slf.borrow(py).closed));
                call(closed, "wait", vec![], vec![]).await?;
                Ok(none())
            }
            .await;
            call(slf.into_any(), "close", vec![], vec![]).await?;
            result
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        coroutine(py, async move {
            let waiting = Python::attach(|py| -> PyResult<_> {
                let closed = copy(&slf.borrow(py).closed);
                closed.bind(py).call_method0("set")?;
                if slf.borrow(py).close_task.is_none() {
                    let task = py
                        .import("asyncio")?
                        .call_method1("create_task", (slf.bind(py).call_method0("_close")?,))?
                        .unbind();
                    slf.borrow_mut(py).close_task = Some(task);
                }
                let this = slf.borrow(py);
                // An admitted receiver may request close, but cannot await itself.
                if this
                    .tasks
                    .bind(py)
                    .contains(py.import("asyncio")?.call_method0("current_task")?)?
                {
                    return Ok(None);
                }
                Ok(Some(
                    py.import("asyncio")?
                        .call_method1(
                            "shield",
                            (this
                                .close_task
                                .as_ref()
                                .ok_or_else(|| {
                                    PyRuntimeError::new_err("Socket close did not start")
                                })?
                                .bind(py),),
                        )?
                        .unbind(),
                ))
            })?;
            if let Some(waiting) = waiting {
                await_py(waiting).await?;
            }
            Ok(none())
        })
    }
    fn _close(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        coroutine(py, async move {
            let connecting = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                if let Some(task) = this.connect_task.as_ref() {
                    if !task.bind(py).call_method0("done")?.is_truthy()? {
                        task.bind(py).call_method0("cancel")?;
                        let kw = PyDict::new(py);
                        kw.set_item("return_exceptions", true)?;
                        return Ok(Some(
                            py.import("asyncio")?
                                .call_method("gather", (task.bind(py),), Some(&kw))?
                                .unbind(),
                        ));
                    }
                }
                Ok(None)
            })?;
            if let Some(connecting) = connecting {
                await_py(connecting).await?;
            }
            let client = Python::attach(|py| slf.borrow(py).socket_client.as_ref().map(copy));
            let result = if let Some(client) = client {
                call(client, "close", vec![], vec![]).await
            } else {
                Ok(none())
            };
            let waiting = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                if this.tasks.bind(py).is_empty() {
                    return Ok(None);
                }
                let kwargs = PyDict::new(py);
                kwargs.set_item(
                    "timeout",
                    py.import(MODULE)?.getattr("EVENT_DRAIN_TIMEOUT")?,
                )?;
                Ok(Some(
                    py.import("asyncio")?
                        .call_method("wait", (this.tasks.bind(py),), Some(&kwargs))?
                        .unbind(),
                ))
            })?;
            if let Some(waiting) = waiting {
                let results = await_py(waiting).await?;
                let gather = Python::attach(|py| -> PyResult<_> {
                    let pending = results.bind(py).get_item(1)?;
                    let tasks = pending.try_iter()?.collect::<PyResult<Vec<_>>>()?;
                    for task in &tasks {
                        task.call_method0("cancel")?;
                    }
                    let kw = PyDict::new(py);
                    kw.set_item("return_exceptions", true)?;
                    Ok(py
                        .import("asyncio")?
                        .call_method("gather", PyTuple::new(py, tasks)?, Some(&kw))?
                        .unbind())
                })?;
                await_py(gather).await?;
            }
            result?;
            Ok(none())
        })
    }
    fn is_connected(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        coroutine(py, async move {
            let client = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                if this.is_closed(py)? {
                    Ok(None)
                } else {
                    Ok(this.socket_client.as_ref().map(copy))
                }
            })?;
            match client {
                Some(client) => call(client, "is_connected", vec![], vec![]).await,
                None => {
                    Python::attach(|py| Ok(false.into_pyobject(py)?.to_owned().unbind().into_any()))
                }
            }
        })
    }
    fn _handle_socket_request(
        slf: Py<Self>,
        py: Python<'_>,
        client: Object,
        request: Object,
    ) -> PyResult<Object> {
        coroutine(py, async move {
            let acknowledged = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                let kind = request.bind(py).getattr("type")?.extract::<String>()?;
                // Events, and interactions such as presses on prompt buttons.
                if !matches!(kind.as_str(), "events_api" | "interactive")
                    || this.is_closed(py)?
                    || this.tasks.bind(py).len() >= this.max_pending_events
                {
                    return Ok(None);
                }
                drop(this);
                let ack = py
                    .import("asyncio")?
                    .call_method0("get_running_loop")?
                    .call_method0("create_future")?;
                ack.call_method1("add_done_callback", (slf.bind(py).getattr("_ack_done")?,))?;
                let task = py.import("asyncio")?.call_method1(
                    "create_task",
                    (slf.bind(py)
                        .call_method1("_deliver_event", (client, request, &ack))?,),
                )?;
                slf.borrow(py).tasks.bind(py).add(&task)?;
                task.call_method1("add_done_callback", (slf.bind(py).getattr("_event_done")?,))?;
                Ok(Some(
                    py.import("asyncio")?
                        .call_method1("shield", (ack,))?
                        .unbind(),
                ))
            })?;
            if let Some(ack) = acknowledged {
                await_py(ack).await?;
            }
            Ok(none())
        })
    }
    fn _deliver_event(
        slf: Py<Self>,
        py: Python<'_>,
        client: Object,
        request: Object,
        acknowledged: Object,
    ) -> PyResult<Object> {
        coroutine(py, async move {
            let response = Python::attach(|py| -> PyResult<_> {
                let kw = PyDict::new(py);
                kw.set_item("envelope_id", request.bind(py).getattr("envelope_id")?)?;
                Ok(py
                    .import(MODULE)?
                    .getattr("SocketModeResponse")?
                    .call((), Some(&kw))?
                    .unbind())
            })?;
            if let Err(error) =
                call(client, "send_socket_mode_response", vec![response], vec![]).await
            {
                Python::attach(|py| {
                    acknowledged
                        .bind(py)
                        .call_method1("set_exception", (error.value(py),))
                        .map(|_| ())
                })?;
                return Err(error);
            }
            let pending = Python::attach(|py| -> PyResult<_> {
                acknowledged
                    .bind(py)
                    .call_method1("set_result", (py.None(),))?;
                let receiver = slf
                    .borrow(py)
                    .receiver
                    .as_ref()
                    .map(copy)
                    .ok_or_else(|| PyRuntimeError::new_err("Socket receiver is unbound"))?;
                Ok(receiver
                    .bind(py)
                    .call1((request.bind(py).getattr("payload")?,))?
                    .unbind())
            })?;
            await_py(pending).await?;
            Ok(none())
        })
    }
    fn _ack_done(&self, task: &Bound<'_, PyAny>) -> PyResult<()> {
        if !task.call_method0("cancelled")?.is_truthy()? {
            task.call_method0("exception")?;
        }
        Ok(())
    }
    fn _event_done(&self, py: Python<'_>, task: &Bound<'_, PyAny>) -> PyResult<()> {
        self.tasks.bind(py).discard(task)?;
        if !task.call_method0("cancelled")?.is_truthy()? {
            let error = task.call_method0("exception")?;
            if !error.is_none() {
                py.import("logging")?
                    .call_method1("getLogger", (MODULE,))?
                    .call_method1(
                        "warning",
                        (
                            "Slack handler failed (%s)",
                            error.get_type().getattr("__name__")?,
                        ),
                    )?;
            }
        }
        Ok(())
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        let Some(state) = &self.inner else {
            return Ok(());
        };

        visit.call(&state.web_client)?;
        visit.call(&state.socket_client)?;
        visit.call(&state.receiver)?;
        visit.call(&state.closed)?;
        visit.call(&state.tasks)?;
        visit.call(&state.connect_task)?;
        visit.call(&state.close_task)
    }
    fn __clear__(&mut self) {
        self.inner = None;
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::host::register_initialized_class::<SocketMode>(module)
}

impl SocketMode {
    #[allow(clippy::too_many_arguments)]
    fn build(
        py: Python<'_>,
        app_token: String,
        web_client: Option<Object>,
        max_pending_events: isize,
    ) -> PyResult<Self> {
        if app_token.is_empty() {
            return Err(PyValueError::new_err(
                "app_token is required for Socket Mode",
            ));
        }
        if max_pending_events < 1 {
            return Err(PyValueError::new_err("max_pending_events must be positive"));
        }
        Ok(Self {
            inner: Some(Box::new(SocketModeState {
                app_token,
                web_client: web_client.unwrap_or_else(none),
                max_pending_events: max_pending_events as usize,
                socket_client: None,
                receiver: None,
                closed: py.import("asyncio")?.getattr("Event")?.call0()?.unbind(),
                tasks: PySet::empty(py)?.unbind(),
                connect_task: None,
                close_task: None,
                run_started: false,
            })),
        })
    }
}
