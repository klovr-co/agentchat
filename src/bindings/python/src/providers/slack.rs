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
    policy::{
        self, DeliveryKind, MessageReceipt, MirroredReply, ReplyDestination, ReplyMessageRoute,
        ThreadHistory,
    },
    rendering,
};
use pyo3::{
    exceptions::{PyException, PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PyTuple, PyType},
    PyTraverseError, PyVisit,
};
use serde_json::json;
type Object = Py<PyAny>;
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
    #[pyo3(signature=(*,bot_token,app_token=None,event_source=None,web_client=None,ack_emoji=Some("eyes"),workspace_id=None,bot_user_id=None,thread_subscriptions=None,max_pending_events=64))]
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
        )?;
        Ok(())
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
            acall(&source, "close", vec![], None).await
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
    fn handle_event(slf: Py<Self>, py: Python<'_>, payload: Object) -> PyResult<Object> {
        host::coroutine(py, async move {
            let payload = value(&payload)?;
            let (message, threads, receiver) = Python::attach(|py| {
                let this = slf.borrow(py);
                let receiver = this
                    ._receiver
                    .as_ref()
                    .ok_or_else(|| {
                        PyRuntimeError::new_err("Slack channel is not bound to AgentChat")
                    })?
                    .clone_ref(py);
                Ok::<_, PyErr>((
                    events::normalize_scoped(
                        &payload,
                        this._bot_user_id.as_deref(),
                        this._workspace_id.as_deref(),
                        "slack",
                    ),
                    this._threads.clone_ref(py),
                    receiver,
                ))
            })?;
            if message.is_none() {
                let press = Python::attach(|py| {
                    let this = slf.borrow(py);
                    events::normalize_action(
                        &payload,
                        this._bot_user_id.as_deref(),
                        this._workspace_id.as_deref(),
                        "slack",
                    )
                });
                if let Some(press) = press {
                    let press = Python::attach(|py| convert::to_message(py, &press))?;
                    acall(&receiver, "__call__", vec![slf.into_any(), press], None).await?;
                }
                return Ok(none());
            }
            if let Some(message) = message {
                if message.metadata.get("requires_subscription") == Some(&json!(true))
                    && !extract::<bool>(
                        &acall(
                            &threads,
                            "contains",
                            vec![j(json!(message.conversation_id.as_ref()))?],
                            None,
                        )
                        .await?,
                    )?
                {
                    return Ok(none());
                }
                let message = Python::attach(|py| convert::to_message(py, &message))?;
                let channel = slf.into_any();
                acall(&channel, "_ack", vec![clone_obj(&message)], None).await?;
                acall(&receiver, "__call__", vec![channel, message], None).await?;
            }
            Ok(none())
        })
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
        host::coroutine(py, async move {
            let destination =
                ReplyDestination::from_source(&from_message(&source)?, workspace.as_deref())
                    .map_err(policy_error)?;
            let Some(thread) = destination.thread_ts else {
                return j(json!(false));
            };
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
        &self,
        py: Python<'_>,
        source: Object,
        stream: Object,
        text: String,
    ) -> PyResult<Object> {
        let web = self._web_client.clone_ref(py);
        self.destination(&from_message(&source)?, true)?;
        host::coroutine(py, async move {
            let handle = value(&stream)?;
            if handle["post"] == true {
                return Ok(stream);
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
            if handle["post"] == true {
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
            })),
        })
    }
}
