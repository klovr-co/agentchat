//! Teams SDK authentication and continuation I/O driven from Rust.
use crate::prompts::PromptFields;
use crate::{
    apps, convert, host,
    runtime::{call, copy, exit_scope, none},
};
use agentchat_core::TaskBoard;
use agentchat_providers::teams as policy;
use pyo3::{
    exceptions::{
        PyAttributeError, PyModuleNotFoundError, PyRuntimeError, PyTypeError, PyValueError,
    },
    prelude::*,
    types::{PyDict, PyList, PyTuple, PyType},
};
use serde_json::json;
type Object = Py<PyAny>;
type Output = PyResult<Object>;
fn sdk(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import("microsoft_agents.hosting.aiohttp").map_err(|e| {
        if e.is_instance_of::<PyModuleNotFoundError>(py) {
            PyModuleNotFoundError::new_err(
                "Install Teams support with: pip install 'agentchat[teams]'",
            )
        } else {
            e
        }
    })
}
#[pyfunction]
pub fn load_teams(py: Python<'_>) -> Output {
    sdk(py)?;
    Ok(py.get_type::<Teams>().into_any().unbind())
}
#[pyfunction]
fn teams_getattr(py: Python<'_>, name: &str) -> Output {
    if name == "Teams" {
        Ok(py
            .import("agentchat.channels.teams.adapter")?
            .getattr("Teams")?
            .unbind())
    } else {
        Err(PyAttributeError::new_err(format!(
            "module 'agentchat.channels.teams' has no attribute {name:?}"
        )))
    }
}
#[pyfunction]
fn teams_dir() -> Vec<&'static str> {
    vec!["Teams"]
}
#[pyfunction]
#[pyo3(signature=(event,*,app_id,tenant_id))]
fn teams_normalize(
    py: Python<'_>,
    event: &Bound<'_, PyAny>,
    app_id: &str,
    tenant_id: &str,
) -> Output {
    match policy::normalize(&convert::to_json(event)?, app_id, tenant_id) {
        Some(message) => convert::to_message(py, &message),
        None => Ok(py.None()),
    }
}
#[pyfunction]
fn teams_text_payload(content: &Bound<'_, PyAny>) -> PyResult<String> {
    let value = content
        .extract::<String>()
        .map_err(|_| PyValueError::new_err("Teams replies require nonempty text"))?;
    policy::text_payload(&value).map_err(PyValueError::new_err)?;
    Ok(value)
}
fn http_error(py: Python<'_>, name: &str, text: Option<&str>) -> PyErr {
    let result = || -> PyResult<PyErr> {
        let kw = PyDict::new(py);
        if let Some(text) = text {
            kw.set_item("text", text)?;
        }
        Ok(PyErr::from_value(
            py.import("aiohttp.web")?
                .getattr(name)?
                .call((), Some(&kw))?,
        ))
    };
    result().unwrap_or_else(|e| e)
}
#[pyclass(
    name = "_TeamsBase",
    dict,
    weakref,
    subclass,
    module = "agentchat._native"
)]
pub struct Teams {
    // A tracked Python object may be visited while CPython is allocating its
    // dictionary, before PyO3 installs the Rust payload. The zeroed state is valid.
    inner: Option<Box<TeamsState>>,
}
pub struct TeamsState {
    _app_id: String,
    _tenant_id: String,
    _host: String,
    _port: u16,
    _auth: Object,
    _hosts: Object,
    _adapter: Object,
    _runner: Object,
    _delivery: Object,
    _stopped: Object,
    _close_lock: Object,
    _closed: bool,
}
impl std::ops::Deref for Teams {
    type Target = TeamsState;
    fn deref(&self) -> &Self::Target {
        self.inner.as_deref().expect("initialized native provider")
    }
}
impl std::ops::DerefMut for Teams {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("initialized native provider")
    }
}

impl Teams {
    fn validate_url(&self, py: Python<'_>, url: &str) -> PyResult<()> {
        policy::validate_service_url(url, true).map_err(PyValueError::new_err)?;
        let allowed = self
            ._hosts
            .bind(py)
            .call_method1("is_allowed", (url,))?
            .is_truthy()?;
        policy::validate_service_url(url, allowed).map_err(PyValueError::new_err)
    }
}
#[pymethods]
impl Teams {
    #[getter(_app_id)]
    fn get_app_id(&self) -> PyResult<String> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._app_id.clone())
    }
    #[setter(_app_id)]
    fn set_app_id(&mut self, value: String) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._app_id = value;
        Ok(())
    }
    #[getter(_tenant_id)]
    fn get_tenant_id(&self) -> PyResult<String> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._tenant_id.clone())
    }
    #[setter(_tenant_id)]
    fn set_tenant_id(&mut self, value: String) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._tenant_id = value;
        Ok(())
    }
    #[getter(_host)]
    fn get_host(&self) -> PyResult<String> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._host.clone())
    }
    #[setter(_host)]
    fn set_host(&mut self, value: String) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._host = value;
        Ok(())
    }
    #[getter(_port)]
    fn get_port(&self) -> PyResult<u16> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._port)
    }
    #[setter(_port)]
    fn set_port(&mut self, value: u16) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._port = value;
        Ok(())
    }
    #[getter(_auth)]
    fn get_auth(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._auth.clone_ref(py))
    }
    #[setter(_auth)]
    fn set_auth(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._auth = value;
        Ok(())
    }
    #[getter(_hosts)]
    fn get_hosts(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._hosts.clone_ref(py))
    }
    #[setter(_hosts)]
    fn set_hosts(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._hosts = value;
        Ok(())
    }
    #[getter(_adapter)]
    fn get_adapter(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._adapter.clone_ref(py))
    }
    #[setter(_adapter)]
    fn set_adapter(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._adapter = value;
        Ok(())
    }
    #[getter(_runner)]
    fn get_runner(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._runner.clone_ref(py))
    }
    #[setter(_runner)]
    fn set_runner(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._runner = value;
        Ok(())
    }
    #[getter(_delivery)]
    fn get_delivery(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._delivery.clone_ref(py))
    }
    #[setter(_delivery)]
    fn set_delivery(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._delivery = value;
        Ok(())
    }
    #[getter(_stopped)]
    fn get_stopped(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._stopped.clone_ref(py))
    }
    #[setter(_stopped)]
    fn set_stopped(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._stopped = value;
        Ok(())
    }
    #[getter(_close_lock)]
    fn get_close_lock(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._close_lock.clone_ref(py))
    }
    #[setter(_close_lock)]
    fn set_close_lock(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._close_lock = value;
        Ok(())
    }
    #[getter(_closed)]
    fn get_closed(&self) -> PyResult<bool> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._closed)
    }
    #[setter(_closed)]
    fn set_closed(&mut self, value: bool) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._closed = value;
        Ok(())
    }
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        if let Some(state) = self.inner.as_ref() {
            visit.call(&state._auth)?;
            visit.call(&state._hosts)?;
            visit.call(&state._adapter)?;
            visit.call(&state._runner)?;
            visit.call(&state._delivery)?;
            visit.call(&state._stopped)?;
            visit.call(&state._close_lock)?;
        }
        Ok(())
    }
    fn __clear__(&mut self) {
        self.inner = None;
    }
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self { inner: None }
    }
    #[pyo3(signature=(*,app_id,app_password,tenant_id,host="127.0.0.1",port=3978,adapter=None,max_pending_events=64,shutdown_timeout=20.0))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        app_id: String,
        app_password: String,
        tenant_id: String,
        host: &str,
        port: u16,
        adapter: Option<Object>,
        max_pending_events: usize,
        shutdown_timeout: f64,
    ) -> PyResult<()> {
        if [&app_id, &app_password, &tenant_id]
            .iter()
            .any(|v| agentchat_providers::wire::strip(v).is_empty())
        {
            return Err(PyValueError::new_err(
                "Teams app_id, app_password, and tenant_id are required",
            ));
        }
        let module = sdk(py)?;
        let core = py.import("microsoft_agents.hosting.core")?;
        let args = PyDict::new(py);
        args.set_item("client_id", &app_id)?;
        args.set_item("client_secret", app_password)?;
        args.set_item("tenant_id", &tenant_id)?;
        args.set_item("anonymous_allowed", false)?;
        args.set_item("validate_issuer", true)?;
        let auth = core
            .getattr("AgentAuthConfiguration")?
            .call((), Some(&args))?
            .unbind();
        let args = PyDict::new(py);
        args.set_item("enabled", true)?;
        let hosts = core
            .getattr("OutboundHostValidator")?
            .call((), Some(&args))?
            .unbind();
        let adapter = match adapter {
            Some(v) => v,
            None => {
                let connections = PyDict::new(py);
                connections.set_item("SERVICE_CONNECTION", auth.bind(py))?;
                let args = PyDict::new(py);
                args.set_item("connections_configurations", connections)?;
                let manager = py
                    .import("microsoft_agents.authentication.msal")?
                    .getattr("MsalConnectionManager")?
                    .call((), Some(&args))?;
                let args = PyDict::new(py);
                args.set_item("connection_manager", manager)?;
                args.set_item("host_validator", hosts.bind(py))?;
                module
                    .getattr("CloudAdapter")?
                    .call((), Some(&args))?
                    .unbind()
            }
        };
        let asyncio = py.import("asyncio")?;
        slf.borrow_mut(py).inner = Some(Box::new(TeamsState {
            _app_id: app_id,
            _tenant_id: tenant_id,
            _host: host.into(),
            _port: port,
            _auth: auth,
            _hosts: hosts,
            _adapter: adapter,
            _runner: py.None(),
            _delivery: py
                .import("agentchat._delivery")?
                .getattr("DeliveryTasks")?
                .call1((max_pending_events, shutdown_timeout))?
                .unbind(),
            _stopped: asyncio.getattr("Event")?.call0()?.unbind(),
            _close_lock: asyncio.getattr("Lock")?.call0()?.unbind(),
            _closed: false,
        }));
        slf.bind(py).setattr(
            "_typing",
            super::typing::keeper(py, "agentchat.channels.teams.adapter", 3.0, 120.0)?,
        )?;
        let callback = host::callback(py, slf.bind(py).getattr("_turn_error")?.unbind())?;
        slf.borrow(py)
            ._adapter
            .bind(py)
            .setattr("on_turn_error", callback)?;
        Ok(())
    }
    #[classattr]
    fn name() -> &'static str {
        "teams"
    }
    #[classmethod]
    fn from_env(cls: &Bound<'_, PyType>) -> Output {
        let py = cls.py();
        let env = py.import("os")?;
        let kw = PyDict::new(py);
        for (key, arg) in [
            ("TEAMS_APP_ID", "app_id"),
            ("TEAMS_APP_PASSWORD", "app_password"),
            ("TEAMS_TENANT_ID", "tenant_id"),
        ] {
            let value = env.call_method1("getenv", (key,))?;
            if !value.is_truthy()? {
                return Err(PyRuntimeError::new_err(
                    "TEAMS_APP_ID, TEAMS_APP_PASSWORD, TEAMS_TENANT_ID are required",
                ));
            }
            kw.set_item(arg, value)?;
        }
        kw.set_item(
            "host",
            env.call_method1("getenv", ("TEAMS_HOST", "127.0.0.1"))?,
        )?;
        kw.set_item(
            "port",
            py.import("builtins")?
                .getattr("int")?
                .call1((env.call_method1("getenv", ("TEAMS_PORT", "3978"))?,))?,
        )?;
        Ok(cls.call((), Some(&kw))?.unbind())
    }
    fn bind(&self, py: Python<'_>, receiver: Object) -> PyResult<()> {
        self._delivery.bind(py).setattr("receiver", receiver)
    }
    fn create_app(slf: Py<Self>, py: Python<'_>) -> Output {
        let args = PyDict::new(py);
        args.set_item(
            "middlewares",
            PyList::new(py, [sdk(py)?.getattr("jwt_authorization_middleware")?])?,
        )?;
        let app = py
            .import("aiohttp.web")?
            .getattr("Application")?
            .call((), Some(&args))?;
        app.set_item("agent_configuration", slf.borrow(py)._auth.bind(py))?;
        let callback = host::callback(py, slf.bind(py).getattr("_handle_request")?.unbind())?;
        app.getattr("router")?
            .call_method1("add_post", ("/api/messages", callback))?;
        Ok(app.unbind())
    }
    fn run(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let runner = Python::attach(|py| {
                slf.borrow(py)
                    ._delivery
                    .bind(py)
                    .call_method0("check_bound")?;
                let app = slf.bind(py).call_method0("create_app")?;
                let kw = PyDict::new(py);
                kw.set_item("access_log", py.None())?;
                let runner = py
                    .import("aiohttp.web")?
                    .getattr("AppRunner")?
                    .call((app,), Some(&kw))?
                    .unbind();
                slf.borrow_mut(py)._runner = runner.clone_ref(py);
                Ok::<_, PyErr>(runner)
            })?;
            let result = async {
                call(copy(&runner), "setup", vec![], vec![]).await?;
                let site = Python::attach(|py| {
                    let this = slf.borrow(py);
                    Ok::<_, PyErr>(
                        py.import("aiohttp.web")?
                            .getattr("TCPSite")?
                            .call1((runner, &this._host, this._port))?
                            .unbind(),
                    )
                })?;
                call(site, "start", vec![], vec![]).await?;
                let stopped = Python::attach(|py| slf.borrow(py)._stopped.clone_ref(py));
                call(stopped, "wait", vec![], vec![]).await?;
                Ok(none())
            }
            .await;
            call(slf.into_any(), "close", vec![], vec![]).await?;
            result
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let lock = Python::attach(|py| slf.borrow(py)._close_lock.clone_ref(py));
            call(copy(&lock), "__aenter__", vec![], vec![]).await?;
            let result = async {
                let objects = Python::attach(|py| {
                    let mut this = slf.borrow_mut(py);
                    if this._closed {
                        return Ok(None);
                    }
                    this._closed = true;
                    this._delivery.bind(py).setattr("closed", true)?;
                    this._stopped.bind(py).call_method0("set")?;
                    Ok::<_, PyErr>(Some((
                        this._runner.clone_ref(py),
                        this._delivery.clone_ref(py),
                    )))
                })?;
                if let Some((runner, delivery)) = objects {
                    let typing =
                        Python::attach(|py| slf.bind(py).getattr("_typing").map(Bound::unbind))?;
                    call(typing, "close", vec![], vec![]).await?;
                    let result = if Python::attach(|py| runner.is_none(py)) {
                        Ok(none())
                    } else {
                        call(runner, "cleanup", vec![], vec![]).await
                    };
                    call(delivery, "close", vec![], vec![]).await?;
                    result?;
                }
                Ok::<_, PyErr>(())
            }
            .await;
            exit_scope(lock, result.as_ref().err()).await?;
            result?;
            Ok(none())
        })
    }
    fn _handle_request(slf: Py<Self>, py: Python<'_>, request: Object) -> Output {
        host::coroutine(py, async move {
            Python::attach(|py| {
                if slf
                    .borrow(py)
                    ._delivery
                    .bind(py)
                    .getattr("closed")?
                    .is_truthy()?
                {
                    Err(http_error(py, "HTTPServiceUnavailable", None))
                } else {
                    Ok(())
                }
            })?;
            let result = async {
                let event = call(copy(&request), "json", vec![], vec![]).await?;
                let adapter = Python::attach(|py| {
                    let value = convert::to_json(event.bind(py))?;
                    let url = value
                        .get("serviceUrl")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| PyValueError::new_err("Missing Teams service URL"))?;
                    let this = slf.borrow(py);
                    this.validate_url(py, url)?;
                    Ok::<_, PyErr>(this._adapter.clone_ref(py))
                })?;
                call(adapter, "process", vec![request, slf.into_any()], vec![]).await
            }
            .await;
            result.map_err(|e| {
                Python::attach(|py| {
                    let validation = py
                        .import("pydantic")
                        .and_then(|m| m.getattr("ValidationError"))
                        .ok()
                        .is_some_and(|t| e.is_instance(py, &t));
                    if validation
                        || e.is_instance_of::<PyValueError>(py)
                        || e.is_instance_of::<PyTypeError>(py)
                    {
                        http_error(py, "HTTPBadRequest", Some("Invalid Teams activity"))
                    } else {
                        e
                    }
                })
            })
        })
    }
    #[staticmethod]
    fn _turn_error(py: Python<'_>, _context: Object, error: Object) -> Output {
        host::coroutine(py, async move {
            Err(Python::attach(|py| PyErr::from_value(error.into_bound(py))))
        })
    }
    fn on_turn(slf: Py<Self>, py: Python<'_>, context: Object) -> Output {
        host::coroutine(py, async move {
            let (value, handler) = Python::attach(|py| -> PyResult<_> {
                let kw = PyDict::new(py);
                kw.set_item("by_alias", true)?;
                kw.set_item("exclude_none", true)?;
                let event = context.bind(py).getattr("activity")?.call_method(
                    "model_dump",
                    (),
                    Some(&kw),
                )?;
                let value = convert::to_json(&event)?;
                let this = slf.borrow(py);
                let mut handler = None;
                if policy::in_tenant(&value, &this._app_id, &this._tenant_id) {
                    if let Some(route) = policy::route(&value) {
                        handler = apps::find(slf.bind(py).as_any(), &route)?;
                        if handler.is_some() {
                            this.validate_url(py, value["serviceUrl"].as_str().unwrap_or(""))?;
                        }
                    }
                }
                Ok((value, handler))
            })?;
            if let Some(handler) = handler {
                return Self::handle_app(&slf, context, value, handler).await;
            }
            Python::attach(|py| {
                let this = slf.borrow(py);
                let Some(message) = policy::normalize(&value, &this._app_id, &this._tenant_id)
                else {
                    return Ok(py.None());
                };
                this.validate_url(py, value["serviceUrl"].as_str().unwrap_or(""))?;
                let personal =
                    policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
                        .is_ok_and(|d| policy::streams_natively(&d));
                let sender = message.sender.id.clone();
                let message = convert::to_message(py, &message)?;
                // The one-to-one chat to publish this person's home message in.
                if personal {
                    apps::memory(slf.bind(py).as_any(), "_personal")?.set_item(sender, &message)?;
                }
                if !this
                    ._delivery
                    .bind(py)
                    .call_method1("submit", (slf.clone_ref(py), message))?
                    .is_truthy()?
                {
                    return Err(http_error(
                        py,
                        "HTTPServiceUnavailable",
                        Some("Teams receiver is busy"),
                    ));
                }
                Ok(py.None())
            })
        })
    }
    /// Handle activities AgentChat does not turn into messages, by activity type,
    /// such as "conversationUpdate" or "installationUpdate".
    fn on_event(slf: &Bound<'_, Self>, event_type: Object) -> Output {
        apps::decorator(slf.as_any(), "event", event_type)
    }
    /// Handle card actions: an `adaptiveCard/action` verb, a dialog-opening
    /// `task/fetch` or an `Action.Submit` whose data has this `action_id`. For an
    /// invoke, the return value is the invoke response body, such as a dialog.
    fn on_action(slf: &Bound<'_, Self>, action_id: Object) -> Output {
        apps::decorator(slf.as_any(), "action", action_id)
    }
    /// Handle dialog submissions (`task/submit`) whose data has this `callback_id`;
    /// the return value is the invoke response body.
    fn on_view(slf: &Bound<'_, Self>, callback_id: Object) -> Output {
        apps::decorator(slf.as_any(), "view", callback_id)
    }
    /// Handle messages starting with this command, such as "/deploy", instead of
    /// passing them to the message handler.
    fn on_command(slf: &Bound<'_, Self>, command: Object) -> Output {
        apps::decorator(slf.as_any(), "command", command)
    }
    /// Handle every other routed activity, such as other invokes.
    fn on_raw(slf: &Bound<'_, Self>, handler: Object) -> Output {
        apps::raw(slf.as_any(), handler)
    }
    fn _validate_service_url(&self, py: Python<'_>, url: &str) -> PyResult<()> {
        self.validate_url(py, url)
    }
    fn reply(slf: Py<Self>, py: Python<'_>, source: Object, content: Object) -> Output {
        host::coroutine(py, async move {
            let content = Python::attach(|py| teams_text_payload(content.bind(py)))?;
            let (message, destination, receipt) =
                send_activity(&slf, &source, text_activity(&content), false).await?;
            Python::attach(|py| {
                let receipt = receipt.bind(py).extract::<String>().map_err(|_| {
                    PyRuntimeError::new_err("Teams did not return a message receipt")
                })?;
                super::transport::receipt_with_metadata(
                    &source,
                    &destination
                        .confirmed_reply(&message, content, &receipt)
                        .map_err(PyRuntimeError::new_err)?,
                )
            })
        })
    }
    /// Show a prompt as an Adaptive Card with one button per choice.
    fn send_prompt(slf: Py<Self>, py: Python<'_>, source: Object, prompt: Object) -> Output {
        host::coroutine(py, async move {
            let fields = Python::attach(|py| PromptFields::read(prompt.bind(py)))?;
            let card = policy::prompt_card(&fields.text, &fields.id, &fields.choices)
                .map_err(PyValueError::new_err)?;
            let activity = json!({"type": "message", "attachments": [card]});
            let (message, destination, receipt) =
                send_activity(&slf, &source, activity, false).await?;
            let receipt = receipt_id(&receipt)?;
            super::transport::receipt_with_metadata(
                &source,
                &destination
                    .confirmed_reply(&message, fields.text, &receipt)
                    .map_err(PyRuntimeError::new_err)?,
            )
        })
    }
    /// Replace the card with text: the choice and who made it, or just the question.
    fn close_prompt(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        sent: Object,
        prompt: Object,
        answer: Object,
    ) -> Output {
        host::coroutine(py, async move {
            let activity = Python::attach(|py| -> PyResult<_> {
                let fields = PromptFields::read(prompt.bind(py))?;
                let text = fields.closed_text(answer.bind(py))?.unwrap_or(fields.text);
                let id = convert::from_message(sent.bind(py))?.metadata["activity_id"].clone();
                let mut activity = text_activity(&text);
                activity["id"] = id;
                activity["attachments"] = json!([]);
                Ok(activity)
            })?;
            send_activity(&slf, &source, activity, true).await?;
            Ok(none())
        })
    }
    /// Teams accepts about one streamed update per second and suggests buffering longer.
    #[classattr]
    fn stream_interval() -> f64 {
        policy::STREAM_INTERVAL
    }
    #[classattr]
    fn stream_limit() -> usize {
        policy::STREAM_LIMIT
    }
    /// Start a streamed answer: a native Teams stream in one-to-one chats, otherwise
    /// a message that later parts update.
    fn start_stream(slf: Py<Self>, py: Python<'_>, source: Object, text: String) -> Output {
        host::coroutine(py, async move {
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let native = Python::attach(|py| {
                let this = slf.borrow(py);
                policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
                    .map(|d| policy::streams_natively(&d))
                    .map_err(PyValueError::new_err)
            })?;
            let activity = policy::stream_start_activity(&text, native);
            let (_, _, receipt) = send_activity(&slf, &source, activity, false).await?;
            let id = receipt_id(&receipt)?;
            Python::attach(|py| convert::from_json(py, &policy::stream_handle(&id, native)))
        })
    }
    fn append_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        stream: Object,
        text: String,
    ) -> Output {
        host::coroutine(py, async move {
            let mut handle = Python::attach(|py| convert::to_json(stream.bind(py)))?;
            if handle["stopped"] == true {
                return Ok(stream);
            }
            if handle.get("id").is_none() {
                // Only task progress has been shown so far; the answer starts now.
                if text.is_empty() {
                    return Ok(stream);
                }
                let started = Python::attach(|py| {
                    slf.bind(py)
                        .call_method1("start_stream", (source.bind(py), &text))
                        .map(Bound::unbind)
                })?;
                let started = host::await_py(started).await?;
                return Python::attach(|py| {
                    let mut started = convert::to_json(started.bind(py))?;
                    started["progress"] = handle["progress"].take();
                    started["board"] = handle["board"].take();
                    started["shown"] = json!(text);
                    convert::from_json(py, &started)
                });
            }
            let (activity, update, mut next) = policy::stream_update_activity(&handle, &text);
            match send_activity(&slf, &source, activity, update).await {
                Ok(_) => {
                    next["shown"] = json!(text);
                    next["text_started"] = json!(true);
                    Python::attach(|py| convert::from_json(py, &next))
                }
                Err(error) if handle["native"] == true && canceled(&error) => {
                    // The person pressed Teams' Stop button: stop the turn, keep what was shown.
                    let stop = Python::attach(|py| -> PyResult<Object> {
                        let message = convert::from_message(source.bind(py))?;
                        let id = handle["id"].as_str().unwrap_or_default();
                        convert::to_message(py, &policy::stop_request(&message, id))
                    })?;
                    Python::attach(|py| {
                        slf.borrow(py)
                            ._delivery
                            .bind(py)
                            .call_method1("submit", (slf.clone_ref(py), stop))
                            .map(|_| ())
                    })?;
                    handle["stopped"] = json!(true);
                    Python::attach(|py| convert::from_json(py, &handle))
                }
                Err(error) => Err(error),
            }
        })
    }
    fn stop_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        stream: Object,
        text: String,
    ) -> Output {
        host::coroutine(py, async move {
            let handle = Python::attach(|py| convert::to_json(stream.bind(py)))?;
            let result = finish_stream(&slf, &source, &handle, text).await;
            remove_progress(&slf, &source, &handle).await;
            result
        })
    }
    /// Show a notice only `user_id` can see: a targeted message in a channel or group
    /// chat ("Only you can see this message", gone after 24 hours), or an ordinary
    /// reply in that user's one-to-one chat. `blocks` are card attachments.
    #[pyo3(signature=(source,user_id,text,*,blocks=None))]
    fn send_ephemeral(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        user_id: String,
        text: String,
        blocks: Option<Object>,
    ) -> Output {
        let attachments = blocks
            .map(|blocks| convert::to_json(blocks.bind(py)))
            .transpose()?;
        let message = convert::from_message(source.bind(py))?;
        policy::text_payload(&text).map_err(PyValueError::new_err)?;
        let personal = {
            let this = slf.borrow(py);
            let destination =
                policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
                    .map_err(PyValueError::new_err)?;
            policy::streams_natively(&destination)
        };
        if personal && message.sender.id != user_id {
            return Err(PyValueError::new_err(
                "A one-to-one Teams chat can only show notices to its own user",
            ));
        }
        host::coroutine(py, async move {
            let mut activity = if personal {
                text_activity(&text)
            } else {
                policy::targeted_activity(&text, &user_id, None)
            };
            if let Some(attachments) =
                attachments.filter(|a| a.as_array().is_some_and(|a| !a.is_empty()))
            {
                activity["attachments"] = attachments;
            }
            let (message, destination, receipt) =
                send_activity(&slf, &source, activity, false).await?;
            let mut sent = destination
                .confirmed_reply(&message, text, &receipt_id(&receipt)?)
                .map_err(PyRuntimeError::new_err)?;
            if !personal {
                sent.metadata.insert("ephemeral".into(), json!(true));
                sent.metadata
                    .insert("recipient_user_id".into(), json!(user_id));
            }
            super::transport::receipt_with_metadata(&source, &sent)
        })
    }
    /// Keep one home message in the one-to-one chat with `user_id`, updated in place
    /// by later calls. A bot can only start that chat with details Teams sent
    /// earlier, so this works once the person has messaged the app one-to-one
    /// since the adapter started; otherwise it raises LookupError.
    fn publish_home(slf: Py<Self>, py: Python<'_>, user_id: String, view: Object) -> Output {
        let text = teams_text_payload(view.bind(py))?;
        let source = apps::memory(slf.bind(py).as_any(), "_personal")?
            .get_item(&user_id)?
            .ok_or_else(|| {
                pyo3::exceptions::PyLookupError::new_err(
                    "Teams can publish a home message only after that person has messaged \
                     the app one-to-one",
                )
            })?
            .unbind();
        let known = apps::memory(slf.bind(py).as_any(), "_homes")?
            .get_item(&user_id)?
            .map(|id| id.extract::<String>())
            .transpose()?;
        host::coroutine(py, async move {
            let mut activity = policy::text_activity(&text);
            activity["textFormat"] = json!("markdown");
            if let Some(id) = &known {
                activity["id"] = json!(id);
            }
            let sent = send_activity(&slf, &source, activity, known.is_some()).await;
            let (message, destination, receipt) = match sent {
                Ok(sent) => sent,
                Err(error) => {
                    // The next call sends a new home message.
                    Python::attach(|py| {
                        apps::memory(slf.bind(py).as_any(), "_homes")?.del_item(&user_id)
                    })
                    .ok();
                    return Err(error);
                }
            };
            let id = match known {
                Some(id) => id,
                None => receipt_id(&receipt)?,
            };
            Python::attach(|py| {
                apps::memory(slf.bind(py).as_any(), "_homes")?.set_item(&user_id, &id)
            })?;
            let home = destination
                .confirmed_reply(&message, text, &id)
                .map_err(PyRuntimeError::new_err)?;
            super::transport::receipt_with_metadata(&source, &home)
        })
    }
    /// Replace the text of a message this app sent.
    fn edit(slf: Py<Self>, py: Python<'_>, message: Object, text: String) -> Output {
        let text = teams_text_payload(text.into_pyobject(py)?.as_any())?;
        let id = convert::from_message(message.bind(py))?
            .metadata
            .get("activity_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| PyValueError::new_err("Expected a Teams message to edit"))?;
        host::coroutine(py, async move {
            let mut activity = policy::text_activity(&text);
            activity["id"] = json!(id);
            let (sent, destination, _) =
                send_activity(&slf, &message, activity, Send::Update).await?;
            let edited = destination
                .confirmed_reply(&sent, text, &id)
                .map_err(PyRuntimeError::new_err)?;
            super::transport::receipt_with_metadata(&message, &edited)
        })
    }
    /// Delete a message this app sent.
    fn delete(slf: Py<Self>, py: Python<'_>, message: Object) -> Output {
        let id = convert::from_message(message.bind(py))?
            .metadata
            .get("activity_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| PyValueError::new_err("Expected a Teams message to delete"))?;
        host::coroutine(py, async move {
            send_activity(
                &slf,
                &message,
                json!({"type": "message", "id": id}),
                Send::Delete,
            )
            .await?;
            Ok(none())
        })
    }
    /// Show task progress: in a one-to-one chat as Teams' informative stream updates
    /// before the answer starts; elsewhere as a separate message updated in place and
    /// deleted when the answer is finished. A failed update never fails the answer.
    fn update_stream_tasks(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        stream: Object,
        updates: Object,
    ) -> Output {
        host::coroutine(py, async move {
            let (updates, handle, native) = Python::attach(|py| -> PyResult<_> {
                let updates: Vec<agentchat_core::StreamUpdate> =
                    serde_json::from_value(convert::to_json(updates.bind(py))?)
                        .map_err(|e| PyValueError::new_err(e.to_string()))?;
                let handle = (!stream.is_none(py))
                    .then(|| convert::to_json(stream.bind(py)))
                    .transpose()?;
                let message = convert::from_message(source.bind(py))?;
                let this = slf.borrow(py);
                let native =
                    policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
                        .map(|d| policy::streams_natively(&d))
                        .map_err(PyValueError::new_err)?;
                Ok((updates, handle, native))
            })?;
            let mut board: TaskBoard = handle
                .as_ref()
                .and_then(|h| serde_json::from_value(h["board"].clone()).ok())
                .unwrap_or_default();
            board.apply(&updates);
            let mut next = handle.clone().unwrap_or_else(|| json!({}));
            next["board"] = serde_json::to_value(&board).unwrap_or_default();
            let result = if native {
                if next["text_started"] == true || next["stopped"] == true {
                    Ok(())
                } else {
                    let text = policy::informative_text(&board);
                    if text.is_empty() {
                        Ok(())
                    } else {
                        let (activity, informative) = policy::informative_activity(
                            handle.as_ref().filter(|h| h.get("id").is_some()),
                            &text,
                        );
                        match send_activity(&slf, &source, activity, false).await {
                            Ok((_, _, receipt)) => {
                                if next.get("id").is_none() {
                                    next = informative;
                                    next["id"] = json!(receipt_id(&receipt)?);
                                    next["board"] =
                                        serde_json::to_value(&board).unwrap_or_default();
                                } else {
                                    next["sequence"] = informative["sequence"].clone();
                                }
                                next["shown"] = json!(text);
                                Ok(())
                            }
                            Err(error) => Err(error),
                        }
                    }
                }
            } else {
                let text = board.render(4000);
                let progress = next["progress"].clone();
                if text.is_empty() || progress["shown"] == text.as_str() {
                    Ok(())
                } else {
                    let mut activity = text_activity(&text);
                    activity["textFormat"] = json!("markdown");
                    let update = progress["id"].is_string();
                    if update {
                        activity["id"] = progress["id"].clone();
                    }
                    match send_activity(&slf, &source, activity, update).await {
                        Ok((_, _, receipt)) => {
                            let id = if update {
                                progress["id"].clone()
                            } else {
                                json!(receipt_id(&receipt)?)
                            };
                            next["progress"] = json!({"id": id, "shown": text});
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
            };
            if let Err(error) = result {
                Python::attach(|py| -> PyResult<()> {
                    py.import("logging")?
                        .call_method1("getLogger", ("agentchat.channels.teams.adapter",))?
                        .call_method1(
                            "warning",
                            ("Teams task progress update failed (%s)", error.to_string()),
                        )?;
                    Ok(())
                })?;
            }
            Python::attach(|py| convert::from_json(py, &next))
        })
    }
    /// Show "typing…" in the source conversation until cleared with "" or the keeper
    /// expires. Teams shows no status text and clears the indicator after a few seconds.
    fn set_status(slf: Py<Self>, py: Python<'_>, source: Object, status: String) -> Output {
        host::coroutine(py, async move {
            let started = Python::attach(|py| -> PyResult<Option<Object>> {
                let message = convert::from_message(source.bind(py))?;
                let this = slf.borrow(py);
                let destination =
                    policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
                        .map_err(PyValueError::new_err)?;
                this.validate_url(py, &destination.service_url)?;
                this._delivery.bind(py).call_method0("check_reply")?;
                drop(this);
                let keeper = slf.bind(py).getattr("_typing")?;
                let key = destination.conversation_id;
                if status.is_empty() {
                    keeper.call_method1("stop", (key,))?;
                    return Ok(None);
                }
                let send = py
                    .import("functools")?
                    .getattr("partial")?
                    .call1((slf.bind(py).getattr("_send_typing")?, source.bind(py)))?;
                Ok(Some(keeper.call_method1("start", (key, send))?.unbind()))
            })?;
            match started {
                Some(pending) => host::await_py(pending).await,
                None => {
                    Python::attach(|py| Ok(true.into_pyobject(py)?.to_owned().into_any().unbind()))
                }
            }
        })
    }
    fn _send_typing(slf: Py<Self>, py: Python<'_>, source: Object) -> Output {
        host::coroutine(py, async move {
            send_activity(&slf, &source, json!({"type": "typing"}), false).await?;
            Ok(none())
        })
    }
}
/// Whether a failed stream update means the person pressed Teams' Stop button.
fn canceled(error: &PyErr) -> bool {
    Python::attach(|py| {
        let value = error.value(py);
        let status = value
            .getattr("status")
            .ok()
            .and_then(|status| status.extract::<u16>().ok());
        let body = value
            .getattr("message")
            .ok()
            .and_then(|message| message.extract::<String>().ok())
            .unwrap_or_default();
        status.is_some_and(|status| policy::stream_canceled(status, &body))
    })
}
/// Delete the task progress message once the answer is finished; best effort.
async fn remove_progress(slf: &Py<Teams>, source: &Object, handle: &serde_json::Value) {
    let Some(id) = handle["progress"]["id"].as_str() else {
        return;
    };
    let _ = send_activity(
        slf,
        source,
        json!({"type": "message", "id": id}),
        Send::Delete,
    )
    .await;
}
/// Finish a streamed answer and confirm it.
async fn finish_stream(
    slf: &Py<Teams>,
    source: &Object,
    handle: &serde_json::Value,
    text: String,
) -> Output {
    let confirm = |content: String, receipt: &str| {
        Python::attach(|py| -> Output {
            let message = convert::from_message(source.bind(py))?;
            let this = slf.borrow(py);
            let destination =
                policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
                    .map_err(PyValueError::new_err)?;
            let sent = destination
                .confirmed_reply(&message, content, receipt)
                .map_err(PyRuntimeError::new_err)?;
            super::transport::receipt_with_metadata(source, &sent)
        })
    };
    let id = handle["id"].as_str().map(str::to_owned);
    if handle["stopped"] == true {
        // Teams keeps what was streamed before Stop; nothing more can be sent.
        let shown = handle["shown"].as_str().unwrap_or_default().to_owned();
        return confirm(shown, id.as_deref().unwrap_or_default());
    }
    if id.is_none() {
        if let Some(progress) = handle["progress"]["id"]
            .as_str()
            .filter(|_| text.trim().is_empty())
        {
            // Only task progress was shown: confirm it without text.
            return confirm(String::new(), progress);
        }
        policy::text_payload(&text).map_err(PyValueError::new_err)?;
        let (message, destination, receipt) =
            send_activity(slf, source, text_activity(&text), false).await?;
        let sent = destination
            .confirmed_reply(&message, text, &receipt_id(&receipt)?)
            .map_err(PyRuntimeError::new_err)?;
        return Python::attach(|_| super::transport::receipt_with_metadata(source, &sent));
    }
    // A stream that only showed informative updates ends with that text.
    let shown_only = text.trim().is_empty() && handle["text_started"] != true;
    let content = if shown_only {
        handle["shown"].as_str().unwrap_or("…").to_owned()
    } else {
        policy::text_payload(&text).map_err(PyValueError::new_err)?;
        text.clone()
    };
    let (activity, update) = policy::stream_final_activity(handle, &content);
    let (message, destination, receipt) = send_activity(slf, source, activity, update).await?;
    let receipt = receipt_id(&receipt)
        .or_else(|_| id.clone().ok_or(()))
        .map_err(|_| PyRuntimeError::new_err("Teams did not return a message receipt"))?;
    let sent = destination
        .confirmed_reply(
            &message,
            if shown_only { String::new() } else { text },
            &receipt,
        )
        .map_err(PyRuntimeError::new_err)?;
    Python::attach(|_| super::transport::receipt_with_metadata(source, &sent))
}
impl Teams {
    /// Run the application's handler for an activity. An invoke gets the handler's
    /// answer as its invoke response, within the acknowledgement deadline.
    async fn handle_app(
        slf: &Py<Self>,
        context: Object,
        value: serde_json::Value,
        handler: Object,
    ) -> Output {
        let (payload, delivery) = Python::attach(|py| -> PyResult<_> {
            Ok((
                convert::from_json(py, &value)?,
                slf.borrow(py)._delivery.clone_ref(py),
            ))
        })?;
        if value["type"] != "invoke" {
            let spawned = Python::attach(|py| {
                apps::spawn(slf.bind(py).as_any(), delivery.bind(py), &handler, &payload)
            })?;
            if !spawned {
                return Err(Python::attach(|py| {
                    http_error(py, "HTTPServiceUnavailable", Some("Teams receiver is busy"))
                }));
            }
            return Ok(none());
        }
        let answer = apps::answer(delivery, handler, payload).await;
        let response = match &answer {
            Ok(body) => policy::invoke_response(
                &Python::attach(|py| convert::to_json(body.bind(py))).unwrap_or_default(),
            ),
            Err(_) => json!({"type": "invokeResponse", "value": {"status": 500}}),
        };
        let activity = Python::attach(|py| -> PyResult<Object> {
            Ok(py
                .import("microsoft_agents.activity")?
                .getattr("Activity")?
                .call_method1("model_validate", (convert::from_json(py, &response)?,))?
                .unbind())
        })?;
        call(context, "send_activity", vec![activity], vec![]).await?;
        if let Err(error) = answer {
            Python::attach(|py| -> PyResult<()> {
                py.import("logging")?
                    .call_method1("getLogger", ("agentchat.channels.teams.adapter",))?
                    .call_method1(
                        "warning",
                        ("Teams invoke handler failed (%s)", error.to_string()),
                    )?;
                Ok(())
            })?;
        }
        Ok(none())
    }
}
use policy::text_activity;
fn receipt_id(receipt: &Object) -> PyResult<String> {
    Python::attach(|py| receipt.bind(py).extract::<String>())
        .map_err(|_| PyRuntimeError::new_err("Teams did not return a message receipt"))
}
/// Send one activity to the conversation `source` came from; returns its receipt.
/// How an activity is sent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Send {
    New,
    Update,
    Delete,
}
impl From<bool> for Send {
    fn from(update: bool) -> Self {
        if update {
            Send::Update
        } else {
            Send::New
        }
    }
}
async fn send_activity(
    slf: &Py<Teams>,
    source: &Object,
    activity: serde_json::Value,
    update: impl Into<Send>,
) -> PyResult<(agentchat_core::Message, policy::Destination, Object)> {
    let update = update.into();
    let (message, destination, adapter, reference, callback, send) = Python::attach(|py| {
        let message = convert::from_message(source.bind(py))?;
        let this = slf.borrow(py);
        let dest = policy::Destination::from_source(&message, &this._app_id, &this._tenant_id)
            .map_err(PyValueError::new_err)?;
        let activity_class = py
            .import("microsoft_agents.activity")?
            .getattr("Activity")?;
        let reference = activity_class
            .call_method1("model_validate", (convert::from_json(py, &dest.activity)?,))?
            .unbind();
        this.validate_url(py, &dest.service_url)?;
        this._delivery.bind(py).call_method0("check_reply")?;
        let send = Py::new(
            py,
            TeamsSend {
                activity: activity_class
                    .call_method1("model_validate", (convert::from_json(py, &activity)?,))?
                    .unbind(),
                receipt: py.None(),
                update: update == Send::Update,
                delete: update == Send::Delete,
            },
        )?;
        let callback = host::callback(py, send.bind(py).getattr("send")?.unbind())?;
        Ok::<_, PyErr>((
            message,
            dest,
            this._adapter.clone_ref(py),
            reference,
            callback,
            send,
        ))
    })?;
    let app_id = Python::attach(|py| {
        destination
            .app_id
            .clone()
            .into_pyobject(py)
            .map(|v| v.into_any().unbind())
    })?;
    call(
        adapter,
        "continue_conversation",
        vec![app_id, reference, callback],
        vec![],
    )
    .await?;
    let receipt = Python::attach(|py| send.borrow(py).receipt.clone_ref(py));
    Ok((message, destination, receipt))
}
#[pyclass]
struct TeamsSend {
    activity: Object,
    receipt: Object,
    /// Replace the message with the activity's ID instead of sending a new one.
    update: bool,
    /// Delete the message with the activity's ID instead.
    delete: bool,
}
#[pymethods]
impl TeamsSend {
    fn send(slf: Py<Self>, py: Python<'_>, context: Object) -> Output {
        host::coroutine(py, async move {
            let (activity, method) = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                if this.delete {
                    return Ok((
                        this.activity.bind(py).getattr("id")?.unbind(),
                        "delete_activity",
                    ));
                }
                let method = if this.update {
                    "update_activity"
                } else {
                    "send_activity"
                };
                Ok((this.activity.clone_ref(py), method))
            })?;
            let receipt = call(context, method, vec![activity], vec![]).await?;
            Python::attach(|py| {
                slf.borrow_mut(py).receipt = if receipt.bind(py).is_truthy()? {
                    receipt.bind(py).getattr("id")?.unbind()
                } else {
                    py.None()
                };
                Ok(py.None())
            })
        })
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Teams>()?;
    m.add_function(wrap_pyfunction!(load_teams, m)?)?;
    m.add_function(wrap_pyfunction!(teams_getattr, m)?)?;
    m.add_function(wrap_pyfunction!(teams_dir, m)?)?;
    m.add_function(wrap_pyfunction!(teams_normalize, m)?)?;
    m.add_function(wrap_pyfunction!(teams_text_payload, m)?)?;
    Ok(())
}
