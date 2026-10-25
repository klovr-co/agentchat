//! Python foreign-I/O adapter for the portable YCloud WhatsApp policies.
use crate::prompts::PromptFields;
use crate::{convert, host};
use agentchat_providers::ycloud::{self as policy, kind};
use pyo3::{
    exceptions::{PyRuntimeError, PyStopAsyncIteration, PyValueError},
    prelude::*,
    types::{PyBytes, PyDict, PyTuple},
    PyTraverseError, PyVisit,
};
use serde_json::{json, Value};

use super::transport::*;

const ADAPTER: &str = "agentchat.channels.ycloud.adapter";

#[pyfunction]
#[pyo3(signature=(payload,*,phone_number))]
fn ycloud_normalize(
    py: Python<'_>,
    payload: &Bound<'_, PyAny>,
    phone_number: &str,
) -> PyResult<Py<PyAny>> {
    let messages = policy::normalize(&convert::to_json(payload)?, phone_number);
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
#[pyo3(signature=(content,*,sender,recipient,message_id,external_id=None))]
fn ycloud_text_payload(
    py: Python<'_>,
    content: &Bound<'_, PyAny>,
    sender: &str,
    recipient: &str,
    message_id: &str,
    external_id: Option<&str>,
) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &policy::text_payload(
            content.extract::<String>().ok().as_deref(),
            sender,
            recipient,
            message_id,
            external_id,
        )
        .map_err(PyValueError::new_err)?,
    )
}
#[pyfunction]
#[pyo3(signature=(*,sender,recipient,name,language_code,components=None,external_id=None))]
fn ycloud_template_payload(
    py: Python<'_>,
    sender: &str,
    recipient: &str,
    name: &str,
    language_code: &str,
    components: Option<&Bound<'_, PyAny>>,
    external_id: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let components = components
        .map(convert::to_json)
        .transpose()?
        .unwrap_or(Value::Null);
    convert::from_json(
        py,
        &policy::template_payload(
            sender,
            recipient,
            name,
            language_code,
            &components,
            external_id,
        )
        .map_err(PyValueError::new_err)?,
    )
}
#[pyfunction]
#[pyo3(signature=(instance,description,*,kind,status=None,code=None,whatsapp_code=None,request_id=None))]
fn ycloud_error_init(
    instance: &Bound<'_, PyAny>,
    description: &str,
    kind: &str,
    status: Option<i64>,
    code: Option<&str>,
    whatsapp_code: Option<&str>,
    request_id: Option<&str>,
) -> PyResult<()> {
    instance.setattr("args", (description,))?;
    instance.setattr("kind", kind)?;
    instance.setattr("status", status)?;
    instance.setattr("code", code)?;
    instance.setattr("whatsapp_code", whatsapp_code)?;
    instance.setattr("request_id", request_id)
}

/// Builds `YCloudError`; its text never contains message bodies or credentials.
fn ycloud_error(error: &policy::ApiError) -> PyErr {
    let built = (|| {
        call(
            &module(ADAPTER)?,
            "YCloudError",
            vec![j(json!(error.description()))?],
            Some(j(json!({
                "kind": error.kind,
                "status": error.status,
                "code": error.code,
                "whatsapp_code": error.whatsapp_code,
                "request_id": error.request_id,
            }))?),
        )
    })();
    match built {
        Ok(e) => suppress_context(Python::attach(|py| PyErr::from_value(e.into_bound(py)))),
        Err(e) => e,
    }
}
fn local_error(kind: &'static str, status: Option<i64>) -> PyErr {
    ycloud_error(&policy::ApiError {
        kind,
        status,
        code: None,
        whatsapp_code: None,
        request_id: None,
    })
}
/// Timeouts, connection errors, and unreadable bodies leave the outcome unknown.
fn transport_failure(error: PyErr) -> PyErr {
    let sanitize = Python::attach(|py| -> PyResult<bool> {
        Ok(
            error.is_instance(py, &py.import("aiohttp")?.getattr("ClientError")?)
                || error.is_instance_of::<pyo3::exceptions::PyTimeoutError>(py)
                || error.is_instance_of::<PyValueError>(py),
        )
    });
    match sanitize {
        Ok(true) => local_error(kind::UNCERTAIN, None),
        Ok(false) => error,
        Err(e) => e,
    }
}
fn session(this: &Py<PyAny>) -> PyResult<Py<PyAny>> {
    let session = get(this, "_session")?;
    if !is_none(&session) {
        return Ok(session);
    }
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
    let session = call(&module("aiohttp")?, "ClientSession", vec![], Some(kw))?;
    set(this, "_session", clone_obj(&session))?;
    Ok(session)
}
fn api_root() -> PyResult<String> {
    extract(&get(&module(ADAPTER)?, "API_ROOT")?)
}
/// One authenticated request; no retries. Non-2xx bodies become `YCloudError`.
async fn api_json(this: &Py<PyAny>, method: &str, path: &str, extra: Value) -> PyResult<Value> {
    let session = session(this)?;
    let url = format!("{}{path}", api_root()?);
    let mut kwargs = json!({"headers":{"X-API-Key":extract::<String>(&get(this, "_api_key")?)?},"allow_redirects":false});
    if let (Some(target), Some(extra)) = (kwargs.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    let result = async {
        let context = call(&session, method, vec![j(json!(url))?], Some(j(kwargs)?))?;
        let response = acall(&context, "__aenter__", vec![], None).await?;
        let response_result = async {
            let status = extract::<i64>(&get(&response, "status")?)?;
            let raw = extract::<Vec<u8>>(&acall(&response, "read", vec![], None).await?)?;
            Ok((status, raw))
        }
        .await;
        exit_context(&context, response_result).await
    }
    .await;
    let (status, raw) = result.map_err(transport_failure)?;
    if !(200..300).contains(&status) {
        let body = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        return Err(ycloud_error(&policy::api_error(status, &body)));
    }
    serde_json::from_slice(&raw).map_err(|_| local_error(kind::UNCERTAIN, Some(status)))
}
enum Fetched {
    Status(i64),
    Body(Vec<u8>, Option<String>),
}
async fn fetch_media(
    session: &Py<PyAny>,
    link: &str,
    api_key: Option<&str>,
    max: usize,
) -> PyResult<Fetched> {
    // `encoded=True` keeps the signed query (`sig=t%3D…`) byte-for-byte.
    let url = call(
        &module("yarl")?,
        "URL",
        vec![j(json!(link))?],
        Some(j(json!({"encoded":true}))?),
    )?;
    let timeout = call(
        &module("aiohttp")?,
        "ClientTimeout",
        vec![],
        Some(j(json!({"total":60}))?),
    )?;
    let headers = match api_key {
        Some(key) => json!({"X-API-Key":key}),
        None => json!({}),
    };
    let kwargs = Python::attach(|py| -> PyResult<_> {
        let kw = PyDict::new(py);
        kw.set_item("timeout", timeout)?;
        kw.set_item("allow_redirects", false)?;
        kw.set_item("headers", convert::from_json(py, &headers)?)?;
        Ok(kw.into_any().unbind())
    })?;
    let context = call(session, "get", vec![url], Some(kwargs))?;
    let response = acall(&context, "__aenter__", vec![], None).await?;
    let result = async {
        let status = extract::<i64>(&get(&response, "status")?)?;
        if status != 200 {
            return Ok(Fetched::Status(status));
        }
        let declared = extract::<Option<u64>>(&get(&response, "content_length")?)?;
        if declared.is_some_and(|size| size > max as u64) {
            return Err(local_error(kind::MEDIA_TOO_LARGE, Some(status)));
        }
        let content_type = extract::<Option<String>>(&call(
            &get(&response, "headers")?,
            "get",
            vec![j(json!("Content-Type"))?],
            None,
        )?)?;
        let iterator = call(
            &get(&response, "content")?,
            "iter_chunked",
            vec![j(json!(65536))?],
            None,
        )?;
        let mut bytes = Vec::new();
        loop {
            match acall(&iterator, "__anext__", vec![], None).await {
                Ok(chunk) => {
                    let chunk = extract::<Vec<u8>>(&chunk)?;
                    if bytes.len() + chunk.len() > max {
                        return Err(local_error(kind::MEDIA_TOO_LARGE, Some(status)));
                    }
                    bytes.extend(chunk);
                }
                Err(error) => {
                    if Python::attach(|py| error.is_instance_of::<PyStopAsyncIteration>(py)) {
                        break;
                    }
                    return Err(error);
                }
            }
        }
        Ok(Fetched::Body(bytes, content_type))
    }
    .await;
    exit_context(&context, result).await
}
fn status_event(update: &policy::StatusUpdate) -> PyResult<Py<PyAny>> {
    let adapter = module(ADAPTER)?;
    let mut fields =
        serde_json::to_value(update).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
    let api_error = fields
        .as_object_mut()
        .and_then(|f| f.remove("whatsapp_api_error"))
        .filter(|v| !v.is_null());
    let kwargs = j(fields)?;
    let api_error = match api_error {
        Some(error) => call(&adapter, "WhatsAppApiError", vec![], Some(j(error)?))?,
        None => none(),
    };
    Python::attach(|py| {
        kwargs
            .bind(py)
            .set_item("whatsapp_api_error", api_error.bind(py))
    })?;
    call(&adapter, "YCloudStatus", vec![], Some(kwargs))
}

#[pyclass(module = "agentchat.channels.ycloud.adapter", subclass, weakref, dict)]
pub(crate) struct YCloud {
    // CPython may invoke GC traversal on zero-initialized storage before __new__.
    payload: Option<Box<YCloudState>>,
}
pub(crate) struct YCloudState {
    state: Py<PyDict>,
}
impl std::ops::Deref for YCloud {
    type Target = YCloudState;
    fn deref(&self) -> &Self::Target {
        self.payload
            .as_deref()
            .expect("methods require a successfully initialized native adapter")
    }
}
#[pymethods]
impl YCloud {
    #[new]
    #[pyo3(signature=(*_args,**_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Self { payload: None }
    }
    #[pyo3(signature=(*,phone_number,api_key,webhook_secret,host="127.0.0.1",port=3981,smb_echoes=false,max_media_bytes=policy::DEFAULT_MAX_MEDIA_BYTES,max_pending_events=64,shutdown_timeout=20.0))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        phone_number: &str,
        api_key: &str,
        webhook_secret: &str,
        host: &str,
        port: i64,
        smb_echoes: bool,
        max_media_bytes: usize,
        max_pending_events: i64,
        shutdown_timeout: f64,
    ) -> PyResult<()> {
        policy::validate_config(phone_number, api_key, webhook_secret)
            .map_err(PyValueError::new_err)?;
        if max_media_bytes == 0 {
            return Err(PyValueError::new_err("max_media_bytes must be positive"));
        }
        let state = slf.bind(py).getattr("__dict__")?.cast_into::<PyDict>()?;
        for (key, val) in [
            ("_phone_number", phone_number),
            ("_api_key", api_key),
            ("_webhook_secret", webhook_secret),
            ("_host", host),
        ] {
            state.set_item(key, val)?;
        }
        state.set_item("_port", port)?;
        state.set_item("_smb_echoes", smb_echoes)?;
        state.set_item("_max_media_bytes", max_media_bytes)?;
        state.set_item("_event_handler", py.None())?;
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
        state.set_item("_typing", super::typing::keeper(py, ADAPTER, 20.0, 120.0)?)?;
        slf.borrow_mut(py).payload = Some(Box::new(YCloudState {
            state: state.unbind(),
        }));
        Ok(())
    }
    #[classattr]
    fn name() -> &'static str {
        "ycloud"
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
            "YCLOUD_PHONE_NUMBER",
            "YCLOUD_API_KEY",
            "YCLOUD_WEBHOOK_SECRET",
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
        for (key, value) in ["phone_number", "api_key", "webhook_secret"]
            .into_iter()
            .zip(values)
        {
            kw.set_item(key, value)?;
        }
        kw.set_item("host", env("YCLOUD_HOST", "127.0.0.1")?)?;
        kw.set_item(
            "port",
            env("YCLOUD_PORT", "3981")?
                .parse::<i64>()
                .map_err(|e| PyValueError::new_err(e.to_string()))?,
        )?;
        let echoes = env("YCLOUD_SMB_ECHOES", "")?.to_ascii_lowercase();
        kw.set_item(
            "smb_echoes",
            matches!(echoes.as_str(), "1" | "true" | "yes"),
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
    /// `handler(channel, event)` receives status updates and Business App echoes.
    fn bind_events(&self, py: Python<'_>, handler: Py<PyAny>) -> PyResult<()> {
        self.state.bind(py).set_item("_event_handler", handler)
    }
    fn create_app(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let this = slf.into_any();
        let app = call(&module("aiohttp.web")?, "Application", vec![], None)?;
        let router = get(&app, "router")?;
        call(
            &router,
            "add_post",
            vec![
                j(json!("/webhooks/ycloud"))?,
                Python::attach(|py| host::callback(py, get(&this, "_receive")?))?,
            ],
            None,
        )?;
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
    fn _receive(slf: Py<Self>, py: Python<'_>, request: Py<PyAny>) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let raw = acall(&request, "read", vec![], None).await?;
            let bytes = extract::<Vec<u8>>(&raw)?;
            let headers = get(&request, "headers")?;
            let supplied = extract::<String>(&call(
                &headers,
                "get",
                vec![j(json!("YCloud-Signature"))?, j(json!(""))?],
                None,
            )?)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs() as i64);
            // The signature covers the raw bytes; nothing is parsed before it passes.
            if !policy::verify_signature(
                &extract::<String>(&get(&this, "_webhook_secret")?)?,
                &bytes,
                &supplied,
                now,
            ) {
                return Err(fail_response(
                    "HTTPUnauthorized",
                    "Invalid webhook signature",
                ));
            }
            let payload = serde_json::from_slice(&bytes)
                .map_err(|_| fail_response("HTTPBadRequest", "Invalid webhook JSON"))?;
            let event = policy::classify(
                &payload,
                &extract::<String>(&get(&this, "_phone_number")?)?,
                extract::<bool>(&get(&this, "_smb_echoes")?)?,
            );
            let delivery = get(&this, "_delivery")?;
            let event = match event {
                policy::Event::Ignored => return error_response("Response", "OK"),
                policy::Event::Messages(messages) => {
                    let busy = Python::attach(|py| -> PyResult<bool> {
                        Ok(delivery.bind(py).getattr("closed")?.extract::<bool>()?
                            || delivery.bind(py).getattr("tasks")?.len()? + messages.len()
                                > delivery.bind(py).getattr("limit")?.extract::<usize>()?)
                    })?;
                    if busy {
                        return Err(fail_response(
                            "HTTPServiceUnavailable",
                            "YCloud receiver is busy",
                        ));
                    }
                    for message in messages {
                        let message = Python::attach(|py| convert::to_message(py, &message))?;
                        call(&delivery, "submit", vec![clone_obj(&this), message], None)?;
                    }
                    return error_response("Response", "OK");
                }
                policy::Event::Status(update) => status_event(&update)?,
                policy::Event::Echo(message) => {
                    Python::attach(|py| convert::to_message(py, &message))?
                }
            };
            let handler = get(&this, "_event_handler")?;
            if is_none(&handler) {
                return error_response("Response", "OK");
            }
            if extract::<bool>(&get(&delivery, "closed")?)? {
                return Err(fail_response(
                    "HTTPServiceUnavailable",
                    "YCloud receiver is closed",
                ));
            }
            // Awaited before acknowledging: a failure returns 500 so YCloud redelivers.
            let outcome = async {
                let awaitable = Python::attach(|py| {
                    handler
                        .bind(py)
                        .call1((this.clone_ref(py), event))
                        .map(Bound::unbind)
                })?;
                host::await_py(awaitable).await
            }
            .await;
            if let Err(error) = outcome {
                let name = Python::attach(|py| {
                    error
                        .get_type(py)
                        .name()
                        .map(|n| n.to_string())
                        .unwrap_or_default()
                });
                log_warning(ADAPTER, &format!("YCloud event handler failed ({name})"))?;
                return Err(fail_response(
                    "HTTPInternalServerError",
                    "Event handler failed",
                ));
            }
            error_response("Response", "OK")
        })
    }
    #[pyo3(signature=(source,content,*,external_id=None))]
    fn reply(
        slf: Py<Self>,
        py: Python<'_>,
        source: Py<PyAny>,
        content: Py<PyAny>,
        external_id: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let source_host = source;
            let source = Python::attach(|py| convert::from_message(source_host.bind(py)))?;
            let phone = extract::<String>(&get(&this, "_phone_number")?)?;
            let destination =
                policy::destination(&source, &phone).map_err(PyValueError::new_err)?;
            let text = extract::<String>(&content).ok();
            let payload = policy::text_payload(
                text.as_deref(),
                &phone,
                &destination.recipient,
                &destination.message_id,
                external_id.as_deref(),
            )
            .map_err(PyValueError::new_err)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let result = api_json(
                &this,
                "post",
                "/whatsapp/messages/sendDirectly",
                json!({"json":payload}),
            )
            .await?;
            let message = policy::receipt(
                &source,
                &phone,
                text.as_deref().unwrap_or_default(),
                &result,
                external_id.as_deref(),
            )
            .map_err(PyRuntimeError::new_err)?;
            receipt_with_metadata(&source_host, &message)
        })
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
            let phone = extract::<String>(&get(&this, "_phone_number")?)?;
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
            let id = extract::<String>(&call(
                &module("urllib.parse")?,
                "quote",
                vec![j(json!(message_id))?, j(json!(""))?],
                None,
            )?)?;
            let path = format!("/whatsapp/inboundMessages/{id}/typing");
            api_json(&this, "post", &path, json!({})).await?;
            Ok(none())
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
            let phone = extract::<String>(&get(&this, "_phone_number")?)?;
            let destination =
                policy::destination(&message, &phone).map_err(PyValueError::new_err)?;
            let payload = policy::prompt_payload(
                &phone,
                &destination.recipient,
                &destination.message_id,
                &fields.text,
                &fields.id,
                &fields.pairs(),
            )
            .map_err(PyValueError::new_err)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let result = api_json(
                &this,
                "post",
                "/whatsapp/messages/sendDirectly",
                json!({"json": payload}),
            )
            .await?;
            let sent = policy::receipt(&message, &phone, &fields.text, &result, None)
                .map_err(PyRuntimeError::new_err)?;
            receipt_with_metadata(&source, &sent)
        })
    }
    /// WhatsApp cannot edit a sent message, so the buttons stay; later presses are
    /// ignored because the prompt is no longer open.
    fn close_prompt(
        &self,
        py: Python<'_>,
        _source: Py<PyAny>,
        _sent: Py<PyAny>,
        _prompt: Py<PyAny>,
        _answer: Py<PyAny>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move { Ok(none()) })
    }
    #[pyo3(signature=(destination,name,language_code,components=None,*,external_id=None))]
    fn send_template(
        slf: Py<Self>,
        py: Python<'_>,
        destination: String,
        name: String,
        language_code: String,
        components: Option<Py<PyAny>>,
        external_id: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        let components = components
            .map(|c| convert::to_json(c.bind(py)))
            .transpose()?
            .unwrap_or(Value::Null);
        host::coroutine(py, async move {
            let this = slf.into_any();
            let phone = extract::<String>(&get(&this, "_phone_number")?)?;
            let payload = policy::template_payload(
                &phone,
                &destination,
                &name,
                &language_code,
                &components,
                external_id.as_deref(),
            )
            .map_err(PyValueError::new_err)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let result = api_json(
                &this,
                "post",
                "/whatsapp/messages/sendDirectly",
                json!({"json":payload}),
            )
            .await?;
            let message = policy::template_receipt(
                &phone,
                &destination,
                &name,
                &language_code,
                &result,
                external_id.as_deref(),
            )
            .map_err(PyRuntimeError::new_err)?;
            Python::attach(|py| convert::to_message(py, &message))
        })
    }
    #[pyo3(signature=(status=Some("APPROVED".to_owned()),language=None))]
    fn list_templates(
        slf: Py<Self>,
        py: Python<'_>,
        status: Option<String>,
        language: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        host::coroutine(py, async move {
            let this = slf.into_any();
            let adapter = module(ADAPTER)?;
            let mut templates = Vec::new();
            for page in 1..=policy::MAX_TEMPLATE_PAGES {
                let mut params =
                    json!({"page":page.to_string(),"limit":policy::TEMPLATE_PAGE_SIZE.to_string()});
                if let Some(status) = &status {
                    params["filter.status"] = json!(status);
                }
                if let Some(language) = &language {
                    params["filter.language"] = json!(language);
                }
                let body = api_json(
                    &this,
                    "get",
                    "/whatsapp/templates",
                    json!({"params":params}),
                )
                .await?;
                let (items, more) = policy::template_page(&body, policy::TEMPLATE_PAGE_SIZE)
                    .map_err(PyRuntimeError::new_err)?;
                for item in items {
                    let fields = serde_json::to_value(item)
                        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
                    templates.push(call(&adapter, "YCloudTemplate", vec![], Some(j(fields)?))?);
                }
                if !more {
                    break;
                }
            }
            Python::attach(|py| Ok(pyo3::types::PyList::new(py, templates)?.into_any().unbind()))
        })
    }
    #[pyo3(signature=(media,*,max_bytes=None))]
    fn download_media(
        slf: Py<Self>,
        py: Python<'_>,
        media: Py<PyAny>,
        max_bytes: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let source = match convert::from_message(media.bind(py)) {
            Ok(message) => Value::Object(message.metadata),
            Err(_) => convert::to_json(media.bind(py))?,
        };
        host::coroutine(py, async move {
            let this = slf.into_any();
            let media = policy::media_ref(&source).map_err(PyValueError::new_err)?;
            policy::check_media_link(&media.link, &api_root()?).map_err(PyValueError::new_err)?;
            let max = match max_bytes {
                Some(0) => return Err(PyValueError::new_err("max_bytes must be positive")),
                Some(max) => max,
                None => extract::<usize>(&get(&this, "_max_media_bytes")?)?,
            };
            let session = session(&this)?;
            let api_key = extract::<String>(&get(&this, "_api_key")?)?;
            // The signed link works without a key for a few minutes; then the key is required.
            let mut fetched = fetch_media(&session, &media.link, None, max)
                .await
                .map_err(transport_failure)?;
            if matches!(fetched, Fetched::Status(401 | 403)) {
                fetched = fetch_media(&session, &media.link, Some(&api_key), max)
                    .await
                    .map_err(transport_failure)?;
            }
            let (content, content_type) = match fetched {
                Fetched::Body(content, _) if content.is_empty() => {
                    return Err(local_error(kind::MEDIA_UNAVAILABLE, Some(200)))
                }
                Fetched::Body(content, content_type) => (content, content_type),
                Fetched::Status(status) => {
                    return Err(local_error(kind::MEDIA_UNAVAILABLE, Some(status)))
                }
            };
            if let Some(expected) = &media.sha256 {
                if !policy::media_digest_matches(&content, expected) {
                    return Err(local_error(kind::MEDIA_INTEGRITY, None));
                }
            }
            let kw = Python::attach(|py| -> PyResult<_> {
                let kw = PyDict::new(py);
                kw.set_item("id", media.id.clone().unwrap_or_else(|| media.name()))?;
                kw.set_item("name", media.name())?;
                kw.set_item("content", PyBytes::new(py, &content))?;
                kw.set_item("media_type", media.mime_type.clone().or(content_type))?;
                Ok(kw.into_any().unbind())
            })?;
            call(
                &module("agentchat.models")?,
                "DownloadedFile",
                vec![],
                Some(kw),
            )
        })
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    host::register_initialized_class::<YCloud>(module)?;
    module.add_function(wrap_pyfunction!(ycloud_normalize, module)?)?;
    module.add_function(wrap_pyfunction!(ycloud_text_payload, module)?)?;
    module.add_function(wrap_pyfunction!(ycloud_template_payload, module)?)?;
    module.add_function(wrap_pyfunction!(ycloud_error_init, module)?)?;
    Ok(())
}
