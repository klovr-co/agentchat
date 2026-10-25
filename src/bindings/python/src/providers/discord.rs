//! Python Discord SDK I/O driven by the native provider policy.
use crate::prompts::PromptFields;
use crate::{apps, convert, host};
use agentchat_core::{Attachment, TaskBoard};
use agentchat_providers::discord as policy;
use pyo3::{
    exceptions::{PyAttributeError, PyModuleNotFoundError, PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PyTuple, PyType},
};
use serde_json::json;
type Output = PyResult<Py<PyAny>>;
fn sdk(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import("discord").map_err(|e| {
        if e.is_instance_of::<PyModuleNotFoundError>(py) {
            PyModuleNotFoundError::new_err(
                "Install Discord support with: pip install 'agentchat[discord]'",
            )
        } else {
            e
        }
    })
}
#[pyfunction]
pub fn load_discord(py: Python<'_>) -> Output {
    sdk(py)?;
    Ok(py.get_type::<Discord>().into_any().unbind())
}
#[pyfunction]
fn discord_getattr(py: Python<'_>, name: &str) -> Output {
    if name == "Discord" {
        Ok(py
            .import("agentchat.channels.discord.adapter")?
            .getattr("Discord")?
            .unbind())
    } else {
        Err(PyAttributeError::new_err(format!(
            "module 'agentchat.channels.discord' has no attribute {name:?}"
        )))
    }
}
#[pyfunction]
fn discord_dir() -> Vec<&'static str> {
    vec!["Discord"]
}
fn event_from_python(event: &Bound<'_, PyAny>) -> PyResult<policy::Event> {
    let py = event.py();
    let module = sdk(py)?;
    let author = event.getattr("author")?;
    let channel = event.getattr("channel")?;
    let guild = event.getattr("guild")?;
    let reference = event.getattr("reference")?;
    let replied_author = if !reference.is_none() {
        let resolved = reference.getattr("resolved")?;
        if resolved.is_instance(&module.getattr("Message")?)? {
            Some(resolved.getattr("author")?.getattr("id")?.extract()?)
        } else {
            None
        }
    } else {
        None
    };
    let attachments = event
        .getattr("attachments")?
        .try_iter()?
        .map(|item| {
            let a = item?;
            Ok(Attachment {
                id: a.getattr("id")?.str()?.to_string(),
                name: a.getattr("filename")?.extract()?,
                media_type: a.getattr("content_type")?.extract()?,
                size: a.getattr("size")?.extract()?,
            })
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(policy::Event {
        id: event.getattr("id")?.extract()?,
        channel_id: channel.getattr("id")?.extract()?,
        guild_id: if guild.is_none() {
            None
        } else {
            Some(guild.getattr("id")?.extract()?)
        },
        author_id: author.getattr("id")?.extract()?,
        display_name: author
            .getattr("display_name")
            .ok()
            .and_then(|v| v.extract().ok())
            .flatten(),
        author_bot: author.getattr("bot")?.is_truthy()?,
        webhook: event.getattr("webhook_id")?.is_truthy()?,
        system: event.call_method0("is_system")?.is_truthy()?,
        mentions: event
            .getattr("mentions")?
            .try_iter()?
            .map(|v| v?.getattr("id")?.extract())
            .collect::<PyResult<Vec<_>>>()?,
        replied_author,
        is_thread: channel.is_instance(&module.getattr("Thread")?)?,
        content: event.getattr("content")?.extract()?,
        attachments,
    })
}
/// The parts of a discord.py component interaction a prompt press needs.
fn press_from_python(event: &Bound<'_, PyAny>) -> PyResult<Option<policy::Press>> {
    let py = event.py();
    let data = event.getattr("data")?;
    let custom_id = if data.is_none() {
        None
    } else {
        data.get_item("custom_id")
            .ok()
            .and_then(|v| v.extract::<String>().ok())
    };
    let message = event.getattr("message")?;
    let (Some(custom_id), false) = (custom_id, message.is_none()) else {
        return Ok(None);
    };
    let user = event.getattr("user")?;
    let channel = event.getattr("channel")?;
    let guild_id = event.getattr("guild_id")?;
    Ok(Some(policy::Press {
        id: event.getattr("id")?.extract()?,
        channel_id: event.getattr("channel_id")?.extract()?,
        guild_id: if guild_id.is_none() {
            None
        } else {
            Some(guild_id.extract()?)
        },
        user_id: user.getattr("id")?.extract()?,
        display_name: user
            .getattr("display_name")
            .ok()
            .and_then(|v| v.extract().ok())
            .flatten(),
        user_bot: user.getattr("bot")?.is_truthy()?,
        custom_id,
        message_id: message.getattr("id")?.extract()?,
        is_thread: !channel.is_none() && channel.is_instance(&sdk(py)?.getattr("Thread")?)?,
    }))
}
#[pyfunction]
#[pyo3(signature=(event,*,bot_id))]
fn discord_normalize(py: Python<'_>, event: &Bound<'_, PyAny>, bot_id: u64) -> Output {
    match policy::normalize(event_from_python(event)?, bot_id) {
        Some(message) => convert::to_message(py, &message),
        None => Ok(py.None()),
    }
}
#[pyfunction]
fn discord_text_payload(content: &Bound<'_, PyAny>) -> PyResult<String> {
    let text = content
        .extract::<String>()
        .map_err(|_| PyValueError::new_err("Discord replies require nonempty text"))?;
    policy::text_payload(&text).map_err(PyValueError::new_err)?;
    Ok(text)
}
#[pyclass(
    name = "_DiscordBase",
    dict,
    weakref,
    subclass,
    module = "agentchat._native"
)]
pub struct Discord {
    // A tracked Python object may be visited while CPython is allocating its
    // dictionary, before PyO3 installs the Rust payload. The zeroed state is valid.
    inner: Option<Box<DiscordState>>,
}
pub struct DiscordState {
    _client: Py<PyAny>,
    _sender: Py<PyAny>,
    _delivery: Py<PyAny>,
    _close_lock: Py<PyAny>,
    _closed: bool,
    _token: String,
}
impl std::ops::Deref for Discord {
    type Target = DiscordState;
    fn deref(&self) -> &Self::Target {
        self.inner.as_deref().expect("initialized native provider")
    }
}
impl std::ops::DerefMut for Discord {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("initialized native provider")
    }
}

#[pymethods]
impl Discord {
    #[getter(_client)]
    fn get_client(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._client.clone_ref(py))
    }
    #[setter(_client)]
    fn set_client(&mut self, value: Py<PyAny>) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._client = value;
        Ok(())
    }
    #[getter(_sender)]
    fn get_sender(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._sender.clone_ref(py))
    }
    #[setter(_sender)]
    fn set_sender(&mut self, value: Py<PyAny>) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._sender = value;
        Ok(())
    }
    #[getter(_delivery)]
    fn get_delivery(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._delivery.clone_ref(py))
    }
    #[setter(_delivery)]
    fn set_delivery(&mut self, value: Py<PyAny>) -> PyResult<()> {
        let state = self
            .inner
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        state._delivery = value;
        Ok(())
    }
    #[getter(_close_lock)]
    fn get_close_lock(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let state = self
            .inner
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("Native provider has been cleared"))?;
        Ok(state._close_lock.clone_ref(py))
    }
    #[setter(_close_lock)]
    fn set_close_lock(&mut self, value: Py<PyAny>) -> PyResult<()> {
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
            visit.call(&state._sender)?;
            visit.call(&state._delivery)?;
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
    #[pyo3(signature=(*,bot_token,client=None,send_client=None,max_pending_events=64,shutdown_timeout=20.0))]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        bot_token: String,
        client: Option<Py<PyAny>>,
        send_client: Option<Py<PyAny>>,
        max_pending_events: usize,
        shutdown_timeout: f64,
    ) -> PyResult<()> {
        if bot_token.trim().is_empty() {
            return Err(PyValueError::new_err("Discord bot_token is required"));
        }
        let module = sdk(py)?;
        let client = match client {
            Some(v) => v,
            None => {
                let intents = module.getattr("Intents")?.call_method0("none")?;
                for attr in ["guilds", "guild_messages", "dm_messages"] {
                    intents.setattr(attr, true)?;
                }
                let args = PyDict::new(py);
                args.set_item("intents", intents)?;
                // Raw gateway frames reach `on_event` handlers.
                args.set_item("enable_debug_events", true)?;
                args.set_item(
                    "allowed_mentions",
                    module.getattr("AllowedMentions")?.call_method0("none")?,
                )?;
                module.getattr("Client")?.call((), Some(&args))?.unbind()
            }
        };
        let sender = match send_client {
            Some(v) => v,
            None => Py::new(py, DiscordRESTClient::build(py, bot_token.clone()))?.into_any(),
        };
        let delivery = py
            .import("agentchat._delivery")?
            .getattr("DeliveryTasks")?
            .call1((max_pending_events, shutdown_timeout))?
            .unbind();
        slf.borrow_mut(py).inner = Some(Box::new(DiscordState {
            _client: client,
            _sender: sender,
            _delivery: delivery,
            _close_lock: py.import("asyncio")?.getattr("Lock")?.call0()?.unbind(),
            _closed: false,
            _token: bot_token,
        }));
        slf.bind(py).setattr(
            "_typing",
            super::typing::keeper(py, "agentchat.channels.discord.adapter", 8.0, 120.0)?,
        )?;
        let callback = host::callback(py, slf.bind(py).getattr("_on_message")?.unbind())?;
        slf.borrow(py)
            ._client
            .bind(py)
            .setattr("on_message", callback)?;
        let callback = host::callback(py, slf.bind(py).getattr("_on_interaction")?.unbind())?;
        slf.borrow(py)
            ._client
            .bind(py)
            .setattr("on_interaction", callback)?;
        let callback = host::callback(py, slf.bind(py).getattr("_on_gateway")?.unbind())?;
        slf.borrow(py)
            ._client
            .bind(py)
            .setattr("on_socket_raw_receive", callback)?;
        Ok(())
    }
    #[classattr]
    fn name() -> &'static str {
        "discord"
    }
    /// Handle gateway events by type, such as "GUILD_CREATE" when the bot joins a
    /// server. The handler receives the raw frame (`t`, `d`). Needs a client created
    /// by this channel, or one with `enable_debug_events=True`.
    fn on_event(slf: &Bound<'_, Self>, event_type: Py<PyAny>) -> Output {
        apps::decorator(slf.as_any(), "event", event_type)
    }
    /// Handle component interactions (buttons, menus) whose `custom_id` matches. The
    /// returned interaction response, such as `{"type": 9, "data": modal}` or
    /// `{"type": 4, "data": {"content": ..., "flags": 64}}`, is sent as the reply.
    fn on_action(slf: &Bound<'_, Self>, action_id: Py<PyAny>) -> Output {
        apps::decorator(slf.as_any(), "action", action_id)
    }
    /// Handle modal submissions whose `custom_id` matches; the return value is the
    /// interaction response.
    fn on_view(slf: &Bound<'_, Self>, callback_id: Py<PyAny>) -> Output {
        apps::decorator(slf.as_any(), "view", callback_id)
    }
    /// Handle an application (slash) command such as "/deploy"; the return value
    /// is the interaction response.
    fn on_command(slf: &Bound<'_, Self>, command: Py<PyAny>) -> Output {
        apps::decorator(slf.as_any(), "command", command)
    }
    /// Handle every other routed interaction or event, such as autocomplete.
    fn on_raw(slf: &Bound<'_, Self>, handler: Py<PyAny>) -> Output {
        apps::raw(slf.as_any(), handler)
    }
    fn _on_gateway(slf: Py<Self>, py: Python<'_>, raw: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            Python::attach(|py| {
                let channel = slf.bind(py);
                if apps::event_types(channel.as_any())?.is_empty() {
                    return Ok(py.None());
                }
                let raw = raw.bind(py);
                let text = match raw.extract::<String>() {
                    Ok(text) => text,
                    Err(_) => return Ok(py.None()),
                };
                let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) else {
                    return Ok(py.None());
                };
                let Some(kind) = policy::gateway_event(&frame) else {
                    return Ok(py.None());
                };
                let route = agentchat_core::Route::Event {
                    event_type: kind.into(),
                };
                if let Some(handler) = apps::find(channel.as_any(), &route)? {
                    let this = slf.borrow(py);
                    let payload = convert::from_json(py, &frame)?;
                    apps::spawn(
                        channel.as_any(),
                        this._delivery.bind(py),
                        &handler,
                        &payload,
                    )?;
                }
                Ok(py.None())
            })
        })
    }
    /// Run an interaction handler and send its response within Discord's three seconds.
    fn _respond(slf: Py<Self>, py: Python<'_>, handler: Py<PyAny>, payload: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            let (delivery, sender, kind, id, token) = Python::attach(|py| -> PyResult<_> {
                let this = slf.borrow(py);
                let data = convert::to_json(payload.bind(py))?;
                Ok((
                    this._delivery.clone_ref(py),
                    this._sender.clone_ref(py),
                    data["type"].as_u64().unwrap_or_default(),
                    data["id"].as_str().unwrap_or_default().to_owned(),
                    data["token"].as_str().unwrap_or_default().to_owned(),
                ))
            })?;
            let answer = apps::answer(delivery, handler, payload).await;
            let body = match &answer {
                Ok(body) => {
                    Python::attach(|py| convert::to_json(body.bind(py))).unwrap_or_default()
                }
                Err(_) => serde_json::Value::Null,
            };
            let response = policy::interaction_response(kind, &body);
            let call = Python::attach(|py| {
                sender
                    .bind(py)
                    .call_method1(
                        "interaction_callback",
                        (id, token, convert::from_json(py, &response)?),
                    )
                    .map(Bound::unbind)
            })?;
            host::await_py(call).await?;
            answer.map(|_| Python::attach(|py| py.None()))
        })
    }
    /// A notice only `user_id` sees. Discord shows those only in reply to an
    /// interaction, so return `{"type": 4, "data": {"content": ..., "flags": 64}}`
    /// from an `on_action` or `on_command` handler instead. In a direct message
    /// with that user, this is an ordinary reply; elsewhere it raises ValueError.
    #[pyo3(signature=(source,user_id,text,*,blocks=None))]
    fn send_ephemeral(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        user_id: String,
        text: String,
        blocks: Option<Py<PyAny>>,
    ) -> Output {
        let _ = blocks;
        let message = convert::from_message(source.bind(py))?;
        if !policy::private_reply_allowed(&message, &user_id) {
            return Err(PyValueError::new_err(
                "Discord shows private messages only in reply to an interaction: return \
                 {'type': 4, 'data': {'content': ..., 'flags': 64}} from an on_action or \
                 on_command handler",
            ));
        }
        slf.bind(py)
            .call_method1("reply", (source, text))
            .map(Bound::unbind)
    }
    /// Delete a message this bot sent.
    fn delete(slf: Py<Self>, py: Python<'_>, message: Py<PyAny>) -> Output {
        let (path, sender) = {
            let this = slf.borrow(py);
            let bot = this._client.bind(py).getattr("user")?;
            if bot.is_none() {
                return Err(PyValueError::new_err(
                    "Source does not belong to this Discord adapter",
                ));
            }
            let message = convert::from_message(message.bind(py))?;
            (
                policy::delete_path(&message, bot.getattr("id")?.extract()?)
                    .map_err(PyValueError::new_err)?,
                this._sender.clone_ref(py),
            )
        };
        Ok(sender.bind(py).call_method1("delete", (path,))?.unbind())
    }
    /// Show task progress as a separate reply, edited in place and deleted when the
    /// answer is finished. A failed update never fails the answer.
    fn update_stream_tasks(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        stream: Py<PyAny>,
        updates: Py<PyAny>,
    ) -> Output {
        host::coroutine(py, async move {
            let updates: Vec<agentchat_core::StreamUpdate> =
                serde_json::from_value(Python::attach(|py| convert::to_json(updates.bind(py)))?)
                    .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let mut handle = Python::attach(|py| {
                if stream.is_none(py) {
                    Ok(json!({}))
                } else {
                    convert::to_json(stream.bind(py))
                }
            })?;
            let mut board: TaskBoard =
                serde_json::from_value(handle["progress"]["board"].clone()).unwrap_or_default();
            board.apply(&updates);
            let text = board.render(1900);
            if text.is_empty() || handle["post"] == true {
                return Python::attach(|py| convert::from_json(py, &handle));
            }
            let progress = handle["progress"].clone();
            let (destination, sender) =
                Python::attach(|py| slf.borrow(py).stream_route(py, source.bind(py)))?;
            let shown: PyResult<u64> = match progress["message_id"].as_u64() {
                Some(id) if progress["shown"] == text.as_str() => Ok(id),
                Some(id) => {
                    let call = Python::attach(|py| {
                        sender
                            .bind(py)
                            .call_method1(
                                "edit_text",
                                (
                                    destination.channel_id,
                                    id,
                                    convert::from_json(
                                        py,
                                        &policy::Destination::edit_payload(&text, false),
                                    )?,
                                ),
                            )
                            .map(Bound::unbind)
                    })?;
                    host::await_py(call).await.map(|_| id)
                }
                None => {
                    let call = Python::attach(|py| {
                        sender
                            .bind(py)
                            .call_method1(
                                "send_text",
                                (
                                    destination.channel_id,
                                    convert::from_json(py, &destination.payload(&text))?,
                                ),
                            )
                            .map(Bound::unbind)
                    })?;
                    match host::await_py(call).await {
                        Ok(receipt) => Python::attach(|py| receipt.bind(py).extract::<u64>())
                            .map_err(|_| PyRuntimeError::new_err("No message receipt")),
                        Err(error) => Err(error),
                    }
                }
            };
            match shown {
                Ok(id) => {
                    handle["progress"] = json!({"message_id": id, "board": board, "shown": text});
                }
                Err(error) => Python::attach(|py| -> PyResult<()> {
                    py.import("logging")?
                        .call_method1("getLogger", ("agentchat.channels.discord.adapter",))?
                        .call_method1(
                            "warning",
                            (
                                "Discord task progress update failed (%s)",
                                error.to_string(),
                            ),
                        )?;
                    Ok(())
                })?,
            }
            Python::attach(|py| convert::from_json(py, &handle))
        })
    }
    #[classmethod]
    fn from_env(cls: &Bound<'_, PyType>) -> Output {
        let py = cls.py();
        let token = py
            .import("os")?
            .call_method1("getenv", ("DISCORD_BOT_TOKEN",))?;
        if !token.is_truthy()? {
            return Err(PyRuntimeError::new_err("DISCORD_BOT_TOKEN is required"));
        }
        let kw = PyDict::new(py);
        kw.set_item("bot_token", token)?;
        Ok(cls.call((), Some(&kw))?.unbind())
    }
    fn bind(&self, py: Python<'_>, receiver: Py<PyAny>) -> PyResult<()> {
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
                let start = Python::attach(|py| {
                    let this = slf.borrow(py);
                    Ok::<_, PyErr>(
                        this._client
                            .bind(py)
                            .call_method1("start", (&this._token,))?
                            .unbind(),
                    )
                })?;
                host::await_py(start).await
            }
            .await;
            let close = Python::attach(|py| slf.bind(py).call_method0("close").map(Bound::unbind))?;
            host::await_py(close).await?;
            result.map(|_| Python::attach(|py| py.None()))
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let lock = Python::attach(|py| slf.borrow(py)._close_lock.clone_ref(py));
            host::await_py(Python::attach(|py| {
                lock.bind(py).call_method0("__aenter__").map(Bound::unbind)
            })?)
            .await?;
            let result = async {
                let resources = Python::attach(|py| {
                    let mut this = slf.borrow_mut(py);
                    if this._closed {
                        return Ok(None);
                    }
                    this._closed = true;
                    this._delivery.bind(py).setattr("closed", true)?;
                    let typing = slf.bind(py).getattr("_typing")?.unbind();
                    Ok::<_, PyErr>(Some([
                        typing,
                        this._delivery.clone_ref(py),
                        this._client.clone_ref(py),
                        this._sender.clone_ref(py),
                    ]))
                })?;
                let mut result = Ok(());
                if let Some(resources) = resources {
                    for resource in resources {
                        let call = Python::attach(|py| {
                            resource.bind(py).call_method0("close").map(Bound::unbind)
                        });
                        let next = match call {
                            Ok(v) => host::await_py(v).await.map(|_| ()),
                            Err(e) => Err(e),
                        };
                        if next.is_err() {
                            result = next;
                        }
                    }
                }
                result
            }
            .await;
            host::await_py(Python::attach(|py| {
                lock.bind(py)
                    .call_method1("__aexit__", (py.None(), py.None(), py.None()))
                    .map(Bound::unbind)
            })?)
            .await?;
            result?;
            Ok(Python::attach(|py| py.None()))
        })
    }
    fn _on_message(slf: Py<Self>, py: Python<'_>, event: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            Python::attach(|py| {
                let this = slf.borrow(py);
                let user = this._client.bind(py).getattr("user")?;
                if user.is_none() || this._delivery.bind(py).getattr("closed")?.is_truthy()? {
                    return Ok(py.None());
                }
                if let Some(message) = policy::normalize(
                    event_from_python(event.bind(py))?,
                    user.getattr("id")?.extract()?,
                ) {
                    let message = convert::to_message(py, &message)?;
                    this._delivery
                        .bind(py)
                        .call_method1("submit", (slf.clone_ref(py), message))?;
                }
                Ok(py.None())
            })
        })
    }
    /// Discord allows about five edits per five seconds in a channel.
    #[classattr]
    fn stream_interval() -> f64 {
        1.0
    }
    #[classattr]
    fn stream_limit() -> usize {
        2000
    }
    /// Post the first part of a streamed answer; later parts edit it. A send client
    /// without `edit_text` posts the whole answer once when the stream stops.
    fn start_stream(slf: Py<Self>, py: Python<'_>, source: Py<PyAny>, text: String) -> Output {
        host::coroutine(py, async move {
            let (destination, sender) =
                Python::attach(|py| slf.borrow(py).stream_route(py, source.bind(py)))?;
            if !Python::attach(|py| sender.bind(py).hasattr("edit_text"))? {
                return Python::attach(|py| convert::from_json(py, &json!({"post": true})));
            }
            let shown = if policy::text_payload(&text).is_ok() {
                text.clone()
            } else {
                "…".to_owned()
            };
            let call = Python::attach(|py| {
                sender
                    .bind(py)
                    .call_method1(
                        "send_text",
                        (
                            destination.channel_id,
                            convert::from_json(py, &destination.payload(&shown))?,
                        ),
                    )
                    .map(Bound::unbind)
            })?;
            let receipt = host::await_py(call).await?;
            Python::attach(|py| {
                let receipt = receipt.bind(py).extract::<u64>().map_err(|_| {
                    PyRuntimeError::new_err("Discord did not return a message receipt")
                })?;
                convert::from_json(py, &json!({"message_id": receipt, "shown": shown}))
            })
        })
    }
    fn append_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        stream: Py<PyAny>,
        text: String,
    ) -> Output {
        host::coroutine(py, async move {
            let handle = Python::attach(|py| convert::to_json(stream.bind(py)))?;
            if handle.get("message_id").is_none() && handle.get("progress").is_some() {
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
                    started["progress"] = handle["progress"].clone();
                    convert::from_json(py, &started)
                });
            }
            let (_, stream) = edit_streamed(&slf, &source, &stream, &text).await?;
            Ok(stream)
        })
    }
    fn stop_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        stream: Py<PyAny>,
        text: String,
    ) -> Output {
        host::coroutine(py, async move {
            let handle = Python::attach(|py| convert::to_json(stream.bind(py)))?;
            let result = finish_stream(&slf, &source, &stream, &handle, text).await;
            remove_progress(&slf, &source, &handle).await;
            result
        })
    }
    /// Show "typing…" in the source channel until cleared with "" or the keeper expires.
    /// Discord shows no status text. A send client without `trigger_typing` returns False.
    fn set_status(slf: Py<Self>, py: Python<'_>, source: Py<PyAny>, status: String) -> Output {
        host::coroutine(py, async move {
            let started = Python::attach(|py| -> PyResult<Result<Py<PyAny>, bool>> {
                let (channel_id, sender) = slf.borrow(py).status_route(py, source.bind(py))?;
                let keeper = slf.bind(py).getattr("_typing")?;
                if status.is_empty() {
                    keeper.call_method1("stop", (channel_id,))?;
                    return Ok(Err(true));
                }
                if !sender.bind(py).hasattr("trigger_typing")? {
                    return Ok(Err(false));
                }
                let send = py
                    .import("functools")?
                    .getattr("partial")?
                    .call1((sender.bind(py).getattr("trigger_typing")?, channel_id))?;
                Ok(Ok(keeper
                    .call_method1("start", (channel_id, send))?
                    .unbind()))
            })?;
            match started {
                Ok(pending) => host::await_py(pending).await,
                Err(shown) => {
                    Python::attach(|py| Ok(shown.into_pyobject(py)?.to_owned().into_any().unbind()))
                }
            }
        })
    }
    /// A press on a prompt button: acknowledge it within Discord's three seconds,
    /// then hand it to AgentChat. Other interactions are left alone.
    fn _on_interaction(slf: Py<Self>, py: Python<'_>, interaction: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            let press = Python::attach(|py| -> PyResult<Option<(Py<PyAny>, Py<PyAny>)>> {
                let this = slf.borrow(py);
                let user = this._client.bind(py).getattr("user")?;
                if user.is_none() || this._delivery.bind(py).getattr("closed")?.is_truthy()? {
                    return Ok(None);
                }
                let event = interaction.bind(py);
                // Other interactions go to the application's handlers, if any.
                if slf
                    .bind(py)
                    .call_method1("_on_app_interaction", (event,))?
                    .is_truthy()?
                {
                    return Ok(None);
                }
                let component = sdk(py)?.getattr("InteractionType")?.getattr("component")?;
                if !event.getattr("type")?.eq(component)? {
                    return Ok(None);
                }
                let Some(press) = press_from_python(event)? else {
                    return Ok(None);
                };
                let Some(message) = policy::normalize_press(press, user.getattr("id")?.extract()?)
                else {
                    return Ok(None);
                };
                let defer = event.getattr("response")?.call_method0("defer")?.unbind();
                Ok(Some((defer, convert::to_message(py, &message)?)))
            })?;
            let Some((defer, message)) = press else {
                return Ok(Python::attach(|py| py.None()));
            };
            host::await_py(defer).await?;
            Python::attach(|py| {
                slf.borrow(py)
                    ._delivery
                    .bind(py)
                    .call_method1("submit", (slf.clone_ref(py), message))?;
                Ok(py.None())
            })
        })
    }
    /// Hand an interaction to the application's handler, if it has one; True when
    /// handled. Prompt presses are never the application's.
    fn _on_app_interaction(slf: &Bound<'_, Self>, event: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = slf.py();
        let kind: u64 = event.getattr("type")?.getattr("value")?.extract()?;
        let data = event.getattr("data")?;
        let data = if data.is_none() {
            serde_json::Value::Null
        } else {
            convert::to_json(&data)?
        };
        let Some(route) = policy::interaction_route(kind, &data) else {
            return Ok(false);
        };
        let Some(handler) = apps::find(slf.as_any(), &route)? else {
            return Ok(false);
        };
        let text = |value: Bound<'_, PyAny>| -> PyResult<serde_json::Value> {
            Ok(if value.is_none() {
                serde_json::Value::Null
            } else {
                json!(value.str()?.to_string())
            })
        };
        let user = event.getattr("user")?;
        let message = event.getattr("message")?;
        let payload = json!({
            "id": text(event.getattr("id")?)?,
            "type": kind,
            "token": event.getattr("token")?.extract::<String>()?,
            "application_id": text(event.getattr("application_id")?)?,
            "guild_id": text(event.getattr("guild_id")?)?,
            "channel_id": text(event.getattr("channel_id")?)?,
            "user": {"id": text(user.getattr("id")?)?, "name": text(user.getattr("name")?)?},
            "message": if message.is_none() { serde_json::Value::Null } else { json!({"id": text(message.getattr("id")?)?}) },
            "data": data,
        });
        let pending = slf.call_method1("_respond", (handler, convert::from_json(py, &payload)?))?;
        let this = slf.borrow();
        this._delivery
            .bind(py)
            .call_method1("spawn", (slf, pending))?;
        Ok(true)
    }
    /// Show a prompt as a reply with one button per choice.
    fn send_prompt(slf: Py<Self>, py: Python<'_>, source: Py<PyAny>, prompt: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            let (destination, fields, call) = Python::attach(|py| {
                let (destination, sender) = slf.borrow(py).stream_route(py, source.bind(py))?;
                let fields = PromptFields::read(prompt.bind(py))?;
                let payload = destination
                    .prompt_payload(&fields.text, &fields.id, &fields.choices)
                    .map_err(PyValueError::new_err)?;
                let call = sender
                    .bind(py)
                    .call_method1(
                        "send_text",
                        (destination.channel_id, convert::from_json(py, &payload)?),
                    )?
                    .unbind();
                Ok::<_, PyErr>((destination, fields, call))
            })?;
            let receipt = host::await_py(call).await?;
            Python::attach(|py| {
                let this = slf.borrow(py);
                let receipt = receipt.bind(py).extract::<u64>().map_err(|_| {
                    PyRuntimeError::new_err("Discord did not return a message receipt")
                })?;
                let display_name = this
                    ._client
                    .bind(py)
                    .getattr("user")?
                    .getattr("display_name")?
                    .extract::<Option<String>>()?;
                let message = destination
                    .confirmed_reply(
                        &convert::from_message(source.bind(py))?,
                        fields.text,
                        receipt,
                        display_name,
                    )
                    .map_err(PyRuntimeError::new_err)?;
                super::transport::receipt_with_metadata(&source, &message)
            })
        })
    }
    /// Take the buttons away; an answered prompt also shows the choice and who made it.
    /// A send client without `edit_text` leaves the buttons.
    fn close_prompt(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        sent: Py<PyAny>,
        prompt: Py<PyAny>,
        answer: Py<PyAny>,
    ) -> Output {
        host::coroutine(py, async move {
            let call = Python::attach(|py| -> PyResult<Option<Py<PyAny>>> {
                let (destination, sender) = slf.borrow(py).stream_route(py, source.bind(py))?;
                if !sender.bind(py).hasattr("edit_text")? {
                    return Ok(None);
                }
                let fields = PromptFields::read(prompt.bind(py))?;
                let content = fields.closed_text(answer.bind(py))?.unwrap_or(fields.text);
                let message_id = convert::from_message(sent.bind(py))?.metadata["message_id"]
                    .as_u64()
                    .ok_or_else(|| PyValueError::new_err("Expected a sent Discord prompt"))?;
                let payload = policy::Destination::edit_payload(&content, true);
                Ok(Some(
                    sender
                        .bind(py)
                        .call_method1(
                            "edit_text",
                            (
                                destination.channel_id,
                                message_id,
                                convert::from_json(py, &payload)?,
                            ),
                        )?
                        .unbind(),
                ))
            })?;
            if let Some(call) = call {
                host::await_py(call).await?;
            }
            Ok(Python::attach(|py| py.None()))
        })
    }
    fn reply(slf: Py<Self>, py: Python<'_>, source: Py<PyAny>, content: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            let source_host = Python::attach(|py| source.clone_ref(py));
            let (source, content, destination, display_name, call) = Python::attach(|py| {
                let content = discord_text_payload(content.bind(py))?;
                let source = convert::from_message(source.bind(py))?;
                let this = slf.borrow(py);
                let bot = this._client.bind(py).getattr("user")?;
                if bot.is_none() {
                    return Err(PyValueError::new_err(
                        "Source does not belong to this Discord adapter",
                    ));
                }
                let destination =
                    policy::Destination::from_source(&source, bot.getattr("id")?.extract()?)
                        .map_err(PyValueError::new_err)?;
                if this
                    ._client
                    .bind(py)
                    .call_method0("is_closed")?
                    .is_truthy()?
                {
                    return Err(PyRuntimeError::new_err("Discord connection is closed"));
                }
                this._delivery.bind(py).call_method0("check_reply")?;
                let display_name = bot.getattr("display_name")?.extract::<Option<String>>()?;
                let call = this
                    ._sender
                    .bind(py)
                    .call_method1(
                        "send_text",
                        (
                            destination.channel_id,
                            convert::from_json(py, &destination.payload(&content))?,
                        ),
                    )?
                    .unbind();
                Ok((source, content, destination, display_name, call))
            })?;
            let receipt = host::await_py(call).await?;
            Python::attach(|py| {
                let receipt = receipt.bind(py).extract::<u64>().map_err(|_| {
                    PyRuntimeError::new_err("Discord did not return a message receipt")
                })?;
                let message = destination
                    .confirmed_reply(&source, content, receipt, display_name)
                    .map_err(PyRuntimeError::new_err)?;
                super::transport::receipt_with_metadata(&source_host, &message)
            })
        })
    }
}
/// Delete the task progress reply once the answer is finished; best effort.
async fn remove_progress(slf: &Py<Discord>, source: &Py<PyAny>, handle: &serde_json::Value) {
    let Some(id) = handle["progress"]["message_id"].as_u64() else {
        return;
    };
    let call = Python::attach(|py| -> PyResult<Py<PyAny>> {
        let (destination, sender) = slf.borrow(py).stream_route(py, source.bind(py))?;
        Ok(sender
            .bind(py)
            .call_method1(
                "delete",
                (format!("channels/{}/messages/{id}", destination.channel_id),),
            )?
            .unbind())
    });
    if let Ok(call) = call {
        let _ = host::await_py(call).await;
    }
}
/// Finish a streamed answer and confirm it. A stream that only showed task
/// progress is confirmed without text.
async fn finish_stream(
    slf: &Py<Discord>,
    source: &Py<PyAny>,
    stream: &Py<PyAny>,
    handle: &serde_json::Value,
    text: String,
) -> Output {
    if handle.get("message_id").is_none() && text.trim().is_empty() {
        if let Some(progress) = handle["progress"]["message_id"].as_u64() {
            return Python::attach(|py| {
                let this = slf.borrow(py);
                let (destination, _) = this.stream_route(py, source.bind(py))?;
                let message = convert::from_message(source.bind(py))?;
                let reply = destination
                    .confirmed_reply(&message, String::new(), progress, None)
                    .map_err(PyRuntimeError::new_err)?;
                super::transport::receipt_with_metadata(source, &reply)
            });
        }
    }
    policy::text_payload(&text).map_err(PyValueError::new_err)?;
    let (Some(message_id), _) = edit_streamed(slf, source, stream, &text).await? else {
        let reply = Python::attach(|py| {
            slf.bind(py)
                .call_method1("reply", (source.bind(py), &text))
                .map(Bound::unbind)
        })?;
        return host::await_py(reply).await;
    };
    Python::attach(|py| {
        let this = slf.borrow(py);
        let (destination, _) = this.stream_route(py, source.bind(py))?;
        let message = convert::from_message(source.bind(py))?;
        let display_name = this
            ._client
            .bind(py)
            .getattr("user")?
            .getattr("display_name")?
            .extract::<Option<String>>()?;
        let reply = destination
            .confirmed_reply(&message, text, message_id, display_name)
            .map_err(PyRuntimeError::new_err)?;
        super::transport::receipt_with_metadata(source, &reply)
    })
}
/// A streamed message's ID, the edit to await (None when unchanged), and the next handle.
type StreamEdit = (u64, Option<Py<PyAny>>, Py<PyAny>);
/// Edit a streamed message to `text`, unless it already shows it. Returns the message
/// ID (None when the stream posts once at the end) and the handle for the next call.
async fn edit_streamed(
    slf: &Py<Discord>,
    source: &Py<PyAny>,
    stream: &Py<PyAny>,
    text: &str,
) -> PyResult<(Option<u64>, Py<PyAny>)> {
    let call = Python::attach(|py| -> PyResult<Option<StreamEdit>> {
        let mut handle = convert::to_json(stream.bind(py))?;
        let Some(message_id) = handle["message_id"].as_u64() else {
            return Ok(None);
        };
        let (destination, sender) = slf.borrow(py).stream_route(py, source.bind(py))?;
        let shown = if policy::text_payload(text).is_ok() {
            text
        } else {
            "…"
        };
        if handle["shown"] == shown {
            return Ok(Some((message_id, None, stream.clone_ref(py))));
        }
        handle["shown"] = json!(shown);
        let payload = policy::Destination::edit_payload(shown, false);
        let call = sender.bind(py).call_method1(
            "edit_text",
            (
                destination.channel_id,
                message_id,
                convert::from_json(py, &payload)?,
            ),
        )?;
        Ok(Some((
            message_id,
            Some(call.unbind()),
            convert::from_json(py, &handle)?,
        )))
    })?;
    match call {
        Some((message_id, call, handle)) => {
            if let Some(call) = call {
                host::await_py(call).await?;
            }
            Ok((Some(message_id), handle))
        }
        None => Ok((None, Python::attach(|py| stream.clone_ref(py)))),
    }
}
impl Discord {
    fn stream_route(
        &self,
        py: Python<'_>,
        source: &Bound<'_, PyAny>,
    ) -> PyResult<(policy::Destination, Py<PyAny>)> {
        let source = convert::from_message(source)?;
        let bot = self._client.bind(py).getattr("user")?;
        if bot.is_none() {
            return Err(PyValueError::new_err(
                "Source does not belong to this Discord adapter",
            ));
        }
        let destination = policy::Destination::from_source(&source, bot.getattr("id")?.extract()?)
            .map_err(PyValueError::new_err)?;
        self._delivery.bind(py).call_method0("check_reply")?;
        Ok((destination, self._sender.clone_ref(py)))
    }
    fn status_route(
        &self,
        py: Python<'_>,
        source: &Bound<'_, PyAny>,
    ) -> PyResult<(u64, Py<PyAny>)> {
        let source = convert::from_message(source)?;
        let bot = self._client.bind(py).getattr("user")?;
        if bot.is_none() {
            return Err(PyValueError::new_err(
                "Source does not belong to this Discord adapter",
            ));
        }
        let destination = policy::Destination::from_source(&source, bot.getattr("id")?.extract()?)
            .map_err(PyValueError::new_err)?;
        self._delivery.bind(py).call_method0("check_reply")?;
        Ok((destination.channel_id, self._sender.clone_ref(py)))
    }
}
#[pyclass(
    dict,
    weakref,
    subclass,
    module = "agentchat.channels.discord.transport"
)]
pub struct DiscordRESTClient {
    inner: Option<Box<DiscordRESTClientState>>,
}
pub struct DiscordRESTClientState {
    _token: String,
    _session: Py<PyAny>,
    _closed: bool,
}
impl std::ops::Deref for DiscordRESTClient {
    type Target = DiscordRESTClientState;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_deref()
            .expect("native object has been cleared")
    }
}
impl std::ops::DerefMut for DiscordRESTClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("native object has been cleared")
    }
}
#[pymethods]
impl DiscordRESTClient {
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

    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self { inner: None }
    }
    fn initialize(&mut self, py: Python<'_>, bot_token: String) {
        *self = Self::build(py, bot_token);
    }
    fn send_text(slf: Py<Self>, py: Python<'_>, channel_id: u64, payload: Py<PyAny>) -> Output {
        host::coroutine(py, async move {
            let data = rest_request(
                &slf,
                "post",
                "send",
                &format!("channels/{channel_id}/messages"),
                Some(payload),
            )
            .await
            .map_err(uncertain_delivery)?;
            Python::attach(|py| {
                let id = policy::receipt(&convert::to_json(data.bind(py))?)
                    .map_err(PyRuntimeError::new_err)?;
                Ok(id.into_pyobject(py)?.into_any().unbind())
            })
        })
    }
    /// Replace the text of a message this bot sent.
    fn edit_text(
        slf: Py<Self>,
        py: Python<'_>,
        channel_id: u64,
        message_id: u64,
        payload: Py<PyAny>,
    ) -> Output {
        host::coroutine(py, async move {
            let path = format!("channels/{channel_id}/messages/{message_id}");
            rest_request(&slf, "patch", "edit", &path, Some(payload))
                .await
                .map_err(uncertain_delivery)?;
            Ok(Python::attach(|py| py.None()))
        })
    }
    /// Respond to an interaction (`POST /interactions/{id}/{token}/callback`).
    fn interaction_callback(
        slf: Py<Self>,
        py: Python<'_>,
        interaction_id: String,
        token: String,
        payload: Py<PyAny>,
    ) -> Output {
        host::coroutine(py, async move {
            if !interaction_id.bytes().all(|c| c.is_ascii_digit())
                || token.is_empty()
                || !token
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
            {
                return Err(PyValueError::new_err("Expected a Discord interaction"));
            }
            rest_request(
                &slf,
                "post",
                "interaction response",
                &format!("interactions/{interaction_id}/{token}/callback"),
                Some(payload),
            )
            .await
            .map_err(uncertain_delivery)?;
            Ok(Python::attach(|py| py.None()))
        })
    }
    /// Delete a message (`DELETE channels/{channel}/messages/{message}`).
    fn delete(slf: Py<Self>, py: Python<'_>, path: String) -> Output {
        host::coroutine(py, async move {
            let valid = path
                .strip_prefix("channels/")
                .and_then(|rest| rest.split_once("/messages/"))
                .is_some_and(|(channel, message)| {
                    [channel, message]
                        .iter()
                        .all(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
                });
            if !valid {
                return Err(PyValueError::new_err(
                    "Expected a Discord message to delete",
                ));
            }
            rest_request(&slf, "delete", "delete", &path, None).await?;
            Ok(Python::attach(|py| py.None()))
        })
    }
    /// Show the typing indicator in a channel; Discord clears it after about ten seconds.
    fn trigger_typing(slf: Py<Self>, py: Python<'_>, channel_id: u64) -> Output {
        host::coroutine(py, async move {
            rest_request(
                &slf,
                "post",
                "typing",
                &format!("channels/{channel_id}/typing"),
                None,
            )
            .await?;
            Ok(Python::attach(|py| py.None()))
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let call = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                this._closed = true;
                if this._session.is_none(py) {
                    Ok(None)
                } else {
                    Ok::<_, PyErr>(Some(this._session.bind(py).call_method0("close")?.unbind()))
                }
            })?;
            if let Some(call) = call {
                host::await_py(call).await?;
            }
            Ok(Python::attach(|py| py.None()))
        })
    }
}
/// Call the Discord REST API; returns the JSON body, or None for an empty reply.
async fn rest_request(
    slf: &Py<DiscordRESTClient>,
    method: &str,
    action: &str,
    path: &str,
    payload: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    let context = Python::attach(|py| {
        let mut this = slf.borrow_mut(py);
        if this._closed {
            return Err(PyRuntimeError::new_err("Discord send client is closed"));
        }
        if this._session.is_none(py) {
            let aio = py.import("aiohttp")?;
            let timeout_args = PyDict::new(py);
            timeout_args.set_item("total", 30)?;
            let args = PyDict::new(py);
            args.set_item(
                "timeout",
                aio.getattr("ClientTimeout")?
                    .call((), Some(&timeout_args))?,
            )?;
            this._session = aio
                .getattr("ClientSession")?
                .call((), Some(&args))?
                .unbind();
        }
        let root = py
            .import("agentchat.channels.discord.transport")?
            .getattr("API_ROOT")?
            .extract::<String>()?;
        let args = PyDict::new(py);
        let headers = PyDict::new(py);
        headers.set_item("Authorization", format!("Bot {}", this._token))?;
        args.set_item("headers", headers)?;
        if let Some(payload) = payload {
            args.set_item("json", payload)?;
        }
        args.set_item("allow_redirects", false)?;
        Ok(this
            ._session
            .bind(py)
            .call_method(method, (format!("{root}/{path}"),), Some(&args))?
            .unbind())
    })?;
    let response = host::await_py(Python::attach(|py| {
        context
            .bind(py)
            .call_method0("__aenter__")
            .map(Bound::unbind)
    })?)
    .await?;
    let result = async {
        let call = Python::attach(|py| {
            let status = response.bind(py).getattr("status")?.extract::<u16>()?;
            if !(200..300).contains(&status) {
                return Err(PyRuntimeError::new_err(format!(
                    "Discord {action} failed (HTTP {status})"
                )));
            }
            if status == 204 {
                return Ok(None);
            }
            Ok(Some(response.bind(py).call_method0("json")?.unbind()))
        })?;
        match call {
            Some(call) => host::await_py(call).await,
            None => Ok(Python::attach(|py| py.None())),
        }
    }
    .await;
    host::await_py(Python::attach(|py| {
        context
            .bind(py)
            .call_method1("__aexit__", (py.None(), py.None(), py.None()))
            .map(Bound::unbind)
    })?)
    .await?;
    result
}
/// A failed or interrupted send may still have been delivered; never imply otherwise.
fn uncertain_delivery(e: PyErr) -> PyErr {
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
            PyRuntimeError::new_err("Discord request failed; delivery may be uncertain")
        } else {
            e
        }
    })
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Discord>()?;
    host::register_initialized_class::<DiscordRESTClient>(m)?;
    m.add_function(wrap_pyfunction!(load_discord, m)?)?;
    m.add_function(wrap_pyfunction!(discord_getattr, m)?)?;
    m.add_function(wrap_pyfunction!(discord_dir, m)?)?;
    m.add_function(wrap_pyfunction!(discord_normalize, m)?)?;
    m.add_function(wrap_pyfunction!(discord_text_payload, m)?)?;
    Ok(())
}

impl DiscordRESTClient {
    fn build(py: Python<'_>, bot_token: String) -> Self {
        Self {
            inner: Some(Box::new(DiscordRESTClientState {
                _token: bot_token,
                _session: py.None(),
                _closed: false,
            })),
        }
    }
}
