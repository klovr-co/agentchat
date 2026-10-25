//! Python foreign-I/O adapter for the portable WhatsApp policies.
use crate::prompts::PromptFields;
use crate::{convert, host};
use agentchat_providers::whatsapp as policy;
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PyTuple},
    PyTraverseError, PyVisit,
};
use serde_json::json;

use super::transport::*;

#[pyfunction]
#[pyo3(signature=(payload,*,phone_number_id))]
fn whatsapp_normalize(
    py: Python<'_>,
    payload: &Bound<'_, PyAny>,
    phone_number_id: &str,
) -> PyResult<Py<PyAny>> {
    let messages = policy::normalize(&convert::to_json(payload)?, phone_number_id);
    Ok(PyTuple::new(
        py,
        messages
            .iter()
            .map(|m| convert::to_message(py, m))
            .collect::<PyResult<Vec<_>>>()?,
    )?
    .into_any()
    .unbind())
}
#[pyfunction]
#[pyo3(signature=(content,*,recipient,message_id))]
fn whatsapp_text_payload(
    py: Python<'_>,
    content: &Bound<'_, PyAny>,
    recipient: &str,
    message_id: &str,
) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &policy::text_payload(
            content.extract::<String>().ok().as_deref(),
            recipient,
            message_id,
        )
        .map_err(PyValueError::new_err)?,
    )
}

#[pyclass(
    module = "agentchat.channels.whatsapp.adapter",
    subclass,
    weakref,
    dict
)]
pub(crate) struct WhatsApp {
    // CPython may invoke GC traversal on zero-initialized storage before __new__.
    payload: Option<Box<WhatsAppState>>,
}
pub(crate) struct WhatsAppState {
    state: Py<PyDict>,
}
impl std::ops::Deref for WhatsApp {
    type Target = WhatsAppState;
    fn deref(&self) -> &Self::Target {
        self.payload
            .as_deref()
            .expect("methods require a successfully initialized native adapter")
    }
}
#[pymethods]
impl WhatsApp {
    #[new]
    #[pyo3(signature=(*_args,**_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self { payload: None }
    }
    #[pyo3(signature=(*,phone_number_id,access_token,app_secret,verify_token,api_version="v23.0",host="127.0.0.1",port=3979,max_pending_events=64,shutdown_timeout=20.0,prompt_style="buttons"))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        phone_number_id: &str,
        access_token: &str,
        app_secret: &str,
        verify_token: &str,
        api_version: &str,
        host: &str,
        port: i64,
        max_pending_events: i64,
        shutdown_timeout: f64,
        prompt_style: &str,
    ) -> PyResult<()> {
        if !matches!(prompt_style, "reactions" | "buttons") {
            return Err(PyValueError::new_err(
                "prompt_style must be 'reactions' or 'buttons'",
            ));
        }
        policy::validate_config(
            phone_number_id,
            access_token,
            app_secret,
            verify_token,
            api_version,
        )
        .map_err(PyValueError::new_err)?;
        let state = slf.bind(py).getattr("__dict__")?.cast_into::<PyDict>()?;
        for (key, val) in [
            ("_phone_number_id", phone_number_id),
            ("_access_token", access_token),
            ("_app_secret", app_secret),
            ("_verify_token", verify_token),
            ("_api_version", api_version),
            ("_host", host),
        ] {
            state.set_item(key, val)?;
        }
        state.set_item("_port", port)?;
        let asyncio = py.import("asyncio")?;
        state.set_item(
            "_delivery",
            py.import("agentchat._delivery")?
                .getattr("DeliveryTasks")?
                .call1((max_pending_events, shutdown_timeout))?,
        )?;
        state.set_item("_stopped", asyncio.getattr("Event")?.call0()?)?;
        state.set_item("_close_lock", asyncio.getattr("Lock")?.call0()?)?;
        state.set_item("_closed", false)?;
        state.set_item("_session", py.None())?;
        state.set_item("_runner", py.None())?;
        state.set_item("_prompt_style", prompt_style)?;
        // Reaction prompts by sent message ID: {"prompt": id, "choices": [[emoji, id], …]}.
        state.set_item("_reaction_prompts", PyDict::new(py))?;
        state.set_item(
            "_typing",
            super::typing::keeper(py, "agentchat.channels.whatsapp.adapter", 20.0, 120.0)?,
        )?;
        slf.borrow_mut(py).payload = Some(Box::new(WhatsAppState {
            state: state.unbind(),
        }));
        Ok(())
    }
    #[classattr]
    fn name() -> &'static str {
        "whatsapp"
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
        let keys = [
            "WHATSAPP_PHONE_NUMBER_ID",
            "WHATSAPP_ACCESS_TOKEN",
            "WHATSAPP_APP_SECRET",
            "WHATSAPP_VERIFY_TOKEN",
        ];
        let values = keys
            .iter()
            .map(|k| env(k, ""))
            .collect::<PyResult<Vec<_>>>()?;
        if values.iter().any(String::is_empty) {
            return Err(PyRuntimeError::new_err(format!(
                "{} are required",
                keys.join(", ")
            )));
        }
        let kw = PyDict::new(py);
        for (key, value) in [
            "phone_number_id",
            "access_token",
            "app_secret",
            "verify_token",
        ]
        .into_iter()
        .zip(values)
        {
            kw.set_item(key, value)?;
        }
        kw.set_item("api_version", env("WHATSAPP_API_VERSION", "v23.0")?)?;
        kw.set_item("prompt_style", env("WHATSAPP_PROMPT_STYLE", "buttons")?)?;
        kw.set_item("host", env("WHATSAPP_HOST", "127.0.0.1")?)?;
        kw.set_item(
            "port",
            env("WHATSAPP_PORT", "3979")?
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
    fn create_app(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let this = slf.into_any();
        let app = call(&module("aiohttp.web")?, "Application", vec![], None)?;
        let router = get(&app, "router")?;
        for (method, path, handler) in [
            ("add_get", "/webhooks/whatsapp", "_verify"),
            ("add_post", "/webhooks/whatsapp", "_receive"),
        ] {
            call(
                &router,
                method,
                vec![
                    j(json!(path))?,
                    Python::attach(|py| host::callback(py, get(&this, handler)?))?,
                ],
                None,
            )?;
        }
        let _ = py;
        Ok(app)
    }
    fn run(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let delivery = get(&this, "_delivery")?;
            call(&delivery, "check_bound", vec![], None)?;
            let app = call(&this, "create_app", vec![], None)?;
            let runner = call(
                &module("aiohttp.web")?,
                "AppRunner",
                vec![app],
                Some(j(json!({"access_log":null}))?),
            )?;
            set(&this, "_runner", clone_obj(&runner))?;
            let result = async {
                acall(&runner, "setup", vec![], None).await?;
                let site = call(
                    &module("aiohttp.web")?,
                    "TCPSite",
                    vec![runner, get(&this, "_host")?, get(&this, "_port")?],
                    None,
                )?;
                acall(&site, "start", vec![], None).await?;
                acall(&get(&this, "_stopped")?, "wait", vec![], None).await
            }
            .await;
            let cleanup = acall(&this, "close", vec![], None).await;
            result?;
            cleanup
        })
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let lock = get(&this, "_close_lock")?;
            acall(&lock, "acquire", vec![], None).await?;
            let result = async {
                if extract::<bool>(&get(&this, "_closed")?)? {
                    return Ok(none());
                }
                set(&this, "_closed", j(json!(true))?)?;
                set(&get(&this, "_delivery")?, "closed", j(json!(true))?)?;
                call(&get(&this, "_stopped")?, "set", vec![], None)?;
                let runner = get(&this, "_runner")?;
                let first = if !is_none(&runner) {
                    acall(&runner, "cleanup", vec![], None).await
                } else {
                    Ok(none())
                };
                acall(&get(&this, "_typing")?, "close", vec![], None).await?;
                let drain = acall(&get(&this, "_delivery")?, "close", vec![], None).await;
                let session = get(&this, "_session")?;
                let closed = if !is_none(&session) {
                    acall(&session, "close", vec![], None).await
                } else {
                    Ok(none())
                };
                first?;
                drain?;
                closed
            }
            .await;
            call(&lock, "release", vec![], None)?;
            result
        })
    }
    fn _verify(slf: Py<Self>, py: Python<'_>, request: Py<PyAny>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let query = get(&request, "query")?;
            let read = |key: &str| -> PyResult<String> {
                extract(&call(
                    &query,
                    "get",
                    vec![j(json!(key))?, j(json!(""))?],
                    None,
                )?)
            };
            let challenge = read("hub.challenge")?;
            if !policy::verify_challenge(
                &read("hub.mode")?,
                &challenge,
                &read("hub.verify_token")?,
                &extract::<String>(&get(&this, "_verify_token")?)?,
            ) {
                return Err(fail_response(
                    "HTTPForbidden",
                    "Webhook verification failed",
                ));
            }
            error_response("Response", &challenge)
        })
    }
    fn _receive(slf: Py<Self>, py: Python<'_>, request: Py<PyAny>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let raw = acall(&request, "read", vec![], None).await?;
            let bytes = extract::<Vec<u8>>(&raw)?;
            let headers = get(&request, "headers")?;
            let supplied = extract::<String>(&call(
                &headers,
                "get",
                vec![j(json!("X-Hub-Signature-256"))?, j(json!(""))?],
                None,
            )?)?;
            if !policy::verify_signature(
                &extract::<String>(&get(&this, "_app_secret")?)?,
                &bytes,
                &supplied,
            ) {
                return Err(fail_response(
                    "HTTPUnauthorized",
                    "Invalid webhook signature",
                ));
            }
            let payload = serde_json::from_slice(&bytes)
                .map_err(|_| fail_response("HTTPBadRequest", "Invalid webhook JSON"))?;
            let phone = extract::<String>(&get(&this, "_phone_number_id")?)?;
            let mut messages = policy::normalize(&payload, &phone);
            messages.extend(answered_reactions(
                &this,
                policy::reactions(&payload, &phone),
            )?);
            let delivery = get(&this, "_delivery")?;
            let busy = Python::attach(|py| -> PyResult<bool> {
                Ok(delivery.bind(py).getattr("closed")?.extract::<bool>()?
                    || delivery.bind(py).getattr("tasks")?.len()? + messages.len()
                        > delivery.bind(py).getattr("limit")?.extract::<usize>()?)
            })?;
            if busy {
                return Err(fail_response(
                    "HTTPServiceUnavailable",
                    "WhatsApp receiver is busy",
                ));
            }
            for message in messages {
                let message = Python::attach(|py| convert::to_message(py, &message))?;
                call(&delivery, "submit", vec![clone_obj(&this), message], None)?;
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
            let phone = extract::<String>(&get(&this, "_phone_number_id")?)?;
            let destination =
                policy::destination(&source, &phone).map_err(PyValueError::new_err)?;
            let text = extract::<String>(&content).ok();
            let payload = policy::text_payload(
                text.as_deref(),
                &destination.recipient,
                &destination.message_id,
            )
            .map_err(PyValueError::new_err)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let result = graph_post(&this, &phone, payload, "send").await?;
            let message =
                policy::receipt(&source, &phone, text.as_deref().unwrap(), &value(&result)?)
                    .map_err(PyRuntimeError::new_err)?;
            receipt_with_metadata(&source_host, &message)
        })
    }
    /// Show a prompt as reply buttons, or a list for more or longer choices.
    fn send_prompt(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        prompt: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let fields = Python::attach(|py| PromptFields::read(prompt.bind(py)))?;
            let phone = extract::<String>(&get(&this, "_phone_number_id")?)?;
            let destination =
                policy::destination(&message, &phone).map_err(PyValueError::new_err)?;
            let buttons = extract::<String>(&get(&this, "_prompt_style")?)? == "buttons";
            let (payload, text, emojis) = if buttons {
                let payload = policy::prompt_payload(
                    &fields.text,
                    &destination.recipient,
                    &destination.message_id,
                    &fields.id,
                    &fields.pairs(),
                )
                .map_err(PyValueError::new_err)?;
                (payload, fields.text.clone(), Vec::new())
            } else {
                let labels: Vec<String> = fields.choices.iter().map(|c| c.1.clone()).collect();
                policy::reaction_prompt_payload(
                    &fields.text,
                    &destination.recipient,
                    &destination.message_id,
                    &labels,
                )
                .map_err(PyValueError::new_err)?
            };
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let result = graph_post(&this, &phone, payload, "send").await?;
            let sent = policy::receipt(&message, &phone, &text, &value(&result)?)
                .map_err(PyRuntimeError::new_err)?;
            if !buttons {
                let choices: Vec<_> = emojis
                    .iter()
                    .zip(&fields.choices)
                    .map(|(emoji, choice)| json!([emoji, choice.0]))
                    .collect();
                remember_reaction_prompt(
                    &this,
                    sent.metadata["message_id"].as_str().unwrap_or_default(),
                    json!({"prompt": fields.id, "choices": choices}),
                )?;
            }
            receipt_with_metadata(&source, &sent)
        })
    }
    /// WhatsApp cannot edit a sent message, so the prompt stays as it is; later
    /// reactions or presses are ignored because the prompt is no longer open.
    fn close_prompt(
        slf: Py<Self>,
        py: Python<'_>,
        _source: Py<PyAny>,
        sent: Py<PyAny>,
        _prompt: Py<PyAny>,
        _answer: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let id = convert::from_message(sent.bind(py))?.metadata["message_id"].clone();
        slf.bind(py)
            .getattr("_reaction_prompts")?
            .call_method1("pop", (convert::from_json(py, &id)?, py.None()))?;
        host::coroutine(py, async move { Ok(none()) })
    }
    /// Show "typing…" for the source message until cleared with "" or the keeper
    /// expires. WhatsApp shows no status text and also marks the message as read.
    fn set_status(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        status: String,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let message = Python::attach(|py| convert::from_message(source.bind(py)))?;
            let phone = extract::<String>(&get(&this, "_phone_number_id")?)?;
            let destination =
                policy::destination(&message, &phone).map_err(PyValueError::new_err)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let keeper = get(&this, "_typing")?;
            let key = j(json!(destination.message_id))?;
            if status.is_empty() {
                call(&keeper, "stop", vec![key], None)?;
                return j(json!(true));
            }
            let send = call(
                &module("functools")?,
                "partial",
                vec![
                    get(&this, "_send_typing")?,
                    j(json!(destination.message_id))?,
                ],
                None,
            )?;
            acall(&keeper, "start", vec![key, send], None).await
        })
    }
    fn _send_typing(slf: Py<Self>, py: Python<'_>, message_id: String) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let phone = extract::<String>(&get(&this, "_phone_number_id")?)?;
            graph_post(&this, &phone, policy::typing_payload(&message_id), "typing").await?;
            Ok(none())
        })
    }
}
/// Most reaction prompts remembered at once; the oldest is forgotten first.
const MAX_REACTION_PROMPTS: usize = 1000;
fn remember_reaction_prompt(
    this: &Py<PyAny>,
    message_id: &str,
    prompt: serde_json::Value,
) -> PyResult<()> {
    Python::attach(|py| {
        let prompts = this
            .bind(py)
            .getattr("_reaction_prompts")?
            .cast_into::<PyDict>()?;
        prompts.set_item(message_id, convert::from_json(py, &prompt)?)?;
        while prompts.len() > MAX_REACTION_PROMPTS {
            let oldest = prompts.keys().get_item(0)?;
            prompts.del_item(oldest)?;
        }
        Ok(())
    })
}
/// Reactions to an open reaction prompt with one of its emoji, as answers; all
/// other reactions are dropped.
fn answered_reactions(
    this: &Py<PyAny>,
    reactions: Vec<agentchat_core::Message>,
) -> PyResult<Vec<agentchat_core::Message>> {
    Python::attach(|py| {
        let prompts = this.bind(py).getattr("_reaction_prompts")?;
        let mut answers = Vec::new();
        for mut reaction in reactions {
            let target = reaction.metadata["reacted_to"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let Some(prompt) = prompts
                .call_method1("get", (target,))?
                .extract::<Option<Bound<'_, PyAny>>>()?
            else {
                continue;
            };
            let prompt = convert::to_json(&prompt)?;
            let emoji = reaction.text.clone();
            let choice = prompt["choices"].as_array().and_then(|choices| {
                choices.iter().find_map(|pair| {
                    policy::same_emoji(&emoji, pair[0].as_str()?).then(|| pair[1].clone())
                })
            });
            if let Some(choice) = choice {
                reaction.metadata.insert(
                    agentchat_core::ANSWER_METADATA.into(),
                    json!({"prompt": prompt["prompt"], "choice": choice}),
                );
                answers.push(reaction);
            }
        }
        Ok(answers)
    })
}
/// POST one Cloud API messages request; a failed send may still have been delivered.
async fn graph_post(
    this: &Py<PyAny>,
    phone: &str,
    payload: serde_json::Value,
    action: &str,
) -> PyResult<Py<PyAny>> {
    let mut session = get(this, "_session")?;
    if is_none(&session) {
        let timeout = call(
            &module("aiohttp")?,
            "ClientTimeout",
            vec![],
            Some(j(json!({"total":30}))?),
        )?;
        let kw = Python::attach(|py| -> PyResult<_> {
            let kw = PyDict::new(py);
            kw.set_item("timeout", timeout)?;
            Ok(kw.into_any().unbind())
        })?;
        session = call(&module("aiohttp")?, "ClientSession", vec![], Some(kw))?;
        set(this, "_session", clone_obj(&session))?;
    }
    let root = extract::<String>(&get(
        &module("agentchat.channels.whatsapp.adapter")?,
        "GRAPH_ROOT",
    )?)?;
    let url = format!(
        "{root}/{}/{phone}/messages",
        extract::<String>(&get(this, "_api_version")?)?
    );
    let kwargs = j(
        json!({"headers":{"Authorization":format!("Bearer {}",extract::<String>(&get(this,"_access_token")?)?)},"json":payload,"allow_redirects":false}),
    )?;
    let result = async {
        let context = call(&session, "post", vec![j(json!(url))?], Some(kwargs))?;
        let response = acall(&context, "__aenter__", vec![], None).await?;
        let response_result = async {
            let status = extract::<i64>(&get(&response, "status")?)?;
            if !(200..300).contains(&status) {
                return Err(PyRuntimeError::new_err(format!(
                    "WhatsApp {action} failed (HTTP {status})"
                )));
            }
            acall(&response, "json", vec![], None).await
        }
        .await;
        exit_context(&context, response_result).await
    }
    .await;
    result.map_err(|error| {
        let sanitize = Python::attach(|py| -> PyResult<bool> {
            Ok(
                error.is_instance(py, &py.import("aiohttp")?.getattr("ClientError")?)
                    || error.is_instance_of::<pyo3::exceptions::PyTimeoutError>(py)
                    || error.is_instance_of::<PyValueError>(py),
            )
        });
        match sanitize {
            Ok(true) => suppress_context(PyRuntimeError::new_err(
                "WhatsApp request failed; delivery may be uncertain",
            )),
            Ok(false) => error,
            Err(other) => other,
        }
    })
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    host::register_initialized_class::<WhatsApp>(module)?;
    module.add_function(wrap_pyfunction!(whatsapp_normalize, module)?)?;
    module.add_function(wrap_pyfunction!(whatsapp_text_payload, module)?)?;
    Ok(())
}
