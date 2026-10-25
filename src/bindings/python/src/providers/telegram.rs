//! Telegram Python interface: foreign async effects driven by native policy.
#[path = "telegram_transport.rs"]
mod transport;
use super::transport::*;
use crate::prompts::PromptFields;
use crate::{apps, convert, host};
use agentchat_core::{Route, TaskBoard};
use agentchat_providers::telegram as policy;
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyBytes, PyDict, PySet, PyTuple},
    PyTraverseError, PyVisit,
};
use serde_json::{json, Value};

#[pyfunction]
#[pyo3(signature=(update,*,bot_id,bot_username))]
fn telegram_normalize(
    py: Python<'_>,
    update: &Bound<'_, PyAny>,
    bot_id: i64,
    bot_username: &str,
) -> PyResult<Py<PyAny>> {
    policy::normalize(&convert::to_json(update)?, bot_id, bot_username)
        .map(|m| convert::to_message(py, &m))
        .unwrap_or_else(|| Ok(py.None()))
}
#[pyfunction]
fn telegram_render(py: Python<'_>, markdown: &str) -> PyResult<Py<PyAny>> {
    let rendered = policy::render(markdown);
    let entities = convert::from_json(py, &json!(rendered.entities))?;
    Ok(py
        .import("agentchat.channels.telegram.rendering")?
        .getattr("RenderedText")?
        .call1((rendered.text, entities))?
        .unbind())
}
#[pyfunction]
fn telegram_text_payload(py: Python<'_>, content: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &policy::text_payload(content.extract::<String>().ok().as_deref())
            .map_err(PyValueError::new_err)?,
    )
}
#[pyfunction]
fn telegram_truncate_caption(title: &str) -> String {
    policy::truncate_caption(title)
}
#[pyfunction]
#[pyo3(signature=(*,attempt,retry_after))]
fn telegram_retry_delay(attempt: u32, retry_after: Option<i64>) -> PyResult<f64> {
    Ok(policy::retry_delay(
        attempt,
        retry_after,
        constant("RETRY_BASE", 1.)?,
        constant("RETRY_MAX", 30.)?,
    ))
}
fn default_download_limit() -> Py<PyAny> {
    Python::attach(|py| 10485760usize.into_pyobject(py).unwrap().into_any().unbind())
}
fn constant(name: &str, default: f64) -> PyResult<f64> {
    let adapter = module("agentchat.channels.telegram.adapter")?;
    Ok(get(&adapter, name)
        .and_then(|v| extract::<f64>(&v))
        .unwrap_or(default))
}
fn telegram_error(error: &PyErr) -> bool {
    Python::attach(|py| {
        py.import("agentchat.channels.telegram.transport")
            .and_then(|v| v.getattr("TelegramError"))
            .is_ok_and(|class| error.is_instance(py, &class))
    })
}
fn error_field(error: &PyErr, name: &str) -> Option<i64> {
    Python::attach(|py| error.value(py).getattr(name).ok()?.extract().ok()?)
}
fn transient(error: &PyErr) -> bool {
    policy::ApiError {
        description: String::new(),
        error_code: error_field(error, "error_code"),
        retry_after: None,
    }
    .is_transient()
}
fn spawn(this: &Py<PyAny>, method: &str, args: Vec<Py<PyAny>>) -> PyResult<Py<PyAny>> {
    call(
        &module("asyncio")?,
        "create_task",
        vec![call(this, method, args, None)?],
        None,
    )
}
fn items_of(obj: &Py<PyAny>) -> PyResult<Vec<Py<PyAny>>> {
    Python::attach(|py| {
        obj.bind(py)
            .try_iter()?
            .map(|v| v.map(Bound::unbind))
            .collect()
    })
}
fn task_set(tasks: Vec<Py<PyAny>>) -> PyResult<Py<PyAny>> {
    Python::attach(|py| Ok(PySet::new(py, tasks)?.into_any().unbind()))
}
async fn wait_task(task: Py<PyAny>) -> PyResult<()> {
    acall(
        &module("asyncio")?,
        "wait",
        vec![task_set(vec![task])?],
        None,
    )
    .await?;
    Ok(())
}

// The object dictionary is a compatibility view for injected clients and task inspection.
// No provider interpretation or Python source code is stored in this view.
#[pyclass(
    module = "agentchat.channels.telegram.adapter",
    subclass,
    weakref,
    dict
)]
pub(crate) struct Telegram {
    // CPython may invoke GC traversal on zero-initialized storage before __new__.
    payload: Option<Box<TelegramState>>,
}
pub(crate) struct TelegramState {
    state: Py<PyDict>,
}
impl std::ops::Deref for Telegram {
    type Target = TelegramState;
    fn deref(&self) -> &Self::Target {
        self.payload
            .as_deref()
            .expect("methods require a successfully initialized native adapter")
    }
}
#[pymethods]
impl Telegram {
    #[new]
    #[pyo3(signature=(*_args,**_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self { payload: None }
    }
    #[pyo3(signature=(*,bot_token,webhook_url=None,webhook_secret=None,host="127.0.0.1",port=3980,client=None,max_pending_events=64,shutdown_timeout=20.0,stream_drafts=false))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        bot_token: &str,
        webhook_url: Option<&str>,
        webhook_secret: Option<&str>,
        host: &str,
        port: i64,
        client: Option<Py<PyAny>>,
        max_pending_events: i64,
        shutdown_timeout: f64,
        stream_drafts: bool,
    ) -> PyResult<()> {
        let bot = policy::validate_config(bot_token, webhook_url, webhook_secret)
            .map_err(PyValueError::new_err)?;
        let state = slf.bind(py).getattr("__dict__")?.cast_into::<PyDict>()?;
        state.set_item("_bot_id", bot)?;
        state.set_item("_username", py.None())?;
        state.set_item("_first_name", py.None())?;
        state.set_item("_webhook_url", webhook_url)?;
        state.set_item("_webhook_secret", webhook_secret)?;
        state.set_item("_host", host)?;
        state.set_item("_port", port)?;
        let client = match client {
            Some(v) => v,
            None => py
                .import("agentchat.channels.telegram.transport")?
                .getattr("TelegramBotClient")?
                .call1((bot_token,))?
                .unbind(),
        };
        state.set_item("_client", client)?;
        state.set_item(
            "_delivery",
            py.import("agentchat._delivery")?
                .getattr("DeliveryTasks")?
                .call1((max_pending_events, shutdown_timeout))?,
        )?;
        for name in ["_offset", "_confirmed_offset", "_worker", "_runner"] {
            state.set_item(name, py.None())?;
        }
        state.set_item("_typing", PyDict::new(py))?;
        state.set_item("_answers", PySet::empty(py)?)?;
        state.set_item(
            "_close_lock",
            py.import("asyncio")?.getattr("Lock")?.call0()?,
        )?;
        state.set_item("_closed", false)?;
        state.set_item("_stream_drafts", stream_drafts)?;
        slf.borrow_mut(py).payload = Some(Box::new(TelegramState {
            state: state.unbind(),
        }));
        Ok(())
    }
    #[classattr]
    fn name() -> &'static str {
        "telegram"
    }
    /// Handle updates AgentChat does not turn into messages, by kind, such as
    /// "my_chat_member" or "edited_message". Telegram sends those kinds only once a
    /// handler for them is registered before `run()`.
    fn on_event(slf: &Bound<'_, Self>, event_type: Py<PyAny>) -> PyResult<Py<PyAny>> {
        apps::decorator(slf.as_any(), "event", event_type)
    }
    /// Handle presses on inline buttons whose `callback_data` matches. The returned
    /// mapping answers the press: `text`, `show_alert`, `url`, `cache_time`.
    fn on_action(slf: &Bound<'_, Self>, action_id: Py<PyAny>) -> PyResult<Py<PyAny>> {
        apps::decorator(slf.as_any(), "action", action_id)
    }
    /// Telegram has no forms; registered view handlers never run.
    fn on_view(slf: &Bound<'_, Self>, callback_id: Py<PyAny>) -> PyResult<Py<PyAny>> {
        apps::decorator(slf.as_any(), "view", callback_id)
    }
    /// Handle messages that start with this command, such as "/start", instead
    /// of passing them to the message handler.
    fn on_command(slf: &Bound<'_, Self>, command: Py<PyAny>) -> PyResult<Py<PyAny>> {
        apps::decorator(slf.as_any(), "command", command)
    }
    /// Handle every routed update no other handler matched.
    fn on_raw(slf: &Bound<'_, Self>, handler: Py<PyAny>) -> PyResult<Py<PyAny>> {
        apps::raw(slf.as_any(), handler)
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Some(payload) = self.payload.as_deref() {
            visit.call(&payload.state)?;
        }
        Ok(())
    }
    fn __clear__(&mut self) {
        self.payload = None;
    }
    #[classmethod]
    fn from_env(cls: &Bound<'_, pyo3::types::PyType>) -> PyResult<Py<PyAny>> {
        let py = cls.py();
        let token = env("TELEGRAM_BOT_TOKEN", "")?;
        if token.is_empty() {
            return Err(PyRuntimeError::new_err("TELEGRAM_BOT_TOKEN is required"));
        }
        let url = env("TELEGRAM_WEBHOOK_URL", "")?;
        let secret = env("TELEGRAM_WEBHOOK_SECRET", "")?;
        if url.is_empty() != secret.is_empty() {
            return Err(PyRuntimeError::new_err(
                "TELEGRAM_WEBHOOK_URL and TELEGRAM_WEBHOOK_SECRET are required together",
            ));
        }
        let kw = PyDict::new(py);
        kw.set_item("bot_token", token)?;
        kw.set_item("webhook_url", (!url.is_empty()).then_some(url))?;
        kw.set_item("webhook_secret", (!secret.is_empty()).then_some(secret))?;
        kw.set_item("host", env("TELEGRAM_HOST", "127.0.0.1")?)?;
        kw.set_item(
            "port",
            env("TELEGRAM_PORT", "3980")?
                .parse::<i64>()
                .map_err(|e| PyValueError::new_err(e.to_string()))?,
        )?;
        Ok(cls.call((), Some(&kw))?.unbind())
    }
    fn bind(&self, py: Python<'_>, receiver: Py<PyAny>) -> PyResult<()> {
        self.state
            .bind(py)
            .get_item("_delivery")?
            .unwrap()
            .setattr("receiver", receiver)
    }
    fn create_app(slf: Py<Self>) -> PyResult<Py<PyAny>> {
        let this = slf.into_any();
        if is_none(&get(&this, "_webhook_secret")?) {
            return Err(PyRuntimeError::new_err(
                "Telegram webhooks need webhook_url and webhook_secret",
            ));
        }
        let app = call(&module("aiohttp.web")?, "Application", vec![], None)?;
        call(
            &get(&app, "router")?,
            "add_post",
            vec![
                j(json!(policy::WEBHOOK_PATH))?,
                Python::attach(|py| host::callback(py, get(&this, "_receive")?))?,
            ],
            None,
        )?;
        Ok(app)
    }
    fn run(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            call(&get(&this, "_delivery")?, "check_bound", vec![], None)?;
            if !is_none(&get(&this, "_worker")?) {
                return Err(PyRuntimeError::new_err(
                    "Telegram channel is already running",
                ));
            }
            let worker = spawn(&this, "_serve", vec![])?;
            set(&this, "_worker", clone_obj(&worker))?;
            let waited = wait_task(clone_obj(&worker)).await;
            let closed = acall(&this, "close", vec![], None).await;
            waited?;
            closed?;
            if !extract::<bool>(&call(&worker, "cancelled", vec![], None)?)? {
                call(&worker, "result", vec![], None)?;
            }
            Ok(none())
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let lock = get(&this, "_close_lock")?;
            acall(&lock, "acquire", vec![], None).await?;
            let result = close_inner(&this).await;
            call(&lock, "release", vec![], None)?;
            result
        })
    }
    fn _load_bot_identity(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            load_identity(&slf.into_any()).await?;
            Ok(none())
        })
    }
    fn _serve(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            load_identity(&this).await?;
            if is_none(&get(&this, "_webhook_url")?) {
                client_call(
                    &this,
                    "deleteWebhook",
                    Some(json!({"drop_pending_updates":false})),
                    None,
                )
                .await?;
                acall(&this, "_poll", vec![], None).await
            } else {
                acall(&this, "_serve_webhook", vec![], None).await
            }
        })
    }
    fn _poll(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let mut attempt = 0;
            loop {
                let offset = extract::<Option<i64>>(&get(&this, "_offset")?)?;
                let mut payload = json!({"timeout":30,"allowed_updates":allowed_updates(&this)?});
                if let Some(offset) = offset {
                    payload["offset"] = json!(offset);
                }
                let updates = match client_call(&this, "getUpdates", Some(payload), Some(40.)).await
                {
                    Ok(value) => value,
                    Err(error) => {
                        if !telegram_error(&error) || !transient(&error) {
                            return Err(error);
                        }
                        let delay = policy::retry_delay(
                            attempt,
                            error_field(&error, "retry_after"),
                            constant("RETRY_BASE", 1.)?,
                            constant("RETRY_MAX", 30.)?,
                        );
                        log_warning(
                            "agentchat.channels.telegram.adapter",
                            &format!("Telegram polling failed ({error}); retrying in {delay:.1}s"),
                        )?;
                        attempt += 1;
                        sleep(delay).await?;
                        continue;
                    }
                };
                attempt = 0;
                set(&this, "_confirmed_offset", j(json!(offset))?)?;
                if !admit_all(&this, &value(&updates)?)? {
                    sleep(constant("BUSY_DELAY", 1.)?).await?;
                }
            }
        })
    }
    fn _has_admitted_all(slf: Py<Self>, updates: &Bound<'_, PyAny>) -> PyResult<bool> {
        admit_all(&slf.into_any(), &convert::to_json(updates)?)
    }
    fn _normalize(slf: Py<Self>, py: Python<'_>, update: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let this = slf.into_any();
        normalized(&this, &convert::to_json(update)?).and_then(|m| {
            m.map(|m| convert::to_message(py, &m))
                .unwrap_or_else(|| Ok(py.None()))
        })
    }
    fn _has_submitted(slf: Py<Self>, message: Py<PyAny>) -> PyResult<bool> {
        submit(&slf.into_any(), message)
    }
    fn _answer_callback(slf: Py<Self>, py: Python<'_>, callback_id: String) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            if let Err(error) = client_call(
                &slf.into_any(),
                "answerCallbackQuery",
                Some(json!({"callback_query_id":callback_id})),
                Some(5.),
            )
            .await
            {
                if Python::attach(|py| error.is_instance_of::<pyo3::exceptions::PyException>(py)) {
                    log_warning(
                        "agentchat.channels.telegram.adapter",
                        &format!("Telegram callback answer failed ({error})"),
                    )?;
                } else {
                    return Err(error);
                }
            }
            Ok(none())
        })
    }
    fn _confirm_offset(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            confirm_offset(&slf.into_any()).await?;
            Ok(none())
        })
    }
    fn _serve_webhook(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let app = call(&this, "create_app", vec![], None)?;
            let runner = call(
                &module("aiohttp.web")?,
                "AppRunner",
                vec![app],
                Some(j(json!({"access_log":null}))?),
            )?;
            set(&this, "_runner", clone_obj(&runner))?;
            acall(&runner, "setup", vec![], None).await?;
            let site = call(
                &module("aiohttp.web")?,
                "TCPSite",
                vec![runner, get(&this, "_host")?, get(&this, "_port")?],
                None,
            )?;
            acall(&site, "start", vec![], None).await?;
            client_call(&this,"setWebhook",Some(json!({"url":value(&get(&this,"_webhook_url")?)?,"secret_token":value(&get(&this,"_webhook_secret")?)?,"allowed_updates":allowed_updates(&this)?})),None).await?;
            let event = call(&module("asyncio")?, "Event", vec![], None)?;
            acall(&event, "wait", vec![], None).await
        })
    }
    fn _receive(slf: Py<Self>, py: Python<'_>, request: Py<PyAny>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let secret = extract::<String>(&get(&this, "_webhook_secret")?)?;
            let supplied = extract::<String>(&call(
                &get(&request, "headers")?,
                "get",
                vec![j(json!("X-Telegram-Bot-Api-Secret-Token"))?, j(json!(""))?],
                None,
            )?)?;
            if !agentchat_providers::whatsapp::constant_time_eq(
                supplied.as_bytes(),
                secret.as_bytes(),
            ) {
                return Err(fail_response("HTTPUnauthorized", "Invalid webhook secret"));
            }
            let raw = acall(&request, "read", vec![], None).await?;
            let update: Value = serde_json::from_slice(&extract::<Vec<u8>>(&raw)?)
                .map_err(|_| fail_response("HTTPBadRequest", "Invalid webhook JSON"))?;
            if !update.is_object() || !update["update_id"].is_i64() {
                return Err(fail_response("HTTPBadRequest", "Invalid webhook update"));
            }
            if extract::<bool>(&get(&get(&this, "_delivery")?, "closed")?)? {
                return Err(fail_response(
                    "HTTPServiceUnavailable",
                    "Telegram receiver is closed",
                ));
            }
            if is_none(&get(&this, "_username")?) {
                if let Err(error) = load_identity(&this).await {
                    if Python::attach(|py| {
                        !error.is_instance_of::<pyo3::exceptions::PyException>(py)
                    }) {
                        return Err(error);
                    }
                    log_warning(
                        "agentchat.channels.telegram.adapter",
                        &format!("Telegram bot identity unavailable ({error})"),
                    )?;
                    return Err(fail_response(
                        "HTTPServiceUnavailable",
                        "Telegram receiver is starting",
                    ));
                }
            }
            if !admit(&this, &update)? {
                return Err(fail_response(
                    "HTTPServiceUnavailable",
                    "Telegram receiver is busy",
                ));
            }
            error_response("Response", "OK")
        })
    }
    fn reply(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        content: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let source_host = source;
            let source = Python::attach(|py| convert::from_message(source_host.bind(py)))?;
            let text = extract::<String>(&content).ok();
            let payload = policy::text_payload(text.as_deref()).map_err(PyValueError::new_err)?;
            send(
                &this,
                &source_host,
                &source,
                payload,
                text.as_deref().unwrap(),
            )
            .await
        })
    }
    /// Telegram limits how often one chat's messages can be edited.
    #[classattr]
    fn stream_interval() -> f64 {
        2.0
    }
    #[classattr]
    fn stream_limit() -> usize {
        4096
    }
    /// Send the first part of a streamed answer; later parts edit it in place. With
    /// `stream_drafts=True`, private chats show a draft with a stop button instead.
    fn start_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        text: String,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let handle = start_main(&slf.into_any(), &source, &text).await?;
            j(handle)
        })
    }
    fn append_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        stream: Py<PyAny>,
        text: String,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let mut handle = value(&stream)?;
            if handle.get("draft_id").is_some() {
                return j(show_draft(&this, &source, handle, &text).await?);
            }
            if handle.get("message_id").is_none() {
                // Only task progress has been shown so far; the answer starts now.
                if text.is_empty() {
                    return Ok(stream);
                }
                let mut started = start_main(&this, &source, &text).await?;
                started["progress"] = handle["progress"].take();
                return j(started);
            }
            let (_, stream) = edit_streamed(&this, &source, &stream, &text).await?;
            Ok(stream)
        })
    }
    fn stop_stream(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        stream: Py<PyAny>,
        text: String,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let handle = value(&stream)?;
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let sent = if handle.get("draft_id").is_some()
                || (handle.get("message_id").is_none() && !text.trim().is_empty())
            {
                // A draft is only a preview; the finished answer is sent as a message.
                send(
                    &this,
                    &source,
                    &message,
                    policy::text_payload(Some(&text)).map_err(PyValueError::new_err)?,
                    &text,
                )
                .await?
            } else if let Some(progress) = handle["progress"]["message_id"]
                .as_i64()
                .filter(|_| handle.get("message_id").is_none())
            {
                // Only task progress was shown: confirm it without text.
                let sent = policy::receipt(
                    &message,
                    extract(&get(&this, "_bot_id")?)?,
                    extract(&get(&this, "_first_name")?)?,
                    "",
                    &json!({"message_id": progress}),
                )
                .map_err(PyRuntimeError::new_err)?;
                receipt_with_metadata(&source, &sent)?
            } else {
                policy::text_payload(Some(&text)).map_err(PyValueError::new_err)?;
                let (id, _) = edit_streamed(&this, &source, &stream, &text).await?;
                let sent = policy::receipt(
                    &message,
                    extract(&get(&this, "_bot_id")?)?,
                    extract(&get(&this, "_first_name")?)?,
                    &text,
                    &json!({"message_id": id}),
                )
                .map_err(PyRuntimeError::new_err)?;
                receipt_with_metadata(&source, &sent)?
            };
            remove_progress(&this, &message, &handle).await;
            Ok(sent)
        })
    }
    /// Show a prompt with one inline button per choice, quoting the source.
    fn send_prompt(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        prompt: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let fields = Python::attach(|py| PromptFields::read(prompt.bind(py)))?;
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let payload = policy::prompt_payload(&fields.text, &fields.id, &fields.pairs())
                .map_err(PyValueError::new_err)?;
            send(&this, &source, &message, payload, &fields.text).await
        })
    }
    /// Take the buttons away; an answered prompt also shows the choice and who made it.
    fn close_prompt(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        sent: Py<PyAny>,
        prompt: Py<PyAny>,
        answer: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let (closed, message_id) = Python::attach(|py| -> PyResult<_> {
                let fields = PromptFields::read(prompt.bind(py))?;
                let sent = convert::from_message(sent.bind(py))?;
                Ok((
                    fields.closed_text(answer.bind(py))?,
                    sent.metadata["message_id"].clone(),
                ))
            })?;
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let dest = destination(&this, &message)?;
            let (method, mut payload) =
                policy::close_prompt_request(closed.as_deref()).map_err(PyValueError::new_err)?;
            payload["chat_id"] = json!(dest.chat_id);
            payload["message_id"] = message_id;
            match client_call(&this, method, Some(payload), None).await {
                Ok(_) => Ok(none()),
                Err(error) if error.to_string().contains("message is not modified") => Ok(none()),
                Err(error) => Err(error),
            }
        })
    }
    fn reply_rich(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        content: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let source_host = source;
            let source = Python::attach(|py| convert::from_message(source_host.bind(py)))?;
            destination(&this, &source)?;
            let rich = Python::attach(|py| convert::from_rich_reply(content.bind(py)))?;
            let payload = policy::rich_payload(&rich).map_err(PyValueError::new_err)?;
            send(&this, &source_host, &source, payload, &rich.text).await
        })
    }
    fn set_status(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        status: String,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let source = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let destination = destination(&this, &source)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let key = Python::attach(|py| {
                Ok::<_, PyErr>(
                    (destination.chat_id, destination.thread_id)
                        .into_pyobject(py)?
                        .into_any()
                        .unbind(),
                )
            })?;
            cancel_typing(&this, &key)?;
            if status.is_empty() {
                return j(json!(true));
            }
            let payload = destination.route(json!({"action":"typing"}), false);
            client_call(&this, "sendChatAction", Some(payload.clone()), None).await?;
            cancel_typing(&this, &key)?;
            let task = spawn(&this, "_keep_typing", vec![clone_obj(&key), j(payload)?])?;
            Python::attach(|py| get(&this, "_typing")?.bind(py).set_item(key, task))?;
            j(json!(true))
        })
    }
    fn _keep_typing(
        slf: Py<Self>,
        py: Python<'_>,
        key: Py<PyAny>,
        payload: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let loop_obj = call(&module("asyncio")?, "get_running_loop", vec![], None)?;
            let deadline = extract::<f64>(&call(&loop_obj, "time", vec![], None)?)?
                + constant("TYPING_EXPIRY", 120.)?;
            let result = async {
                loop {
                    let interval = constant("TYPING_INTERVAL", 4.)?;
                    let now = extract::<f64>(&call(&loop_obj, "time", vec![], None)?)?;
                    if now + interval >= deadline {
                        break;
                    }
                    sleep(interval).await?;
                    if let Err(e) = client_call(
                        &this,
                        "sendChatAction",
                        Some(value(&payload)?),
                        Some(interval),
                    )
                    .await
                    {
                        if Python::attach(|py| {
                            !e.is_instance_of::<pyo3::exceptions::PyException>(py)
                        }) {
                            return Err(e);
                        }
                    }
                }
                Ok(none())
            }
            .await;
            let current = call(&module("asyncio")?, "current_task", vec![], None)?;
            Python::attach(|py| -> PyResult<()> {
                let typing = get(&this, "_typing")?;
                let previous = typing.bind(py).call_method1("get", (key.bind(py),))?;
                if previous.is(current.bind(py)) {
                    typing.bind(py).del_item(key.bind(py))?;
                }
                Ok(())
            })?;
            result
        })
    }
    /// Run an action handler, then answer the press with its result.
    fn _answer_action(
        slf: Py<Self>,
        py: Python<'_>,
        handler: Py<PyAny>,
        payload: Py<PyAny>,
        callback_id: String,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let answer = apps::answer(get(&this, "_delivery")?, handler, payload).await;
            let body = match &answer {
                Ok(body) => value(body).unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            if let Err(error) = client_call(
                &this,
                "answerCallbackQuery",
                Some(policy::callback_answer(&callback_id, &body)),
                Some(5.),
            )
            .await
            {
                log_warning(
                    "agentchat.channels.telegram.adapter",
                    &format!("Telegram callback answer failed ({error})"),
                )?;
            }
            answer.map(|_| none())
        })
    }
    /// Show a notice only `user_id` can see: an ephemeral message in a group, an
    /// ordinary reply in that user's private chat. `blocks` may hold one
    /// `reply_markup` mapping.
    #[pyo3(signature=(source,user_id,text,*,blocks=None))]
    fn send_ephemeral(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        user_id: String,
        text: String,
        blocks: Option<Py<PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let markup = blocks
            .map(|blocks| convert::to_json(blocks.bind(py)))
            .transpose()?
            .and_then(|blocks| blocks.as_array().and_then(|b| b.first().cloned()));
        host::coroutine(py, async move {
            let this = slf.into_any();
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let bot = extract(&get(&this, "_bot_id")?)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            match policy::ephemeral_request(&message, bot, &user_id, &text, markup.as_ref())
                .map_err(PyValueError::new_err)?
            {
                policy::Ephemeral::Reply(payload) => {
                    let result = client_call(&this, "sendMessage", Some(payload), None).await?;
                    let sent = policy::receipt(
                        &message,
                        bot,
                        extract(&get(&this, "_first_name")?)?,
                        &text,
                        &value(&result)?,
                    )
                    .map_err(PyRuntimeError::new_err)?;
                    receipt_with_metadata(&source, &sent)
                }
                policy::Ephemeral::Ephemeral(payload) => {
                    let result = client_call(&this, "sendMessage", Some(payload), None).await?;
                    let sent = policy::ephemeral_receipt(
                        &message,
                        bot,
                        extract(&get(&this, "_first_name")?)?,
                        &user_id,
                        &text,
                        &value(&result)?,
                    )
                    .map_err(PyRuntimeError::new_err)?;
                    receipt_with_metadata(&source, &sent)
                }
            }
        })
    }
    /// Delete a message this bot sent (`deleteMessage`, up to 48 hours old) or a
    /// notice (`deleteEphemeralMessage`).
    fn delete(slf: Py<Self>, py: Python<'_>, message: Py<PyAny>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let message = Python::attach(|py| convert::from_message(message.bind(py)))?;
            let (method, payload) =
                policy::delete_request(&message, extract(&get(&this, "_bot_id")?)?)
                    .map_err(PyValueError::new_err)?;
            client_call(&this, method, Some(payload), None).await?;
            Ok(none())
        })
    }
    /// Show task progress as a separate message above the answer, edited in place
    /// and deleted when the answer is finished. A failed update never fails the answer.
    fn update_stream_tasks(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        stream: Py<PyAny>,
        updates: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let updates: Vec<agentchat_core::StreamUpdate> =
                serde_json::from_value(value(&updates)?)
                    .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let mut handle = if is_none(&stream) {
                json!({})
            } else {
                value(&stream)?
            };
            let mut board: TaskBoard =
                serde_json::from_value(handle["progress"]["board"].clone()).unwrap_or_default();
            board.apply(&updates);
            let text = board.render(3500);
            if text.is_empty() {
                return j(handle);
            }
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let dest = destination(&this, &message)?;
            let progress = &handle["progress"];
            let shown = match progress["message_id"].as_i64() {
                Some(id) if progress["shown"] == text.as_str() => Ok(id),
                Some(id) => {
                    let mut payload = stream_payload(&text)?;
                    payload["chat_id"] = json!(dest.chat_id);
                    payload["message_id"] = json!(id);
                    match client_call(&this, "editMessageText", Some(payload), None).await {
                        Err(error) if !error.to_string().contains("message is not modified") => {
                            Err(error)
                        }
                        _ => Ok(id),
                    }
                }
                None => {
                    match client_call(
                        &this,
                        "sendMessage",
                        Some(dest.route(stream_payload(&text)?, true)),
                        None,
                    )
                    .await
                    {
                        Ok(result) => value(&result)?["message_id"]
                            .as_i64()
                            .ok_or_else(|| PyRuntimeError::new_err("No message receipt")),
                        Err(error) => Err(error),
                    }
                }
            };
            match shown {
                Ok(id) => {
                    handle["progress"] = json!({"message_id": id, "board": board, "shown": text});
                }
                Err(error) => log_warning(
                    "agentchat.channels.telegram.adapter",
                    &format!("Telegram task progress update failed ({error})"),
                )?,
            }
            j(handle)
        })
    }
    fn upload_files(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        files: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(
            py,
            async move { upload(&slf.into_any(), source, files).await },
        )
    }
    #[pyo3(signature=(source,attachment,*,max_bytes=default_download_limit()))]
    fn download_attachment(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        attachment: Py<PyAny>,
        max_bytes: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            download(&slf.into_any(), source, attachment, max_bytes).await
        })
    }
}
async fn client_call(
    this: &Py<PyAny>,
    method: &str,
    payload: Option<Value>,
    timeout: Option<f64>,
) -> PyResult<Py<PyAny>> {
    let mut args = vec![j(json!(method))?];
    if let Some(payload) = payload {
        args.push(j(payload)?);
    }
    let kw = timeout
        .map(|t| j(json!({"request_timeout":t})))
        .transpose()?;
    acall(&get(this, "_client")?, "call", args, kw).await
}
fn destination(
    this: &Py<PyAny>,
    source: &agentchat_core::Message,
) -> PyResult<policy::Destination> {
    policy::destination(source, extract(&get(this, "_bot_id")?)?).map_err(PyValueError::new_err)
}
async fn load_identity(this: &Py<PyAny>) -> PyResult<()> {
    let me = match client_call(this, "getMe", None, None).await {
        Ok(v) => value(&v)?,
        Err(e) => {
            if telegram_error(&e) && !transient(&e) {
                return Err(transport::api_error(
                    "Telegram bot authentication failed",
                    error_field(&e, "error_code"),
                    None,
                ));
            }
            return Err(e);
        }
    };
    let identity = policy::bot_identity(&me, extract(&get(this, "_bot_id")?)?)
        .map_err(PyRuntimeError::new_err)?;
    set(this, "_first_name", j(json!(identity.first_name))?)?;
    set(this, "_username", j(json!(identity.username))?)
}
fn normalized(this: &Py<PyAny>, update: &Value) -> PyResult<Option<agentchat_core::Message>> {
    let bot = extract(&get(this, "_bot_id")?)?;
    let username = extract::<String>(&get(this, "_username")?)?;
    Ok(policy::normalize(update, bot, &username))
}
fn submit(this: &Py<PyAny>, message: Py<PyAny>) -> PyResult<bool> {
    if !extract::<bool>(&call(
        &get(this, "_delivery")?,
        "submit",
        vec![clone_obj(this), clone_obj(&message)],
        None,
    )?)? {
        return Ok(false);
    }
    let callback = Python::attach(|py| {
        message
            .bind(py)
            .getattr("metadata")?
            .call_method1("get", ("callback_query_id",))?
            .extract::<Option<String>>()
    })?;
    if let Some(id) = callback {
        let task = spawn(this, "_answer_callback", vec![j(json!(id))?])?;
        let answers = get(this, "_answers")?;
        call(&answers, "add", vec![clone_obj(&task)], None)?;
        call(
            &task,
            "add_done_callback",
            vec![get(&answers, "discard")?],
            None,
        )?;
    }
    Ok(true)
}
fn admit_all(this: &Py<PyAny>, updates: &Value) -> PyResult<bool> {
    for update in updates.as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(id) = update["update_id"].as_i64() else {
            continue;
        };
        if !admit(this, update)? {
            return Ok(false);
        }
        set(this, "_offset", j(json!(id + 1))?)?;
    }
    Ok(true)
}
fn allowed_updates(this: &Py<PyAny>) -> PyResult<Vec<String>> {
    let events = Python::attach(|py| apps::event_types(this.bind(py)))?;
    Ok(policy::allowed_updates(
        &events,
        extract::<bool>(&get(this, "_stream_drafts")?)?,
    ))
}
/// Hand one update to the application's handler or to AgentChat. A press or
/// command the application handles never becomes a message. False when busy.
fn admit(this: &Py<PyAny>, update: &Value) -> PyResult<bool> {
    if let Some(route) = policy::route(update) {
        if let Some(handler) = Python::attach(|py| apps::find(this.bind(py), &route))? {
            let payload = j(update.clone())?;
            let delivery = get(this, "_delivery")?;
            let callback = update["callback_query"]["id"]
                .as_str()
                .filter(|_| matches!(route, Route::Action { .. }));
            let spawned = match callback {
                Some(id) => {
                    let pending = call(
                        this,
                        "_answer_action",
                        vec![handler, payload, j(json!(id))?],
                        None,
                    )?;
                    extract::<bool>(&call(
                        &delivery,
                        "spawn",
                        vec![clone_obj(this), pending],
                        None,
                    )?)?
                }
                None => Python::attach(|py| {
                    apps::spawn(this.bind(py), delivery.bind(py), &handler, &payload)
                })?,
            };
            if !spawned {
                return Ok(false);
            }
            if !matches!(route, Route::Event { .. }) {
                return Ok(true);
            }
        }
    }
    if let Some(stop) = policy::normalize_stop(update, extract(&get(this, "_bot_id")?)?) {
        return submit(this, Python::attach(|py| convert::to_message(py, &stop))?);
    }
    match normalized(this, update)? {
        Some(message) => submit(
            this,
            Python::attach(|py| convert::to_message(py, &message))?,
        ),
        None => Ok(true),
    }
}
async fn confirm_offset(this: &Py<PyAny>) -> PyResult<()> {
    let offset = extract::<Option<i64>>(&get(this, "_offset")?)?;
    if offset.is_none() || offset == extract::<Option<i64>>(&get(this, "_confirmed_offset")?)? {
        return Ok(());
    }
    if let Err(error) = client_call(
        this,
        "getUpdates",
        Some(json!({"offset":offset,"limit":1,"timeout":0})),
        Some(2.),
    )
    .await
    {
        if !telegram_error(&error) {
            return Err(error);
        }
        log_warning(
            "agentchat.channels.telegram.adapter",
            &format!("Telegram offset confirmation failed ({error})"),
        )?;
    }
    Ok(())
}
async fn close_inner(this: &Py<PyAny>) -> PyResult<Py<PyAny>> {
    if extract::<bool>(&get(this, "_closed")?)? {
        return Ok(none());
    }
    set(this, "_closed", j(json!(true))?)?;
    set(&get(this, "_delivery")?, "closed", j(json!(true))?)?;
    let first = async {
        let worker = get(this, "_worker")?;
        let current = call(&module("asyncio")?, "current_task", vec![], None)?;
        if !is_none(&worker) && !Python::attach(|py| worker.bind(py).is(current.bind(py))) {
            call(&worker, "cancel", vec![], None)?;
            wait_task(worker).await?;
        }
        let runner = get(this, "_runner")?;
        if !is_none(&runner) {
            acall(&runner, "cleanup", vec![], None).await?;
        }
        confirm_offset(this).await
    }
    .await;
    let drain = acall(&get(this, "_delivery")?, "close", vec![], None).await;
    let background = async {
        let typing = call(&get(this, "_typing")?, "values", vec![], None)?;
        let mut tasks = items_of(&typing)?;
        tasks.extend(items_of(&get(this, "_answers")?)?);
        for task in &tasks {
            call(task, "cancel", vec![], None)?;
        }
        if !tasks.is_empty() {
            acall(
                &module("asyncio")?,
                "gather",
                tasks,
                Some(j(json!({"return_exceptions":true}))?),
            )
            .await?;
        }
        Ok::<_, PyErr>(())
    }
    .await;
    let closed = acall(&get(this, "_client")?, "close", vec![], None).await;
    first?;
    drain?;
    background?;
    closed
}
async fn send(
    this: &Py<PyAny>,
    source_host: &Py<PyAny>,
    source: &agentchat_core::Message,
    payload: Value,
    text: &str,
) -> PyResult<Py<PyAny>> {
    let dest = destination(this, source)?;
    call(&get(this, "_delivery")?, "check_reply", vec![], None)?;
    let result = client_call(this, "sendMessage", Some(dest.route(payload, true)), None).await?;
    let message = policy::receipt(
        source,
        extract(&get(this, "_bot_id")?)?,
        extract(&get(this, "_first_name")?)?,
        text,
        &value(&result)?,
    )
    .map_err(PyRuntimeError::new_err)?;
    receipt_with_metadata(source_host, &message)
}
/// Start the visible answer: a draft in a private chat when drafts are on,
/// otherwise a message later parts edit. Returns the stream handle.
async fn start_main(this: &Py<PyAny>, source: &Py<PyAny>, text: &str) -> PyResult<Value> {
    let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
    let dest = destination(this, &message)?;
    call(&get(this, "_delivery")?, "check_reply", vec![], None)?;
    if extract::<bool>(&get(this, "_stream_drafts")?)? && policy::drafts_allowed(&message) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |elapsed| elapsed.subsec_nanos() as i64);
        let draft = ((nanos ^ dest.message_id) & 0x7fff_ffff) | 1;
        return show_draft(this, source, json!({"draft_id": draft}), text).await;
    }
    let payload = stream_payload(text)?;
    let result = client_call(this, "sendMessage", Some(dest.route(payload, true)), None).await?;
    let id = value(&result)?["message_id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| PyRuntimeError::new_err("Telegram did not return a message receipt"))?;
    Ok(json!({"message_id": id, "shown": text}))
}
/// Show `text` in the stream's draft; Telegram keeps a draft about 30 seconds.
async fn show_draft(
    this: &Py<PyAny>,
    source: &Py<PyAny>,
    mut handle: Value,
    text: &str,
) -> PyResult<Value> {
    if handle["shown"] == text {
        return Ok(handle);
    }
    let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
    let dest = destination(this, &message)?;
    let draft = handle["draft_id"]
        .as_i64()
        .ok_or_else(|| PyValueError::new_err("Expected a Telegram draft handle"))?;
    client_call(
        this,
        "sendMessageDraft",
        Some(policy::draft_request(&dest, draft, text)),
        None,
    )
    .await?;
    handle["shown"] = json!(text);
    Ok(handle)
}
/// Delete the task progress message once the answer is finished; best effort.
async fn remove_progress(this: &Py<PyAny>, message: &agentchat_core::Message, handle: &Value) {
    let Some(id) = handle["progress"]["message_id"].as_i64() else {
        return;
    };
    let Ok(dest) = destination(this, message) else {
        return;
    };
    if let Err(error) = client_call(
        this,
        "deleteMessage",
        Some(json!({"chat_id": dest.chat_id, "message_id": id})),
        None,
    )
    .await
    {
        let _ = log_warning(
            "agentchat.channels.telegram.adapter",
            &format!("Telegram task progress cleanup failed ({error})"),
        );
    }
}
/// A streamed part as a message payload; a part with no visible text shows "…".
fn stream_payload(text: &str) -> PyResult<Value> {
    policy::text_payload(Some(text))
        .or_else(|_| policy::text_payload(Some("…")))
        .map_err(PyValueError::new_err)
}
/// Edit a streamed message to `text`, unless it already shows it. Returns its
/// message ID and the handle for the next call.
async fn edit_streamed(
    this: &Py<PyAny>,
    source: &Py<PyAny>,
    stream: &Py<PyAny>,
    text: &str,
) -> PyResult<(i64, Py<PyAny>)> {
    let source = Python::attach(|py| convert::from_message(source.bind(py)))?;
    let dest = destination(this, &source)?;
    call(&get(this, "_delivery")?, "check_reply", vec![], None)?;
    let mut handle = value(stream)?;
    let id = handle["message_id"]
        .as_i64()
        .ok_or_else(|| PyValueError::new_err("Expected a Telegram stream handle"))?;
    if handle["shown"] == text {
        return Ok((id, clone_obj(stream)));
    }
    let mut payload = stream_payload(text)?;
    payload["chat_id"] = json!(dest.chat_id);
    payload["message_id"] = json!(id);
    match client_call(this, "editMessageText", Some(payload), None).await {
        Ok(_) => {}
        // Markdown that renders the same as before leaves nothing to change.
        Err(error) if error.to_string().contains("message is not modified") => {}
        Err(error) => return Err(error),
    }
    handle["shown"] = json!(text);
    Ok((id, j(handle)?))
}
fn cancel_typing(this: &Py<PyAny>, key: &Py<PyAny>) -> PyResult<()> {
    let task = call(
        &get(this, "_typing")?,
        "pop",
        vec![clone_obj(key), none()],
        None,
    )?;
    if !is_none(&task) {
        call(&task, "cancel", vec![], None)?;
    }
    Ok(())
}
async fn upload(this: &Py<PyAny>, source: Py<PyAny>, files: Py<PyAny>) -> PyResult<Py<PyAny>> {
    let source = Python::attach(|py| convert::from_message(source.bind(py)))?;
    let dest = destination(this, &source)?;
    let files = items_of(&files)?;
    if files.is_empty() {
        return Err(PyValueError::new_err("Expected at least one file"));
    }
    for file in &files {
        let filename = extract::<String>(&get(file, "filename")?)
            .map_err(|_| PyValueError::new_err("Expected a plain filename"))?;
        let content = Python::attach(|py| {
            get(file, "content")?
                .bind(py)
                .cast::<PyBytes>()
                .map(|value| value.as_bytes().to_vec())
                .map_err(|_| PyValueError::new_err("Expected nonempty file bytes"))
        })?;
        let title = extract::<Option<String>>(&get(file, "title")?)
            .map_err(|_| PyValueError::new_err("Expected a file title"))?;
        policy::validate_upload(&agentchat_core::UploadFile {
            filename,
            content,
            title,
        })
        .map_err(PyValueError::new_err)?;
    }
    let mut uploaded = Vec::new();
    for file in files {
        call(&get(this, "_delivery")?, "check_reply", vec![], None)?;
        let title = extract::<Option<String>>(&get(&file, "title")?)?;
        let fields = policy::upload_fields(&dest, title.as_deref());
        let kw = Python::attach(|py| -> PyResult<_> {
            let kw = PyDict::new(py);
            kw.set_item("method", "sendDocument")?;
            kw.set_item("fields", convert::from_json(py, &fields)?)?;
            kw.set_item("file_field", "document")?;
            kw.set_item("filename", get(&file, "filename")?)?;
            kw.set_item("content", get(&file, "content")?)?;
            Ok(kw.into_any().unbind())
        })?;
        let result = value(&acall(&get(this, "_client")?, "upload", vec![], Some(kw)).await?)?;
        let id = policy::document_receipt(&result).map_err(PyRuntimeError::new_err)?;
        uploaded.push(call(
            &module("agentchat.models")?,
            "UploadedFile",
            vec![j(json!(id))?],
            None,
        )?);
    }
    Python::attach(|py| Ok(PyTuple::new(py, uploaded)?.into_any().unbind()))
}
async fn download(
    this: &Py<PyAny>,
    source: Py<PyAny>,
    attachment: Py<PyAny>,
    limit: Py<PyAny>,
) -> PyResult<Py<PyAny>> {
    let source = Python::attach(|py| convert::from_message(source.bind(py)))?;
    let attachment = Python::attach(|py| convert::from_attachment(attachment.bind(py)))?;
    let max = policy::validate_download(
        &source,
        extract(&get(this, "_bot_id")?)?,
        &attachment,
        &value(&limit)?,
    )
    .map_err(PyValueError::new_err)?;
    let file = acall(
        &get(this, "_client")?,
        "download_file",
        vec![j(json!(attachment.id))?],
        Some(j(json!({"max_bytes":max}))?),
    )
    .await?;
    let content = get(&file, "content")?;
    if Python::attach(|py| content.bind(py).len())? == 0 {
        return Err(PyValueError::new_err(
            "This Telegram file was empty. Send the attachment again.",
        ));
    }
    let path = extract::<String>(&get(&file, "path")?)?;
    let name = policy::download_name(&attachment, &path);
    let kw = Python::attach(|py| -> PyResult<_> {
        let kw = PyDict::new(py);
        kw.set_item("id", attachment.id)?;
        kw.set_item("name", name)?;
        kw.set_item("content", content)?;
        kw.set_item("media_type", attachment.media_type)?;
        Ok(kw.into_any().unbind())
    })?;
    call(
        &module("agentchat.models")?,
        "DownloadedFile",
        vec![],
        Some(kw),
    )
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    host::register_initialized_class::<Telegram>(module)?;
    module.add_function(wrap_pyfunction!(telegram_normalize, module)?)?;
    module.add_function(wrap_pyfunction!(telegram_render, module)?)?;
    module.add_function(wrap_pyfunction!(telegram_text_payload, module)?)?;
    module.add_function(wrap_pyfunction!(telegram_truncate_caption, module)?)?;
    module.add_function(wrap_pyfunction!(telegram_retry_delay, module)?)?;
    transport::register(module)
}
