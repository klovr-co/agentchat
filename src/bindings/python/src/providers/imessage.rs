//! Native iMessage lifecycle with foreign Socket.IO and HTTP I/O.
use crate::{
    apps, convert, host,
    runtime::{call, copy, exit_scope, none},
};
use agentchat_providers::{imessage as policy, wire::strip};
use pyo3::{
    exceptions::{
        PyAttributeError, PyException, PyModuleNotFoundError, PyRuntimeError, PyValueError,
    },
    prelude::*,
    types::{PyDict, PyFrozenSet, PyTuple, PyType},
};
type Object = Py<PyAny>;
type Output = PyResult<Object>;
fn sdk(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import("socketio").map_err(|e| {
        if e.is_instance_of::<PyModuleNotFoundError>(py) {
            PyModuleNotFoundError::new_err(
                "Install iMessage support with: pip install 'agentchat[imessage]'",
            )
        } else {
            e
        }
    })
}
#[pyfunction]
pub fn load_imessage(py: Python<'_>) -> Output {
    sdk(py)?;
    Ok(py.get_type::<IMessage>().into_any().unbind())
}
#[pyfunction]
fn imessage_getattr(py: Python<'_>, name: &str) -> Output {
    if name == "IMessage" {
        Ok(py
            .import("agentchat.channels.imessage.adapter")?
            .getattr("IMessage")?
            .unbind())
    } else {
        Err(PyAttributeError::new_err(format!(
            "module 'agentchat.channels.imessage' has no attribute {name:?}"
        )))
    }
}
#[pyfunction]
fn imessage_dir() -> Vec<&'static str> {
    vec!["IMessage"]
}
#[pyfunction]
#[pyo3(signature=(event,*,instance_id))]
fn imessage_normalize(py: Python<'_>, event: &Bound<'_, PyAny>, instance_id: &str) -> Output {
    match policy::normalize(&convert::to_json(event)?, instance_id) {
        Some(message) => convert::to_message(py, &message),
        None => Ok(py.None()),
    }
}
#[pyfunction]
fn imessage_text_payload(content: &Bound<'_, PyAny>) -> PyResult<String> {
    let value = content
        .extract::<String>()
        .map_err(|_| PyValueError::new_err("iMessage replies require nonempty text"))?;
    policy::text_payload(&value).map_err(PyValueError::new_err)?;
    Ok(value)
}
#[pyclass(
    name = "_IMessageBase",
    dict,
    weakref,
    subclass,
    module = "agentchat._native"
)]
pub struct IMessage {
    // A tracked Python object may be visited while CPython is allocating its
    // dictionary, before PyO3 installs the Rust payload. The zeroed state is valid.
    inner: Option<Box<IMessageState>>,
}
pub struct IMessageState {
    _client: Object,
    _socket: Object,
    _delivery: Object,
    _instance_id: String,
    _allowed_chats: Object,
    _close_lock: Object,
    _closed: bool,
    _typing: Object,
}
impl std::ops::Deref for IMessage {
    type Target = IMessageState;
    fn deref(&self) -> &Self::Target {
        self.inner.as_deref().expect("initialized native provider")
    }
}
impl std::ops::DerefMut for IMessage {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("initialized native provider")
    }
}

#[pymethods]
impl IMessage {
    #[getter(_client)]
    fn get_client(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._client.clone_ref(py))
    }
    #[setter(_client)]
    fn set_client(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._client = value;
        Ok(())
    }
    #[getter(_socket)]
    fn get_socket(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._socket.clone_ref(py))
    }
    #[setter(_socket)]
    fn set_socket(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._socket = value;
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
    #[getter(_instance_id)]
    fn get_instance_id(&self) -> PyResult<String> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._instance_id.clone())
    }
    #[setter(_instance_id)]
    fn set_instance_id(&mut self, value: String) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._instance_id = value;
        Ok(())
    }
    #[getter(_allowed_chats)]
    fn get_allowed_chats(&self, py: Python<'_>) -> PyResult<Object> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._allowed_chats.clone_ref(py))
    }
    #[setter(_allowed_chats)]
    fn set_allowed_chats(&mut self, value: Object) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._allowed_chats = value;
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
            visit.call(&state._client)?;
            visit.call(&state._socket)?;
            visit.call(&state._delivery)?;
            visit.call(&state._allowed_chats)?;
            visit.call(&state._close_lock)?;
            visit.call(&state._typing)?;
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
    #[pyo3(signature=(*,server_url,password,instance_id=None,allowed_chat_guids=None,client=None,socket_client=None,max_pending_events=64,shutdown_timeout=20.0,stop_keyword=None))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        server_url: String,
        password: String,
        instance_id: Option<String>,
        allowed_chat_guids: Option<Object>,
        client: Option<Object>,
        socket_client: Option<Object>,
        max_pending_events: usize,
        shutdown_timeout: f64,
        stop_keyword: Option<String>,
    ) -> PyResult<()> {
        if strip(&password).is_empty() {
            return Err(PyValueError::new_err("BlueBubbles password is required"));
        }
        let module = sdk(py)?;
        let client = match client {
            Some(client) => client,
            None => Py::new(py, BlueBubblesClient::build(py, &server_url, password)?)?.into_any(),
        };
        let instance = match instance_id.filter(|s| !s.is_empty()) {
            Some(v) => v,
            None => {
                let url = client.bind(py).getattr("server_url")?.extract::<String>()?;
                policy::Server { url }.instance_id()
            }
        };
        let allowed = match allowed_chat_guids {
            None => py.None(),
            Some(v) => {
                let values = v.bind(py).try_iter()?.collect::<PyResult<Vec<_>>>()?;
                PyFrozenSet::new(py, values)?.into_any().unbind()
            }
        };
        let socket = match socket_client {
            Some(v) => v,
            None => {
                let logging = py.import("logging")?;
                let args = PyDict::new(py);
                args.set_item("level", 51)?;
                let logger = logging
                    .getattr("Logger")?
                    .call(("agentchat.channels.imessage.adapter.socket",), Some(&args))?;
                logger.setattr("propagate", false)?;
                let kw = PyDict::new(py);
                kw.set_item("reconnection", true)?;
                kw.set_item("logger", &logger)?;
                kw.set_item("engineio_logger", logger)?;
                kw.set_item("handle_sigint", false)?;
                module.getattr("AsyncClient")?.call((), Some(&kw))?.unbind()
            }
        };
        slf.borrow_mut(py).inner = Some(Box::new(IMessageState {
            _client: client,
            _socket: socket,
            _delivery: py
                .import("agentchat._delivery")?
                .getattr("DeliveryTasks")?
                .call1((max_pending_events, shutdown_timeout))?
                .unbind(),
            _instance_id: instance,
            _allowed_chats: allowed,
            _close_lock: py.import("asyncio")?.getattr("Lock")?.call0()?.unbind(),
            _closed: false,
            _typing: super::typing::keeper(
                py,
                "agentchat.channels.imessage.adapter",
                policy::TYPING_INTERVAL,
                policy::TYPING_EXPIRY,
            )?,
        }));
        let callback = host::callback(py, slf.bind(py).getattr("_on_message")?.unbind())?;
        slf.borrow(py)
            ._socket
            .bind(py)
            .call_method1("on", ("new-message", callback))?;
        slf.bind(py).setattr("_stop_keyword", stop_keyword)?;
        Ok(())
    }
    #[classattr]
    fn name() -> &'static str {
        "imessage"
    }
    /// Handle a BlueBubbles socket event by name, such as "updated-message" or
    /// "group-name-change". The handler receives `{"event": name, "data": ...}`.
    fn on_event(slf: &Bound<'_, Self>, event_type: Object) -> Output {
        let py = slf.py();
        let decorator = apps::decorator(slf.as_any(), "event", event_type.clone_ref(py))?;
        let name = event_type.bind(py).extract::<String>()?;
        if name != "new-message" {
            let callback = py
                .import("functools")?
                .getattr("partial")?
                .call1((slf.getattr("_on_socket_event")?, &name))?;
            slf.borrow()
                ._socket
                .bind(py)
                .call_method1("on", (name, callback))?;
        }
        Ok(decorator)
    }
    /// iMessage has no buttons; registered action handlers never run.
    fn on_action(slf: &Bound<'_, Self>, action_id: Object) -> Output {
        apps::decorator(slf.as_any(), "action", action_id)
    }
    /// iMessage has no forms; registered view handlers never run.
    fn on_view(slf: &Bound<'_, Self>, callback_id: Object) -> Output {
        apps::decorator(slf.as_any(), "view", callback_id)
    }
    /// Handle messages starting with this command, such as "/start", instead of
    /// passing them to the message handler.
    fn on_command(slf: &Bound<'_, Self>, command: Object) -> Output {
        apps::decorator(slf.as_any(), "command", command)
    }
    /// Handle every other routed event.
    fn on_raw(slf: &Bound<'_, Self>, handler: Object) -> Output {
        apps::raw(slf.as_any(), handler)
    }
    fn _on_socket_event(slf: Py<Self>, py: Python<'_>, name: String, data: Object) -> Output {
        host::coroutine(py, async move {
            Python::attach(|py| {
                let channel = slf.bind(py);
                let route = agentchat_core::Route::Event {
                    event_type: name.clone(),
                };
                if let Some(handler) = apps::find(channel.as_any(), &route)? {
                    let payload = PyDict::new(py);
                    payload.set_item("event", &name)?;
                    payload.set_item("data", data.bind(py))?;
                    let this = slf.borrow(py);
                    apps::spawn(
                        channel.as_any(),
                        this._delivery.bind(py),
                        &handler,
                        &payload.into_any().unbind(),
                    )?;
                }
                Ok(py.None())
            })
        })
    }
    /// iMessage has no private notices; in a one-to-one chat with that person an
    /// ordinary reply is already private. Elsewhere this raises ValueError.
    #[pyo3(signature=(source,user_id,text,*,blocks=None))]
    fn send_ephemeral(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        user_id: String,
        text: String,
        blocks: Option<Object>,
    ) -> Output {
        if blocks.is_some_and(|b| !b.is_none(py)) {
            return Err(PyValueError::new_err("iMessage notices are text only"));
        }
        let message = convert::from_message(source.bind(py))?;
        let direct = message
            .metadata
            .get("chat_guid")
            .and_then(serde_json::Value::as_str)
            .is_some_and(policy::is_direct);
        if !direct || message.sender.id != user_id {
            return Err(PyValueError::new_err(
                "iMessage can only show a notice as a reply in a one-to-one chat with that person",
            ));
        }
        Ok(slf.bind(py).call_method1("reply", (source, text))?.unbind())
    }
    #[classmethod]
    fn from_env(cls: &Bound<'_, PyType>) -> Output {
        let py = cls.py();
        let env = py.import("os")?;
        let url = env.call_method1("getenv", ("BLUEBUBBLES_URL",))?;
        let password = env.call_method1("getenv", ("BLUEBUBBLES_PASSWORD",))?;
        if !url.is_truthy()? || !password.is_truthy()? {
            return Err(PyRuntimeError::new_err(
                "BLUEBUBBLES_URL and BLUEBUBBLES_PASSWORD are required",
            ));
        }
        let kw = PyDict::new(py);
        kw.set_item("server_url", url)?;
        kw.set_item("password", password)?;
        kw.set_item(
            "instance_id",
            env.call_method1("getenv", ("IMESSAGE_INSTANCE_ID",))?,
        )?;
        let chats = env.call_method1("getenv", ("IMESSAGE_ALLOWED_CHATS",))?;
        if chats.is_truthy()? {
            kw.set_item(
                "allowed_chat_guids",
                chats
                    .extract::<String>()?
                    .split(',')
                    .map(strip)
                    .collect::<Vec<_>>(),
            )?;
        }
        Ok(cls.call((), Some(&kw))?.unbind())
    }
    fn bind(&self, py: Python<'_>, receiver: Object) -> PyResult<()> {
        self._delivery.bind(py).setattr("receiver", receiver)
    }
    fn run(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            Python::attach(|py| {
                slf.borrow(py)
                    ._delivery
                    .bind(py)
                    .call_method0("check_bound")
                    .map(|_| ())
            })?;
            let result = async {
                let (client, socket) = Python::attach(|py| {
                    let this = slf.borrow(py);
                    (this._client.clone_ref(py), this._socket.clone_ref(py))
                });
                call(copy(&client), "check_connection", vec![], vec![]).await?;
                let connected = async {
                    let url = Python::attach(|py| {
                        client.bind(py).getattr("socket_url").map(Bound::unbind)
                    })?;
                    call(copy(&socket), "connect", vec![url], vec![]).await?;
                    call(socket, "wait", vec![], vec![]).await?;
                    Ok::<_, PyErr>(())
                }
                .await;
                connected.map_err(|e| {
                    Python::attach(|py| {
                        if e.is_instance_of::<PyException>(py) {
                            PyRuntimeError::new_err("BlueBubbles socket connection failed")
                        } else {
                            e
                        }
                    })
                })?;
                if !Python::attach(|py| slf.borrow(py)._closed) {
                    return Err(PyRuntimeError::new_err(
                        "BlueBubbles socket disconnected; check the server and password",
                    ));
                }
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
                    Ok::<_, PyErr>(Some([
                        this._socket.clone_ref(py),
                        this._delivery.clone_ref(py),
                        this._client.clone_ref(py),
                    ]))
                })?;
                let mut result = Ok(());
                if objects.is_some() {
                    let keeper = Python::attach(|py| slf.borrow(py)._typing.clone_ref(py));
                    result = call(keeper, "close", vec![], vec![]).await.map(|_| ());
                }
                if let Some(objects) = objects {
                    for (index, object) in objects.into_iter().enumerate() {
                        let next = call(
                            object,
                            if index == 0 { "shutdown" } else { "close" },
                            vec![],
                            vec![],
                        )
                        .await
                        .map(|_| ());
                        if next.is_err() {
                            result = next;
                        }
                    }
                }
                result
            }
            .await;
            exit_scope(lock, result.as_ref().err()).await?;
            result?;
            Ok(none())
        })
    }
    fn _on_message(slf: Py<Self>, py: Python<'_>, event: Object) -> Output {
        host::coroutine(py, async move {
            Python::attach(|py| {
                let this = slf.borrow(py);
                if this._delivery.bind(py).getattr("closed")?.is_truthy()? {
                    return Ok(py.None());
                }
                let Some(message) =
                    policy::normalize(&convert::to_json(event.bind(py))?, &this._instance_id)
                else {
                    return Ok(py.None());
                };
                if !this._allowed_chats.is_none(py)
                    && !this
                        ._allowed_chats
                        .bind(py)
                        .contains(message.metadata["chat_guid"].as_str().unwrap_or(""))?
                {
                    return Ok(py.None());
                }
                // A command the application handles never becomes a message.
                if let Some(command) = agentchat_core::command_of(&message.text) {
                    let route = agentchat_core::Route::Command { command };
                    if let Some(handler) = apps::find(slf.bind(py).as_any(), &route)? {
                        apps::spawn(
                            slf.bind(py).as_any(),
                            this._delivery.bind(py),
                            &handler,
                            &event,
                        )?;
                        return Ok(py.None());
                    }
                }
                // The stop keyword, when configured, stops the sender's running turn.
                let keyword = slf
                    .bind(py)
                    .getattr("_stop_keyword")?
                    .extract::<Option<String>>()?;
                let message = if policy::is_stop_keyword(&message.text, keyword.as_deref()) {
                    policy::stop_request(&message)
                } else {
                    message
                };
                let value = convert::to_message(py, &message)?;
                this._delivery
                    .bind(py)
                    .call_method1("submit", (slf.clone_ref(py), value))?;
                Ok(py.None())
            })
        })
    }
    fn reply(slf: Py<Self>, py: Python<'_>, source: Object, content: Object) -> Output {
        host::coroutine(py, async move {
            let source_host = Python::attach(|py| source.clone_ref(py));
            let (source, content, destination, client) = Python::attach(|py| {
                let content = imessage_text_payload(content.bind(py))?;
                let source = convert::from_message(source.bind(py))?;
                let this = slf.borrow(py);
                let dest = this.destination(py, &source)?;
                Ok::<_, PyErr>((source, content, dest, this._client.clone_ref(py)))
            })?;
            let args = Python::attach(|py| {
                Ok::<_, PyErr>(vec![
                    destination
                        .chat_guid
                        .clone()
                        .into_pyobject(py)?
                        .into_any()
                        .unbind(),
                    content.clone().into_pyobject(py)?.into_any().unbind(),
                ])
            })?;
            let receipt = call(client, "send_text", args, vec![]).await?;
            Python::attach(|py| {
                let id = receipt.bind(py).extract::<String>().map_err(|_| {
                    PyRuntimeError::new_err("BlueBubbles did not return a message receipt")
                })?;
                super::transport::receipt_with_metadata(
                    &source_host,
                    &destination
                        .confirmed_reply(&source, content, &id)
                        .map_err(PyRuntimeError::new_err)?,
                )
            })
        })
    }
    /// Show "typing…" in the source chat until cleared with "" or the keeper expires.
    /// iMessage shows no status text. Returns False when the BlueBubbles server
    /// cannot show typing (its Private API is off) or the client has no typing.
    fn set_status(slf: Py<Self>, py: Python<'_>, source: Object, status: String) -> Output {
        host::coroutine(py, async move {
            let (chat, client, keeper, socket) = Python::attach(|py| {
                let message = convert::from_message(source.bind(py))?;
                let this = slf.borrow(py);
                let destination = this.destination(py, &message)?;
                Ok::<_, PyErr>((
                    destination.chat_guid,
                    this._client.clone_ref(py),
                    this._typing.clone_ref(py),
                    this._socket.clone_ref(py),
                ))
            })?;
            let (key, typing) = Python::attach(|py| {
                Ok::<_, PyErr>((
                    chat.clone().into_pyobject(py)?.into_any().unbind(),
                    client.bind(py).hasattr("start_typing")?,
                ))
            })?;
            let answer = |value: bool| {
                Python::attach(|py| Ok(value.into_pyobject(py)?.to_owned().into_any().unbind()))
            };
            if !typing {
                return answer(status.is_empty());
            }
            if status.is_empty() {
                Python::attach(|py| {
                    keeper
                        .bind(py)
                        .call_method1("stop", (key.bind(py),))
                        .map(drop)
                })?;
                stop_typing(socket, &chat).await;
                return answer(true);
            }
            let first = call(copy(&client), "start_typing", vec![copy(&key)], vec![]).await?;
            if !Python::attach(|py| first.bind(py).is_truthy())? {
                Python::attach(|py| {
                    keeper
                        .bind(py)
                        .call_method1("stop", (key.bind(py),))
                        .map(drop)
                })?;
                return answer(false);
            }
            Python::attach(|py| {
                let send = py
                    .import("functools")?
                    .getattr("partial")?
                    .call1((client.bind(py).getattr("start_typing")?, key.bind(py)))?;
                keeper.bind(py).call_method1("keep", (key.bind(py), send))?;
                Ok::<_, PyErr>(())
            })?;
            answer(true)
        })
    }
}

impl IMessage {
    /// The chat a source message belongs to, if this adapter may reply there.
    fn destination(
        &self,
        py: Python<'_>,
        source: &agentchat_core::Message,
    ) -> PyResult<policy::Destination> {
        let allowed = if self._allowed_chats.is_none(py) {
            None
        } else {
            Some(
                self._allowed_chats
                    .bind(py)
                    .try_iter()?
                    .map(|v| v?.extract::<String>())
                    .collect::<PyResult<Vec<_>>>()?,
            )
        };
        let destination =
            policy::Destination::from_source(source, &self._instance_id, allowed.as_deref())
                .map_err(PyValueError::new_err)?;
        self._delivery.bind(py).call_method0("check_reply")?;
        Ok(destination)
    }
}

/// Clear "typing…" over the socket; the REST route for this restarts typing on
/// current servers. Best effort: the reply or the server's timeout also clears it.
async fn stop_typing(socket: Object, chat: &str) {
    let emitted = Python::attach(|py| -> PyResult<Option<Object>> {
        let socket = socket.bind(py);
        if !socket.hasattr("emit")?
            || !socket
                .getattr("connected")
                .and_then(|c| c.is_truthy())
                .unwrap_or(false)
        {
            return Ok(None);
        }
        let payload = convert::from_json(py, &policy::typing_event_payload(chat))?;
        Ok(Some(
            socket
                .call_method1("emit", (policy::STOP_TYPING_EVENT, payload))?
                .unbind(),
        ))
    });
    if let Ok(Some(pending)) = emitted {
        let _ = host::await_py(pending).await;
    }
}

#[pyclass(
    dict,
    weakref,
    subclass,
    module = "agentchat.channels.imessage.transport"
)]
pub struct BlueBubblesClient {
    inner: Option<Box<BlueBubblesClientState>>,
}
pub struct BlueBubblesClientState {
    server_url: String,
    _password: String,
    _session: Object,
    _closed: bool,
}
impl std::ops::Deref for BlueBubblesClient {
    type Target = BlueBubblesClientState;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_deref()
            .expect("native object has been cleared")
    }
}
impl std::ops::DerefMut for BlueBubblesClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("native object has been cleared")
    }
}
#[pymethods]
impl BlueBubblesClient {
    fn __traverse__(&self, visit: pyo3::PyVisit<'_>) -> Result<(), pyo3::PyTraverseError> {
        if let Some(state) = &self.inner {
            visit.call(&state._session)?;
        }
        Ok(())
    }
    fn __clear__(&mut self) {
        self.inner = None;
    }
    #[getter(_session)]
    fn session(&self, py: Python<'_>) -> Py<PyAny> {
        self._session.clone_ref(py)
    }
    #[setter(_session)]
    fn set_session(&mut self, value: Py<PyAny>) {
        self._session = value;
    }
    #[getter(_closed)]
    fn closed(&self) -> bool {
        self._closed
    }
    #[setter(_closed)]
    fn set_closed(&mut self, value: bool) {
        self._closed = value;
    }
    #[getter(server_url)]
    fn server_url(&self) -> &str {
        &self.server_url
    }
    #[setter(server_url)]
    fn set_server_url(&mut self, value: String) {
        self.server_url = value;
    }

    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self { inner: None }
    }
    fn initialize(&mut self, py: Python<'_>, server_url: &str, password: String) -> PyResult<()> {
        *self = Self::build(py, server_url, password)?;
        Ok(())
    }
    #[getter]
    fn socket_url(&self) -> String {
        policy::Server {
            url: self.server_url.clone(),
        }
        .socket_url(&self._password)
    }
    #[pyo3(signature=(method,path,**kwargs))]
    fn request(
        slf: Py<Self>,
        py: Python<'_>,
        method: String,
        path: String,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> Output {
        let kwargs = kwargs.map(|v| v.clone().unbind());
        host::coroutine(py, async move {
            let (status, data) = exchange(slf, method, path, kwargs).await?;
            Python::attach(|py| {
                if !(200..300).contains(&status) {
                    return Err(PyRuntimeError::new_err(format!(
                        "BlueBubbles request failed (HTTP {status})"
                    )));
                }
                let value = convert::to_json(data.bind(py))?;
                if !value.is_object()
                    || value.get("status").and_then(serde_json::Value::as_u64) != Some(200)
                {
                    return Err(PyRuntimeError::new_err(
                        "BlueBubbles returned an invalid response",
                    ));
                }
                Ok(data)
            })
        })
    }
    /// Show "typing…" in a chat. Returns False when the server cannot: its Private
    /// API is off or not connected.
    fn start_typing(slf: Py<Self>, py: Python<'_>, chat_guid: String) -> Output {
        host::coroutine(py, async move {
            let path = policy::typing_path(&chat_guid);
            let (status, data) = exchange(slf, "POST".into(), path, None).await?;
            Python::attach(|py| {
                let value = convert::to_json(data.bind(py))?;
                let shown = if (200..300).contains(&status) {
                    true
                } else if policy::typing_unavailable(status, &value) {
                    false
                } else {
                    return Err(PyRuntimeError::new_err(format!(
                        "BlueBubbles request failed (HTTP {status})"
                    )));
                };
                Ok(shown.into_pyobject(py)?.to_owned().into_any().unbind())
            })
        })
    }
    fn check_connection(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let args = Python::attach(|py| {
                vec![
                    "GET".into_pyobject(py).unwrap().into_any().unbind(),
                    "/api/v1/ping"
                        .into_pyobject(py)
                        .unwrap()
                        .into_any()
                        .unbind(),
                ]
            });
            call(slf.into_any(), "request", args, vec![]).await?;
            Ok(none())
        })
    }
    fn send_text(slf: Py<Self>, py: Python<'_>, chat_guid: String, content: String) -> Output {
        host::coroutine(py, async move {
            let (args, payload) = Python::attach(|py| {
                let temporary = py.import("uuid")?.call_method0("uuid4")?.str()?.to_string();
                let payload = convert::from_json(
                    py,
                    &policy::text_request(&chat_guid, &content, &temporary),
                )?;
                Ok::<_, PyErr>((
                    vec![
                        "POST".into_pyobject(py)?.into_any().unbind(),
                        "/api/v1/message/text"
                            .into_pyobject(py)?
                            .into_any()
                            .unbind(),
                    ],
                    payload,
                ))
            })?;
            let data = call(slf.into_any(), "request", args, vec![("json", payload)]).await?;
            Python::attach(|py| {
                let data = convert::to_json(data.bind(py))?;
                let id = policy::receipt(&data).map_err(PyRuntimeError::new_err)?;
                Ok(id.into_pyobject(py)?.into_any().unbind())
            })
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let session = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                this._closed = true;
                (!this._session.is_none(py)).then(|| this._session.clone_ref(py))
            });
            if let Some(session) = session {
                call(session, "close", vec![], vec![]).await?;
            }
            Ok(none())
        })
    }
}
/// One BlueBubbles request: its HTTP status and JSON body (None if not JSON).
async fn exchange(
    slf: Py<BlueBubblesClient>,
    method: String,
    path: String,
    kwargs: Option<Py<PyDict>>,
) -> PyResult<(u16, Object)> {
    let result = async {
        let context = Python::attach(|py| {
            let mut this = slf.borrow_mut(py);
            if this._closed {
                return Err(PyRuntimeError::new_err("BlueBubbles client is closed"));
            }
            if this._session.is_none(py) {
                let aio = py.import("aiohttp")?;
                let kw = PyDict::new(py);
                kw.set_item("total", 60)?;
                let timeout = aio.getattr("ClientTimeout")?.call((), Some(&kw))?;
                let kw = PyDict::new(py);
                kw.set_item("timeout", timeout)?;
                this._session = aio.getattr("ClientSession")?.call((), Some(&kw))?.unbind();
            }
            let kw = match kwargs {
                Some(v) => v.bind(py).copy()?,
                None => PyDict::new(py),
            };
            let params = PyDict::new(py);
            params.set_item("password", &this._password)?;
            kw.set_item("params", params)?;
            kw.set_item("allow_redirects", false)?;
            Ok(this
                ._session
                .bind(py)
                .call_method(
                    "request",
                    (method, format!("{}{path}", this.server_url)),
                    Some(&kw),
                )?
                .unbind())
        })?;
        let response = call(copy(&context), "__aenter__", vec![], vec![]).await?;
        let result = async {
            let status =
                Python::attach(|py| response.bind(py).getattr("status")?.extract::<u16>())?;
            let data = call(response, "json", vec![], vec![]).await;
            let data = match data {
                Ok(data) => data,
                // Only a successful response must be JSON; errors may be anything.
                Err(_) if !(200..300).contains(&status) => none(),
                Err(error) => return Err(error),
            };
            Ok((status, data))
        }
        .await;
        exit_scope(context, result.as_ref().err()).await?;
        result
    }
    .await;
    result.map_err(|e| {
        Python::attach(|py| {
            let network = py
                .import("aiohttp")
                .and_then(|m| m.getattr("ClientError"))
                .ok()
                .is_some_and(|t| e.is_instance(py, &t));
            if network
                || e.is_instance_of::<pyo3::exceptions::PyTimeoutError>(py)
                || e.is_instance_of::<PyValueError>(py)
            {
                PyRuntimeError::new_err("BlueBubbles request failed; delivery may be uncertain")
            } else {
                e
            }
        })
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<IMessage>()?;
    host::register_initialized_class::<BlueBubblesClient>(m)?;
    m.add_function(wrap_pyfunction!(load_imessage, m)?)?;
    m.add_function(wrap_pyfunction!(imessage_getattr, m)?)?;
    m.add_function(wrap_pyfunction!(imessage_dir, m)?)?;
    m.add_function(wrap_pyfunction!(imessage_normalize, m)?)?;
    m.add_function(wrap_pyfunction!(imessage_text_payload, m)?)?;
    Ok(())
}

impl BlueBubblesClient {
    fn build(py: Python<'_>, server_url: &str, password: String) -> PyResult<Self> {
        Ok(Self {
            inner: Some(Box::new(BlueBubblesClientState {
                server_url: policy::Server::parse(server_url)
                    .map_err(PyValueError::new_err)?
                    .url,
                _password: password,
                _session: py.None(),
                _closed: false,
            })),
        })
    }
}
