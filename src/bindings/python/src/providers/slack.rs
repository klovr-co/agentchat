//! Python SDK effects for the shared Slack provider policy.
use super::{
    slack_files::{self, policy_error, PythonFiles},
    transport::*,
};
use crate::prompts::PromptFields;
use crate::{convert, host};
use agentchat_core::{Message, Role};
use agentchat_slack::{
    events,
    host::ack_request,
    interaction::{self, Route, ViewTarget},
    policy::{
        self, DeliveryKind, MessageReceipt, MirroredReply, ReplyDestination, ReplyMessageRoute,
        ThreadHistory,
    },
    rendering,
};
use pyo3::{
    exceptions::{PyException, PyRuntimeError, PyTypeError, PyValueError},
    prelude::*,
    types::{PyDict, PyList, PySet, PyTuple, PyType},
    PyTraverseError, PyVisit,
};
use serde_json::json;
type Object = Py<PyAny>;
const MODULE: &str = "agentchat.channels.slack";
/// Call a Web API method the SDK may not wrap yet; an unconfirmed call raises.
async fn slack_api(
    web: &Object,
    method: &str,
    payload: serde_json::Value,
) -> PyResult<serde_json::Value> {
    let response = acall(
        web,
        "api_call",
        vec![j(json!(method))?],
        Some(j(json!({"json": payload}))?),
    )
    .await?;
    let response = value(&response)?;
    if response["ok"] != true {
        return Err(PyRuntimeError::new_err(format!(
            "Slack {method} could not be confirmed"
        )));
    }
    Ok(response)
}
/// One write attempt: `Ok(Err(code))` when Slack definitely refused it, `Err` when
/// the outcome is unknown.
async fn slack_attempt(
    web: &Object,
    method: &str,
    payload: serde_json::Value,
) -> PyResult<Result<serde_json::Value, String>> {
    let response = acall(
        web,
        "api_call",
        vec![j(json!(method))?],
        Some(j(json!({"json": payload}))?),
    )
    .await;
    let response = match response {
        Ok(response) => value(&response)?,
        Err(error) => {
            // slack_sdk raises SlackApiError for `ok: false`; its response holds the code.
            let refused = Python::attach(|py| -> PyResult<Option<serde_json::Value>> {
                let api_error = py.import("slack_sdk.errors")?.getattr("SlackApiError")?;
                if !error.is_instance(py, &api_error) {
                    return Ok(None);
                }
                let response = error.value(py).getattr("response")?;
                Ok(Some(crate::convert::to_json(&response)?))
            });
            match refused {
                Ok(Some(response)) => response,
                _ => return Err(error),
            }
        }
    };
    if response["ok"] == true {
        return Ok(Ok(response));
    }
    Ok(Err(response["error"]
        .as_str()
        .unwrap_or("unknown_error")
        .to_owned()))
}
/// A Web API call through the SDK's own method; an unconfirmed call raises.
async fn sdk_call(
    web: &Object,
    method: &str,
    payload: serde_json::Value,
) -> PyResult<serde_json::Value> {
    let response = value(&acall(web, method, vec![], Some(j(payload)?)).await?)?;
    if response["ok"] != true {
        return Err(PyRuntimeError::new_err(format!(
            "Slack {method} could not be confirmed ({})",
            response["error"].as_str().unwrap_or("unknown_error")
        )));
    }
    Ok(response)
}
fn from_message(value: &Object) -> PyResult<Message> {
    Python::attach(|py| convert::from_message(value.bind(py)))
}
fn task(pending: Object) -> PyResult<Object> {
    call(&module("asyncio")?, "create_task", vec![pending], None)
}
async fn shield(pending: Object) -> PyResult<Object> {
    acall(&module("asyncio")?, "shield", vec![pending], None).await
}
fn cancelled(error: &PyErr) -> bool {
    Python::attach(|py| {
        py.import("asyncio")
            .and_then(|m| m.getattr("CancelledError"))
            .is_ok_and(|c| error.matches(py, &c).unwrap_or(false))
    })
}
#[pyclass(dict, subclass, weakref, module = "agentchat._native")]
struct MemoryThreadSubscriptions {
    threads: policy::SubscriptionCache,
}
impl MemoryThreadSubscriptions {
    fn build() -> Self {
        Self {
            threads: policy::SubscriptionCache::default(),
        }
    }
}
#[pymethods]
impl MemoryThreadSubscriptions {
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self::build()
    }
    fn initialize(&mut self) {
        self.threads = policy::SubscriptionCache::default();
    }
    fn contains(slf: Py<Self>, py: Python<'_>, conversation_id: String) -> PyResult<Object> {
        host::coroutine(py, async move {
            let contains = Python::attach(|py| slf.borrow(py).threads.contains(&conversation_id));
            j(json!(contains))
        })
    }
    fn add(slf: Py<Self>, py: Python<'_>, conversation_id: String) -> PyResult<Object> {
        host::coroutine(py, async move {
            Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                this.threads.add(&conversation_id);
            });
            Ok(none())
        })
    }
}
#[pyclass(dict, subclass, weakref, module = "agentchat._native")]
pub struct Slack {
    state: Option<Box<SlackState>>,
}
pub struct SlackState {
    _bot_token: String,
    _web_client: Object,
    _event_source: Object,
    _receiver: Option<Object>,
    _run_started: bool,
    _auth_task: Option<Object>,
    _auth_cancelled_for_close: bool,
    _close_task: Option<Object>,
    _ack_emoji: Option<String>,
    _workspace_id: Option<String>,
    _bot_user_id: Option<String>,
    _threads: Object,
    _agent_sessions: bool,
    /// The application's handlers: `(kind, key, handler)` tuples.
    _routes: Py<PyList>,
    /// Handlers that missed the acknowledgement deadline and still run.
    _late: Py<PySet>,
}
impl std::ops::Deref for Slack {
    type Target = SlackState;
    fn deref(&self) -> &Self::Target {
        self.state
            .as_deref()
            .expect("Slack state exists outside garbage collection")
    }
}
impl std::ops::DerefMut for Slack {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.state
            .as_deref_mut()
            .expect("Slack state exists outside garbage collection")
    }
}

impl Slack {
    fn destination(&self, source: &Message, strict: bool) -> PyResult<ReplyDestination> {
        let destination = ReplyDestination::from_source(source, self._workspace_id.as_deref())
            .map_err(policy_error)?;
        if strict {
            ReplyDestination::validate(&destination.channel_id, destination.thread_ts.as_deref())
                .map_err(policy_error)?;
        }
        Ok(destination)
    }
}
#[pymethods]
impl Slack {
    #[getter(_bot_token)]
    fn get_bot_token(&self) -> String {
        self._bot_token.clone()
    }
    #[getter(_web_client)]
    fn get_web_client(&self, py: Python<'_>) -> Object {
        self._web_client.clone_ref(py)
    }
    #[getter(_event_source)]
    fn get_event_source(&self, py: Python<'_>) -> Object {
        self._event_source.clone_ref(py)
    }
    #[getter(_receiver)]
    fn get_receiver(&self, py: Python<'_>) -> Option<Object> {
        self._receiver.as_ref().map(|value| value.clone_ref(py))
    }
    #[setter(_receiver)]
    fn set_receiver(&mut self, value: Option<Object>) {
        self._receiver = value;
    }
    #[getter(_run_started)]
    fn get_run_started(&self) -> bool {
        self._run_started
    }
    #[setter(_run_started)]
    fn set_run_started(&mut self, value: bool) {
        self._run_started = value;
    }
    #[getter(_auth_task)]
    fn get_auth_task(&self, py: Python<'_>) -> Option<Object> {
        self._auth_task.as_ref().map(|value| value.clone_ref(py))
    }
    #[setter(_auth_task)]
    fn set_auth_task(&mut self, value: Option<Object>) {
        self._auth_task = value;
    }
    #[getter(_auth_cancelled_for_close)]
    fn get_auth_cancelled_for_close(&self) -> bool {
        self._auth_cancelled_for_close
    }
    #[setter(_auth_cancelled_for_close)]
    fn set_auth_cancelled_for_close(&mut self, value: bool) {
        self._auth_cancelled_for_close = value;
    }
    #[getter(_close_task)]
    fn get_close_task(&self, py: Python<'_>) -> Option<Object> {
        self._close_task.as_ref().map(|value| value.clone_ref(py))
    }
    #[setter(_close_task)]
    fn set_close_task(&mut self, value: Option<Object>) {
        self._close_task = value;
    }
    #[getter(_ack_emoji)]
    fn get_ack_emoji(&self) -> Option<String> {
        self._ack_emoji.clone()
    }
    #[setter(_ack_emoji)]
    fn set_ack_emoji(&mut self, value: Option<String>) {
        self._ack_emoji = value;
    }
    #[getter(_workspace_id)]
    fn get_workspace_id(&self) -> Option<String> {
        self._workspace_id.clone()
    }
    #[setter(_workspace_id)]
    fn set_workspace_id(&mut self, value: Option<String>) {
        self._workspace_id = value;
    }
    #[getter(_bot_user_id)]
    fn get_bot_user_id(&self) -> Option<String> {
        self._bot_user_id.clone()
    }
    #[setter(_bot_user_id)]
    fn set_bot_user_id(&mut self, value: Option<String>) {
        self._bot_user_id = value;
    }
    #[getter(_threads)]
    fn get_threads(&self, py: Python<'_>) -> Object {
        self._threads.clone_ref(py)
    }

    #[classattr]
    #[pyo3(name = "name")]
    const NAME: &'static str = "slack";
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self { state: None }
    }
    #[pyo3(signature=(*,bot_token,app_token=None,event_source=None,web_client=None,ack_emoji=Some("eyes"),workspace_id=None,bot_user_id=None,thread_subscriptions=None,max_pending_events=64,agent_sessions=false))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        &mut self,
        py: Python<'_>,
        bot_token: String,
        app_token: Option<String>,
        event_source: Option<Object>,
        web_client: Option<Object>,
        ack_emoji: Option<&str>,
        workspace_id: Option<String>,
        bot_user_id: Option<String>,
        thread_subscriptions: Option<Object>,
        max_pending_events: i64,
        agent_sessions: bool,
    ) -> PyResult<()> {
        *self = Self::build(
            py,
            bot_token,
            app_token,
            event_source,
            web_client,
            ack_emoji,
            workspace_id,
            bot_user_id,
            thread_subscriptions,
            max_pending_events,
            agent_sessions,
        )?;
        Ok(())
    }
    #[getter(agent_sessions)]
    fn get_agent_sessions(&self) -> bool {
        self._agent_sessions
    }
    fn _register(
        &self,
        py: Python<'_>,
        kind: &str,
        key: Object,
        handler: Object,
    ) -> PyResult<Object> {
        if !handler.bind(py).is_callable() {
            return Err(PyTypeError::new_err("A Slack handler must be callable"));
        }
        self._routes
            .bind(py)
            .append((kind, key, handler.clone_ref(py)))?;
        Ok(handler)
    }
    fn _decorator(slf: &Bound<'_, Self>, kind: &str, key: Object) -> PyResult<Object> {
        let py = slf.py();
        let text = key.bind(py).extract::<String>().is_ok();
        if !text && (matches!(kind, "event" | "command") || !key.bind(py).hasattr("search")?) {
            return Err(PyTypeError::new_err(
                if matches!(kind, "event" | "command") {
                    "Expected a string"
                } else {
                    "Expected a string or a compiled pattern"
                },
            ));
        }
        Ok(py
            .import("functools")?
            .getattr("partial")?
            .call1((slf.getattr("_register")?, kind, key))?
            .unbind())
    }
    /// Handle Events API events of `event_type` that AgentChat does not turn into
    /// a message, such as `app_home_opened`. The handler receives the whole event
    /// envelope; the event is `payload["event"]`.
    fn on_event(slf: &Bound<'_, Self>, event_type: Object) -> PyResult<Object> {
        Self::_decorator(slf, "event", event_type)
    }
    /// Handle `block_actions` whose `action_id` equals `action_id`, or where a
    /// compiled pattern's `search` matches. AgentChat's prompt buttons never reach it.
    fn on_action(slf: &Bound<'_, Self>, action_id: Object) -> PyResult<Object> {
        Self::_decorator(slf, "action", action_id)
    }
    /// Handle `view_submission` and `view_closed` for matching `callback_id`s. A
    /// submission handler's return value is the acknowledgement body, such as
    /// `{"response_action": "errors", "errors": {...}}`.
    fn on_view(slf: &Bound<'_, Self>, callback_id: Object) -> PyResult<Object> {
        Self::_decorator(slf, "view", callback_id)
    }
    /// Handle a slash command such as "/deploy"; the return value is the response.
    fn on_command(slf: &Bound<'_, Self>, command: Object) -> PyResult<Object> {
        Self::_decorator(slf, "command", command)
    }
    /// Handle every other payload no handler above matched, such as shortcuts and
    /// options requests.
    fn on_raw(slf: &Bound<'_, Self>, handler: Object) -> PyResult<Object> {
        let py = slf.py();
        slf.borrow()._register(py, "raw", py.None(), handler)
    }
    #[classmethod]
    #[pyo3(signature=(*,ack_emoji=Some("eyes")))]
    fn from_env(cls: &Bound<'_, PyType>, ack_emoji: Option<&str>) -> PyResult<Object> {
        let bot = env("SLACK_BOT_TOKEN", "")?;
        let app = env("SLACK_APP_TOKEN", "")?;
        if bot.is_empty() || app.is_empty() {
            return Err(PyRuntimeError::new_err(
                "SLACK_BOT_TOKEN and SLACK_APP_TOKEN are required",
            ));
        }
        let kwargs = PyDict::new(cls.py());
        kwargs.set_item("bot_token", bot)?;
        kwargs.set_item("app_token", app)?;
        kwargs.set_item("ack_emoji", ack_emoji)?;
        Ok(cls.call((), Some(&kwargs))?.unbind())
    }
    fn bind(&mut self, receiver: Object) {
        self._receiver = Some(receiver);
    }
    fn run(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        host::coroutine(py, async move {
            let web = Python::attach(|py| {
                let mut this = slf.borrow_mut(py);
                if this._receiver.is_none() {
                    return Err(PyRuntimeError::new_err(
                        "Slack channel is not bound to AgentChat",
                    ));
                }
                if this._run_started || this._close_task.is_some() {
                    return Err(PyRuntimeError::new_err(
                        "Slack channel has already been started or closed",
                    ));
                }
                this._run_started = true;
                Ok(this._web_client.clone_ref(py))
            })?;
            let result = async {
                let auth = task(call(&web, "auth_test", vec![], None)?)?;
                Python::attach(|py| slf.borrow_mut(py)._auth_task = Some(auth.clone_ref(py)));
                let identity = match host::await_py(auth).await {
                    Ok(identity) => identity,
                    Err(error) => {
                        let local_close =
                            Python::attach(|py| slf.borrow(py)._auth_cancelled_for_close);
                        let current = call(&module("asyncio")?, "current_task", vec![], None)?;
                        if cancelled(&error)
                            && local_close
                            && (is_none(&current)
                                || extract::<u64>(&call(&current, "cancelling", vec![], None)?)?
                                    == 0)
                        {
                            return Ok(none());
                        }
                        return Err(error);
                    }
                };
                let identity = value(&identity)?;
                let workspace = identity["team_id"].as_str().ok_or_else(|| {
                    PyRuntimeError::new_err("Slack did not return a bot identity")
                })?;
                let bot = identity["user_id"].as_str().ok_or_else(|| {
                    PyRuntimeError::new_err("Slack did not return a bot identity")
                })?;
                let source = Python::attach(|py| {
                    let mut this = slf.borrow_mut(py);
                    if this._workspace_id.as_ref().is_some_and(|s| s != workspace) {
                        return Err(PyRuntimeError::new_err(
                            "Slack token belongs to a different workspace",
                        ));
                    }
                    this._workspace_id = Some(workspace.into());
                    this._bot_user_id = Some(bot.into());
                    Ok(this
                        ._close_task
                        .is_none()
                        .then(|| this._event_source.clone_ref(py)))
                })?;
                if let Some(source) = source {
                    let receiver = Python::attach(|py| {
                        slf.bind(py).getattr("handle_event").map(Bound::unbind)
                    })?;
                    acall(&source, "run", vec![receiver], None).await?;
                }
                Ok(none())
            }
            .await;
            let close = acall(&slf.into_any(), "close", vec![], None).await;
            close?;
            result
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        host::coroutine(py, async move {
            let close = Python::attach(|py| -> PyResult<Object> {
                if let Some(close) = &slf.borrow(py)._close_task {
                    return Ok(close.clone_ref(py));
                }
                let close = task(slf.bind(py).call_method0("_close")?.unbind())?;
                slf.borrow_mut(py)._close_task = Some(close.clone_ref(py));
                Ok(close)
            })?;
            shield(close).await
        })
    }
    fn _close(slf: Py<Self>, py: Python<'_>) -> PyResult<Object> {
        host::coroutine(py, async move {
            let (auth, source) = Python::attach(|py| {
                let this = slf.borrow(py);
                (
                    this._auth_task.as_ref().map(|t| t.clone_ref(py)),
                    this._event_source.clone_ref(py),
                )
            });
            if let Some(auth) = auth {
                if !extract::<bool>(&call(&auth, "done", vec![], None)?)? {
                    Python::attach(|py| slf.borrow_mut(py)._auth_cancelled_for_close = true);
                    call(&auth, "cancel", vec![], None)?;
                    acall(
                        &module("asyncio")?,
                        "gather",
                        vec![auth],
                        Some(j(json!({"return_exceptions":true}))?),
                    )
                    .await?;
                }
            }
            let closed = acall(&source, "close", vec![], None).await;
            // Handlers that missed their acknowledgement deadline get the same drain time.
            let late = Python::attach(|py| -> PyResult<_> {
                let late = slf.borrow(py)._late.clone_ref(py);
                if late.bind(py).is_empty() {
                    return Ok(None);
                }
                let kwargs = PyDict::new(py);
                kwargs.set_item(
                    "timeout",
                    py.import("agentchat.channels.slack.socket_mode")?
                        .getattr("EVENT_DRAIN_TIMEOUT")?,
                )?;
                Ok(Some(
                    py.import("asyncio")?
                        .call_method(
                            "wait",
                            (PyTuple::new(py, late.bind(py).iter())?,),
                            Some(&kwargs),
                        )?
                        .unbind(),
                ))
            })?;
            if let Some(waiting) = late {
                let results = host::await_py(waiting).await?;
                Python::attach(|py| -> PyResult<()> {
                    for task in results.bind(py).get_item(1)?.try_iter()? {
                        task?.call_method0("cancel")?;
                    }
                    Ok(())
                })?;
            }
            closed
        })
    }
    fn is_connected(&self, py: Python<'_>) -> PyResult<Object> {
        let source = self._event_source.clone_ref(py);
        host::coroutine(py, async move {
            acall(&source, "is_connected", vec![], None).await
        })
    }
    fn get_user(&self, py: Python<'_>, user_id: String) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        host::coroutine(py, async move {
            let response = acall(
                &web,
                "users_info",
                vec![],
                Some(j(json!({"user":user_id}))?),
            )
            .await?;
            let user = agentchat_slack::policy::user_profile(&value(&response)?, &user_id)
                .map_err(policy_error)?;
            Python::attach(|py| {
                Ok(py
                    .import("agentchat.channels.slack.adapter")?
                    .getattr("SlackUser")?
                    .call1((
                        user.id,
                        user.team_id,
                        user.display_name,
                        user.email,
                        user.is_bot,
                        user.deleted,
                    ))?
                    .unbind())
            })
        })
    }
    #[pyo3(signature=(source,*,limit=50))]
    fn thread_history(&self, py: Python<'_>, source: Object, limit: usize) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let workspace = self._workspace_id.clone();
        let bot = self._bot_user_id.clone();
        host::coroutine(py, async move {
            let mut history =
                ThreadHistory::new(from_message(&source)?, bot, workspace.as_deref(), limit)
                    .map_err(policy_error)?;
            if !history.is_root() {
                loop {
                    let response = acall(
                        &web,
                        "conversations_replies",
                        vec![],
                        Some(j(history.request())?),
                    )
                    .await?;
                    if history.accept(&value(&response)?).map_err(policy_error)? {
                        break;
                    }
                }
            }
            Python::attach(|py| {
                Ok(PyTuple::new(
                    py,
                    history
                        .messages()
                        .iter()
                        .map(|m| convert::to_message(py, m))
                        .collect::<PyResult<Vec<_>>>()?,
                )?
                .into_any()
                .unbind())
            })
        })
    }
    fn subscribe(&self, py: Python<'_>, message: Object) -> PyResult<Object> {
        let threads = self._threads.clone_ref(py);
        host::coroutine(py, async move {
            let message = from_message(&message)?;
            if message.metadata.get("channel_type") != Some(&json!("im")) {
                acall(
                    &threads,
                    "add",
                    vec![j(json!(message.conversation_id.as_ref()))?],
                    None,
                )
                .await?;
            }
            Ok(none())
        })
    }
    /// Deliver an authenticated payload. Messages, prompt answers and stop requests
    /// go to AgentChat; everything else goes to the application's handlers. Returns
    /// the acknowledgement body for payloads that take one (view submissions,
    /// options requests and slash commands), waiting at most the acknowledgement
    /// deadline; otherwise None.
    fn handle_event(slf: Py<Self>, py: Python<'_>, payload: Object) -> PyResult<Object> {
        host::coroutine(py, async move {
            let data = value(&payload)?;
            let (workspace, bot, threads, receiver) = Python::attach(|py| {
                let this = slf.borrow(py);
                let receiver = this
                    ._receiver
                    .as_ref()
                    .ok_or_else(|| {
                        PyRuntimeError::new_err("Slack channel is not bound to AgentChat")
                    })?
                    .clone_ref(py);
                Ok::<_, PyErr>((
                    this._workspace_id.clone(),
                    this._bot_user_id.clone(),
                    this._threads.clone_ref(py),
                    receiver,
                ))
            })?;
            if !interaction::in_workspace(&data, workspace.as_deref()) {
                return Ok(none());
            }
            let channel = Python::attach(|py| slf.clone_ref(py).into_any());
            if data.get("event").is_some_and(|event| event.is_object()) {
                // Slack's native stop is a stop request, and the application sees it too.
                if let Some(stop) =
                    interaction::normalize_stop(&data, workspace.as_deref(), "slack")
                {
                    let stop = Python::attach(|py| convert::to_message(py, &stop))?;
                    let delivered = acall(
                        &receiver,
                        "__call__",
                        vec![clone_obj(&channel), clone_obj(&stop)],
                        None,
                    )
                    .await;
                    let cleared =
                        acall(&channel, "set_status", vec![stop, j(json!(""))?], None).await;
                    if cleared.is_err() {
                        log_warning(MODULE, "Could not clear the Slack status after a stop")?;
                    }
                    delivered?;
                    Self::dispatch(&slf, payload, false).await?;
                    return Ok(none());
                }
                let mut message =
                    events::normalize_scoped(&data, bot.as_deref(), workspace.as_deref(), "slack");
                if let Some(found) = &message {
                    if found.metadata.get("requires_subscription") == Some(&json!(true))
                        && !extract::<bool>(
                            &acall(
                                &threads,
                                "contains",
                                vec![j(json!(found.conversation_id.as_ref()))?],
                                None,
                            )
                            .await?,
                        )?
                    {
                        message = None;
                    }
                }
                let Some(message) = message else {
                    Self::dispatch(&slf, payload, false).await?;
                    return Ok(none());
                };
                let message = Python::attach(|py| convert::to_message(py, &message))?;
                acall(&channel, "_ack", vec![clone_obj(&message)], None).await?;
                acall(&receiver, "__call__", vec![channel, message], None).await?;
                return Ok(none());
            }
            let press =
                events::normalize_action(&data, bot.as_deref(), workspace.as_deref(), "slack");
            if let Some(press) = press {
                let press = Python::attach(|py| convert::to_message(py, &press))?;
                acall(&receiver, "__call__", vec![channel, press], None).await?;
                return Ok(none());
            }
            Self::dispatch(&slf, payload, interaction::responds(&data)).await
        })
    }
    fn _late_done(&self, py: Python<'_>, task: &Bound<'_, PyAny>) -> PyResult<()> {
        self._late.bind(py).discard(task)?;
        if !task.call_method0("cancelled")?.is_truthy()?
            && !task.call_method0("exception")?.is_none()
        {
            log_warning(
                MODULE,
                "A Slack handler failed after its acknowledgement deadline",
            )?;
        }
        Ok(())
    }
    fn _ack(&self, py: Python<'_>, message: Object) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let emoji = self._ack_emoji.clone();
        host::coroutine(py, async move {
            if let Some(emoji) = emoji.filter(|e| !e.is_empty()) {
                let message = from_message(&message)?;
                let payload = ack_request(
                    message
                        .metadata
                        .get("channel_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default(),
                    &emoji,
                    message
                        .metadata
                        .get("message_timestamp")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default(),
                );
                let result = acall(&web, "reactions_add", vec![], Some(j(payload)?)).await;
                if let Err(error) = result {
                    if !Python::attach(|py| error.is_instance_of::<PyException>(py)) {
                        return Err(error);
                    }
                }
            }
            Ok(none())
        })
    }
    fn set_status(&self, py: Python<'_>, source: Object, status: String) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let workspace = self._workspace_id.clone();
        let sessions = self._agent_sessions;
        host::coroutine(py, async move {
            let destination =
                ReplyDestination::from_source(&from_message(&source)?, workspace.as_deref())
                    .map_err(policy_error)?;
            let Some(thread) = destination.thread_ts else {
                return j(json!(false));
            };
            if sessions {
                let (method, payload) =
                    policy::status_request(&destination.channel_id, &thread, &status, true);
                slack_api(&web, method, payload).await?;
                return j(json!(true));
            }
            let response = acall(
                &web,
                "assistant_threads_setStatus",
                vec![],
                Some(j(
                    json!({"channel_id":destination.channel_id,"thread_ts":thread,"status":status}),
                )?),
            )
            .await?;
            if value(&response)?["ok"] != true {
                return Err(PyRuntimeError::new_err(
                    "Slack working status could not be confirmed",
                ));
            }
            j(json!(true))
        })
    }
    /// Post a notice in the source conversation that only `user_id` can see. It is
    /// never posted publicly instead.
    #[pyo3(signature=(source,user_id,text,*,blocks=None))]
    fn send_ephemeral(
        &self,
        py: Python<'_>,
        source: Object,
        user_id: String,
        text: String,
        blocks: Option<Object>,
    ) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let message = from_message(&source)?;
        let destination = self.destination(&message, true)?;
        let content = Python::attach(|py| -> PyResult<_> {
            let reply = PyDict::new(py);
            reply.set_item("text", &text)?;
            if let Some(blocks) = &blocks {
                reply.set_item("blocks", blocks)?;
            }
            convert::from_rich_reply(
                py.import("agentchat.models")?
                    .getattr("RichReply")?
                    .call((), Some(&reply))?
                    .as_any(),
            )
        })?;
        let request = interaction::ephemeral_request(
            &destination.channel_id,
            destination.thread_ts.as_deref(),
            &user_id,
            &content,
        )
        .map_err(policy_error)?;
        host::coroutine(py, async move {
            let response = sdk_call(&web, "chat_postEphemeral", request).await?;
            let sent = interaction::ephemeral_message(
                &message,
                &destination.channel_id,
                destination.thread_ts.as_deref(),
                &user_id,
                &text,
                &response,
            )
            .map_err(policy_error)?;
            Python::attach(|py| convert::to_message(py, &sent))
        })
    }
    /// Delete a message this app sent (`chat.delete`).
    fn delete(&self, py: Python<'_>, message: Object) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let request =
            interaction::delete_request(&from_message(&message)?, self._workspace_id.as_deref())
                .map_err(policy_error)?;
        host::coroutine(py, async move {
            sdk_call(&web, "chat_delete", request).await?;
            Ok(none())
        })
    }
    /// Open a modal with an interaction's `trigger_id`, which expires three seconds
    /// after Slack sends it. Returns the opened view.
    fn open_view(
        &self,
        py: Python<'_>,
        trigger_id: &str,
        view: &Bound<'_, PyAny>,
    ) -> PyResult<Object> {
        let request = interaction::open_view_request(trigger_id, &convert::to_json(view)?)
            .map_err(policy_error)?;
        self.view_call(py, "views_open", request)
    }
    /// Push a view onto an open modal with the `trigger_id` of an interaction in it.
    fn push_view(
        &self,
        py: Python<'_>,
        trigger_id: &str,
        view: &Bound<'_, PyAny>,
    ) -> PyResult<Object> {
        let request = interaction::open_view_request(trigger_id, &convert::to_json(view)?)
            .map_err(policy_error)?;
        self.view_call(py, "views_push", request)
    }
    /// Replace a view named by `view_id` or `external_id`; `hash` guards against races.
    #[pyo3(signature=(view,*,view_id=None,external_id=None,hash=None))]
    fn update_view(
        &self,
        py: Python<'_>,
        view: &Bound<'_, PyAny>,
        view_id: Option<String>,
        external_id: Option<String>,
        hash: Option<String>,
    ) -> PyResult<Object> {
        let target = match (view_id, external_id) {
            (Some(id), None) => ViewTarget::Id(id),
            (None, Some(id)) => ViewTarget::External(id),
            _ => {
                return Err(PyValueError::new_err(
                    "Pass exactly one of view_id or external_id",
                ))
            }
        };
        let request =
            interaction::update_view_request(&convert::to_json(view)?, &target, hash.as_deref())
                .map_err(policy_error)?;
        self.view_call(py, "views_update", request)
    }
    /// Publish a user's App Home tab.
    #[pyo3(signature=(user_id,view,*,hash=None))]
    fn publish_home(
        &self,
        py: Python<'_>,
        user_id: &str,
        view: &Bound<'_, PyAny>,
        hash: Option<String>,
    ) -> PyResult<Object> {
        let request =
            interaction::publish_home_request(user_id, &convert::to_json(view)?, hash.as_deref())
                .map_err(policy_error)?;
        self.view_call(py, "views_publish", request)
    }
    /// Show task and plan updates with a streamed answer. A stream started with them
    /// uses Slack's chunks; if Slack refuses them, the answer streams as plain text.
    fn update_stream_tasks(
        &self,
        py: Python<'_>,
        source: Object,
        stream: Object,
        updates: &Bound<'_, PyAny>,
    ) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let message = from_message(&source)?;
        let destination = self.destination(&message, true)?;
        let streams = web.bind(py).hasattr("api_call")?;
        let updates: Vec<agentchat_core::StreamUpdate> =
            serde_json::from_value(convert::to_json(updates)?)
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let handle = (!stream.is_none(py)).then(|| value(&stream)).transpose()?;
        host::coroutine(py, async move {
            let Some(handle) = handle else {
                let Some(thread) = destination.thread_ts.filter(|_| streams) else {
                    return j(json!({"post": true}));
                };
                let request = policy::stream_start_tasks_request(
                    &destination.channel_id,
                    &thread,
                    &message,
                    &updates,
                );
                return match slack_attempt(&web, "chat.startStream", request).await? {
                    Ok(response) => {
                        MessageReceipt::confirmed_timestamp(&response, DeliveryKind::Stream)
                            .map_err(policy_error)?;
                        j(policy::chunk_stream_handle(
                            &destination.channel_id,
                            &response["ts"],
                        ))
                    }
                    // Nothing was started; the answer starts as plain text with its first text.
                    Err(code) if policy::retry_without_chunks(&code) => {
                        log_warning(MODULE, "Slack refused task updates; streaming plain text")?;
                        j(json!({"defer": true}))
                    }
                    Err(code) => Err(PyRuntimeError::new_err(format!(
                        "Slack chat.startStream could not be confirmed ({code})"
                    ))),
                };
            };
            let Some(request) = policy::stream_tasks_request(&handle, &updates) else {
                return Ok(stream);
            };
            // Task updates are secondary: a refused update never fails the answer.
            match slack_attempt(&web, "chat.appendStream", request).await {
                Ok(Ok(_)) => Ok(stream),
                _ => {
                    log_warning(
                        MODULE,
                        "Slack refused task updates; the answer continues without them",
                    )?;
                    j(policy::without_tasks(&handle))
                }
            }
        })
    }
    fn _delivery_destination(
        &self,
        source: &Bound<'_, PyAny>,
    ) -> PyResult<(String, Option<String>)> {
        let destination = self.destination(&convert::from_message(source)?, true)?;
        Ok((destination.channel_id, destination.thread_ts))
    }
    fn upload_files(
        &self,
        py: Python<'_>,
        source: &Bound<'_, PyAny>,
        files: &Bound<'_, PyAny>,
    ) -> PyResult<Object> {
        let destination = self.destination(&convert::from_message(source)?, true)?;
        let files = slack_files::files_from_python(files)?;
        host::coroutine(
            py,
            slack_files::upload(
                files,
                destination.channel_id,
                destination.thread_ts,
                PythonFiles {
                    request: self._web_client.clone_ref(py),
                    guard: None,
                    sdk: true,
                },
            ),
        )
    }
    #[pyo3(signature=(source,attachment,*,max_bytes=None))]
    fn download_attachment(
        &self,
        py: Python<'_>,
        source: &Bound<'_, PyAny>,
        attachment: &Bound<'_, PyAny>,
        max_bytes: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Object> {
        let source = convert::from_message(source)?;
        let attachment = convert::from_attachment(attachment)?;
        if source.channel != "slack"
            || source.role != Role::User
            || self
                ._workspace_id
                .as_ref()
                .is_some_and(|id| source.metadata.get("team_id") != Some(&json!(id)))
        {
            return Err(PyValueError::new_err(
                "Expected a source in the configured Slack workspace",
            ));
        }
        if !source.attachments.contains(&attachment) {
            return Err(PyValueError::new_err(
                "Attachment does not belong to the source message",
            ));
        }
        let max_bytes = max_bytes
            .map(slack_files::byte_limit)
            .transpose()?
            .unwrap_or(10 * 1024 * 1024);
        host::coroutine(
            py,
            slack_files::download(
                attachment,
                self._bot_token.clone(),
                max_bytes,
                PythonFiles {
                    request: self._web_client.clone_ref(py),
                    guard: None,
                    sdk: true,
                },
            ),
        )
    }
    /// Show a prompt in the source thread: its text and one button per choice.
    fn send_prompt(&self, py: Python<'_>, source: Object, prompt: Object) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let destination = self.destination(&from_message(&source)?, true)?;
        let fields = PromptFields::read(prompt.bind(py))?;
        host::coroutine(py, async move {
            let blocks = rendering::prompt_blocks(&fields.text, &fields.id, &fields.choices)
                .map_err(policy_error)?;
            let payload = json!({
                "channel": destination.channel_id,
                "thread_ts": destination.thread_ts,
                "text": fields.text,
                "blocks": blocks,
                "unfurl_links": false,
                "unfurl_media": false,
            });
            let response = acall(&web, "chat_postMessage", vec![], Some(j(payload)?)).await?;
            let receipt = MessageReceipt::confirmed(&value(&response)?, DeliveryKind::RichReply)
                .map_err(policy_error)?;
            let message = policy::reply_message(
                &from_message(&source)?,
                ReplyMessageRoute::validated(&destination),
                fields.text,
                receipt,
                None,
            );
            Python::attach(|py| convert::to_message(py, &message))
        })
    }
    /// Replace the buttons with the choice and who made it, or remove them on expiry.
    fn close_prompt(
        &self,
        py: Python<'_>,
        source: Object,
        sent: Object,
        prompt: Object,
        answer: Object,
    ) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let destination = self.destination(&from_message(&source)?, true)?;
        let fields = PromptFields::read(prompt.bind(py))?;
        let text = fields.closed_text(answer.bind(py))?.unwrap_or(fields.text);
        let ts = from_message(&sent)?.metadata["message_timestamp"].clone();
        host::coroutine(py, async move {
            let payload = json!({
                "channel": destination.channel_id,
                "ts": ts,
                "text": text,
                "blocks": rendering::text_blocks(&text).map_err(policy_error)?,
            });
            let response = acall(&web, "chat_update", vec![], Some(j(payload)?)).await?;
            if value(&response)?["ok"] != true {
                return Err(PyRuntimeError::new_err("Slack could not close the prompt"));
            }
            Ok(none())
        })
    }
    /// Slack accepts a few stream updates per second; one a second leaves headroom.
    #[classattr]
    fn stream_interval() -> f64 {
        1.0
    }
    /// A Slack stream holds at most 12,000 characters; longer answers continue.
    #[classattr]
    fn stream_limit() -> usize {
        policy::STREAM_LIMIT
    }
    /// Start a native Slack stream in the source thread. Outside a thread, or with a
    /// web client that has no `api_call`, the answer is posted once when it stops.
    fn start_stream(&self, py: Python<'_>, source: Object, text: String) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let destination = self.destination(&from_message(&source)?, true)?;
        let streams = web.bind(py).hasattr("api_call")?;
        host::coroutine(py, async move {
            let message = from_message(&source)?;
            let Some(thread) = destination.thread_ts.filter(|_| streams) else {
                return j(json!({"post": true}));
            };
            let payload =
                policy::stream_start_request(&destination.channel_id, &thread, &text, &message);
            let response = slack_api(&web, "chat.startStream", payload).await?;
            MessageReceipt::confirmed_timestamp(&response, DeliveryKind::Stream)
                .map_err(policy_error)?;
            j(json!({"channel": destination.channel_id, "ts": response["ts"], "sent": text}))
        })
    }
    fn append_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        stream: Object,
        text: String,
    ) -> PyResult<Object> {
        let web = slf.borrow(py)._web_client.clone_ref(py);
        slf.borrow(py).destination(&from_message(&source)?, true)?;
        host::coroutine(py, async move {
            let handle = value(&stream)?;
            if handle["post"] == true {
                return Ok(stream);
            }
            if handle["defer"] == true {
                if text.is_empty() {
                    return Ok(stream);
                }
                let text = j(json!(text))?;
                return acall(&slf.into_any(), "start_stream", vec![source, text], None).await;
            }
            let (payload, next) =
                policy::stream_request(&handle, &text, false).map_err(policy_error)?;
            if let Some(payload) = payload {
                slack_api(&web, "chat.appendStream", payload).await?;
            }
            j(next)
        })
    }
    fn stop_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Object,
        stream: Object,
        text: String,
    ) -> PyResult<Object> {
        let this = slf.borrow(py);
        let web = this._web_client.clone_ref(py);
        let destination = this.destination(&from_message(&source)?, true)?;
        drop(this);
        host::coroutine(py, async move {
            let handle = value(&stream)?;
            if handle["post"] == true || handle["defer"] == true {
                let text = j(json!(text))?;
                return acall(&slf.into_any(), "reply", vec![source, text], None).await;
            }
            let (payload, _) =
                policy::stream_request(&handle, &text, true).map_err(policy_error)?;
            if let Some(payload) = payload {
                slack_api(&web, "chat.stopStream", payload).await?;
            }
            let receipt = MessageReceipt::confirmed_timestamp(
                &json!({"ok": true, "ts": handle["ts"]}),
                DeliveryKind::Stream,
            )
            .map_err(policy_error)?;
            let message = policy::reply_message(
                &from_message(&source)?,
                ReplyMessageRoute::validated(&destination),
                text,
                receipt,
                None,
            );
            Python::attach(|py| convert::to_message(py, &message))
        })
    }
    fn reply(&self, py: Python<'_>, source: Object, content: String) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        host::coroutine(py, async move {
            let source = from_message(&source)?;
            let payload = json!({
                "channel": source.metadata.get("channel_id"),
                "thread_ts": source.metadata.get("reply_thread_timestamp"),
                "text": content,
                "blocks": rendering::text_blocks(&content).map_err(policy_error)?,
                "unfurl_links": false,
                "unfurl_media": false,
            });
            let response = acall(&web, "chat_postMessage", vec![], Some(j(payload)?)).await?;
            let response = value(&response)?;
            let receipt = MessageReceipt::compatibility_text(&response);
            let message = policy::reply_message(
                &source,
                ReplyMessageRoute::compatibility_source(&source),
                content,
                receipt,
                None,
            );
            Python::attach(|py| convert::to_message(py, &message))
        })
    }
    fn reply_rich(&self, py: Python<'_>, source: Object, content: Object) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        let workspace = self._workspace_id.clone();
        host::coroutine(py, async move {
            let source = from_message(&source)?;
            let destination = ReplyDestination::from_source(&source, workspace.as_deref())
                .map_err(policy_error)?;
            ReplyDestination::validate(&destination.channel_id, destination.thread_ts.as_deref())
                .map_err(policy_error)?;
            let content = Python::attach(|py| convert::from_rich_reply(content.bind(py)))?;
            let mut payload = rendering::rich_payload(&content).map_err(policy_error)?;
            payload["channel"] = json!(destination.channel_id);
            payload["thread_ts"] = json!(destination.thread_ts);
            let response = acall(&web, "chat_postMessage", vec![], Some(j(payload)?)).await?;
            let response = value(&response)?;
            let receipt = MessageReceipt::confirmed(&response, DeliveryKind::RichReply)
                .map_err(policy_error)?;
            let message = policy::reply_message(
                &source,
                ReplyMessageRoute::validated(&destination),
                content.text,
                receipt,
                None,
            );
            Python::attach(|py| convert::to_message(py, &message))
        })
    }
    #[pyo3(signature=(source,message,*,origin="web"))]
    fn mirror(
        &self,
        py: Python<'_>,
        source: Object,
        message: Object,
        origin: &str,
    ) -> PyResult<Object> {
        let origin = origin.to_owned();
        let workspace = self._workspace_id.clone();
        let web = self._web_client.clone_ref(py);
        host::coroutine(py, async move {
            let source = from_message(&source)?;
            let destination = ReplyDestination::from_source(&source, workspace.as_deref())
                .map_err(policy_error)?;
            let message = from_message(&message)?;
            let mut payload = rendering::mirror_payload(&message, &origin).map_err(policy_error)?;
            payload["channel"] = json!(source.metadata.get("channel_id"));
            payload["thread_ts"] = json!(source.metadata.get("reply_thread_timestamp"));
            let response = acall(&web, "chat_postMessage", vec![], Some(j(payload)?)).await?;
            let response = value(&response)?;
            let receipt =
                MessageReceipt::confirmed(&response, DeliveryKind::Mirror).map_err(policy_error)?;
            let reply = policy::reply_message(
                &source,
                ReplyMessageRoute::validated(&destination),
                message.text.clone(),
                receipt,
                Some(MirroredReply {
                    message: &message,
                    origin: &origin,
                }),
            );
            Python::attach(|py| convert::to_message(py, &reply))
        })
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        // CPython can traverse during eager dictionary allocation, before PyO3
        // installs the initialized payload. None is the valid zero state.
        if let Some(state) = &self.state {
            visit.call(&state._web_client)?;
            visit.call(&state._event_source)?;
            visit.call(&state._receiver)?;
            visit.call(&state._auth_task)?;
            visit.call(&state._close_task)?;
            visit.call(&state._threads)?;
            visit.call(&state._routes)?;
            visit.call(&state._late)?;
        }
        Ok(())
    }
    fn __clear__(&mut self) {
        self.state = None;
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::host::register_initialized_class::<Slack>(module)?;
    crate::host::register_initialized_class::<MemoryThreadSubscriptions>(module)?;
    Ok(())
}

impl Slack {
    fn view_call(
        &self,
        py: Python<'_>,
        method: &'static str,
        request: serde_json::Value,
    ) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        host::coroutine(py, async move {
            let response = sdk_call(&web, method, request).await?;
            let view = interaction::view_of(&response).map_err(policy_error)?;
            j(view)
        })
    }
    /// Run the handler for an application payload. With `deadline`, wait at most
    /// the acknowledgement deadline for its answer; a slower handler keeps running.
    async fn dispatch(slf: &Py<Self>, payload: Object, deadline: bool) -> PyResult<Object> {
        let route = interaction::route(&value(&payload)?);
        let pending = Python::attach(|py| -> PyResult<Option<Object>> {
            let Some(handler) = slf.borrow(py).find(py, &route)? else {
                return Ok(None);
            };
            let pending = handler.bind(py).call1((payload.bind(py),))?;
            if !deadline {
                return Ok(Some(pending.unbind()));
            }
            Ok(Some(
                py.import("asyncio")?
                    .call_method1("ensure_future", (pending,))?
                    .unbind(),
            ))
        })?;
        let Some(pending) = pending else {
            return Ok(none());
        };
        if !deadline {
            return host::await_py(pending).await;
        }
        let waiting = Python::attach(|py| -> PyResult<Object> {
            let kwargs = PyDict::new(py);
            kwargs.set_item("timeout", interaction::ACK_DEADLINE.as_secs_f64())?;
            Ok(py
                .import("asyncio")?
                .call_method("wait", ((pending.bind(py),),), Some(&kwargs))?
                .unbind())
        })?;
        host::await_py(waiting).await?;
        Python::attach(|py| {
            let task = pending.bind(py);
            if task.call_method0("done")?.is_truthy()? {
                return Ok(task.call_method0("result")?.unbind());
            }
            log_warning(MODULE, "Slack handler missed the acknowledgement deadline")?;
            let this = slf.borrow(py);
            this._late.bind(py).add(task)?;
            task.call_method1("add_done_callback", (slf.bind(py).getattr("_late_done")?,))?;
            Ok(py.None())
        })
    }
    /// The first handler registered for `route`, else the raw handler.
    fn find(&self, py: Python<'_>, route: &Route) -> PyResult<Option<Object>> {
        let (kind, keys): (&str, Vec<&str>) = match route {
            Route::Event { event_type } => ("event", vec![event_type.as_str()]),
            Route::Action { action_ids } => {
                ("action", action_ids.iter().map(String::as_str).collect())
            }
            Route::View { callback_id } => ("view", vec![callback_id.as_str()]),
            Route::Command { command } => ("command", vec![command.as_str()]),
            Route::Other => ("other", vec![]),
        };
        let mut raw = None;
        for entry in self._routes.bind(py).iter() {
            let (entry_kind, key, handler): (String, Bound<'_, PyAny>, Bound<'_, PyAny>) =
                entry.extract()?;
            if entry_kind == "raw" {
                raw = Some(handler.unbind());
                continue;
            }
            if entry_kind != kind {
                continue;
            }
            for value in &keys {
                let matched = match key.extract::<String>() {
                    Ok(exact) => exact == *value,
                    Err(_) => !key.call_method1("search", (*value,))?.is_none(),
                };
                if matched {
                    return Ok(Some(handler.unbind()));
                }
            }
        }
        Ok(raw)
    }
    #[allow(clippy::too_many_arguments)]
    fn build(
        py: Python<'_>,
        bot_token: String,
        app_token: Option<String>,
        event_source: Option<Object>,
        web_client: Option<Object>,
        ack_emoji: Option<&str>,
        workspace_id: Option<String>,
        bot_user_id: Option<String>,
        thread_subscriptions: Option<Object>,
        max_pending_events: i64,
        agent_sessions: bool,
    ) -> PyResult<Self> {
        if app_token.is_none() == event_source.is_none() {
            return Err(PyValueError::new_err(
                "Provide exactly one of app_token or event_source",
            ));
        }
        if max_pending_events < 1 {
            return Err(PyValueError::new_err("max_pending_events must be positive"));
        }
        let web_client = match web_client {
            Some(value) => value,
            None => call(
                &module("agentchat.channels.slack.adapter")?,
                "AsyncWebClient",
                vec![],
                Some(j(json!({"token":bot_token,"retry_handlers":[]}))?),
            )?,
        };
        let event_source = match event_source {
            Some(value) => value,
            None => {
                let kwargs =
                    j(json!({"app_token":app_token,"max_pending_events":max_pending_events}))?;
                kwargs
                    .bind(py)
                    .set_item("web_client", web_client.bind(py))?;
                call(
                    &module("agentchat.channels.slack.socket_mode")?,
                    "SocketMode",
                    vec![],
                    Some(kwargs),
                )?
            }
        };
        Ok(Self {
            state: Some(Box::new(SlackState {
                _bot_token: bot_token,
                _web_client: web_client,
                _event_source: event_source,
                _receiver: None,
                _run_started: false,
                _auth_task: None,
                _auth_cancelled_for_close: false,
                _close_task: None,
                _ack_emoji: ack_emoji.map(str::to_owned),
                _workspace_id: workspace_id,
                _bot_user_id: bot_user_id,
                _threads: match thread_subscriptions {
                    Some(value) => value,
                    None => Py::new(py, MemoryThreadSubscriptions::build())?.into_any(),
                },
                _agent_sessions: agent_sessions,
                _routes: PyList::empty(py).unbind(),
                _late: PySet::empty(py)?.unbind(),
            })),
        })
    }
}
