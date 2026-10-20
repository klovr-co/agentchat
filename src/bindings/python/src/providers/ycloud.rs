//! Python foreign-I/O adapter for the portable YCloud WhatsApp policies.
use crate::{convert, host};
use agentchat_providers::ycloud as policy;
use pyo3::{
    exceptions::{PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PyTuple},
    PyTraverseError, PyVisit,
};
use serde_json::json;

use super::transport::*;

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
#[pyo3(signature=(content,*,sender,recipient,message_id))]
fn ycloud_text_payload(
    py: Python<'_>,
    content: &Bound<'_, PyAny>,
    sender: &str,
    recipient: &str,
    message_id: &str,
) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &policy::text_payload(
            content.extract::<String>().ok().as_deref(),
            sender,
            recipient,
            message_id,
        )
        .map_err(PyValueError::new_err)?,
    )
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
    #[pyo3(signature=(*,phone_number,api_key,webhook_secret,host="127.0.0.1",port=3981,max_pending_events=64,shutdown_timeout=20.0))]
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        slf: Py<Self>,
        py: Python<'_>,
        phone_number: &str,
        api_key: &str,
        webhook_secret: &str,
        host: &str,
        port: i64,
        max_pending_events: i64,
        shutdown_timeout: f64,
    ) -> PyResult<()> {
        policy::validate_config(phone_number, api_key, webhook_secret)
            .map_err(PyValueError::new_err)?;
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
            let messages =
                policy::normalize(&payload, &extract::<String>(&get(&this, "_phone_number")?)?);
            let delivery = get(&this, "_delivery")?;
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
            let phone = extract::<String>(&get(&this, "_phone_number")?)?;
            let destination =
                policy::destination(&source, &phone).map_err(PyValueError::new_err)?;
            let text = extract::<String>(&content).ok();
            let payload = policy::text_payload(
                text.as_deref(),
                &phone,
                &destination.recipient,
                &destination.message_id,
            )
            .map_err(PyValueError::new_err)?;
            call(&get(&this, "_delivery")?, "check_reply", vec![], None)?;
            let mut session = get(&this, "_session")?;
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
                set(&this, "_session", clone_obj(&session))?;
            }
            let root = extract::<String>(&get(
                &module("agentchat.channels.ycloud.adapter")?,
                "API_ROOT",
            )?)?;
            let url = format!("{root}/whatsapp/messages/sendDirectly");
            let kwargs = j(
                json!({"headers":{"X-API-Key":extract::<String>(&get(&this,"_api_key")?)?},"json":payload,"allow_redirects":false}),
            )?;
            let result = async {
                let context = call(&session, "post", vec![j(json!(url))?], Some(kwargs))?;
                let response = acall(&context, "__aenter__", vec![], None).await?;
                let response_result = async {
                    let status = extract::<i64>(&get(&response, "status")?)?;
                    if !(200..300).contains(&status) {
                        return Err(PyRuntimeError::new_err(format!(
                            "YCloud send failed (HTTP {status})"
                        )));
                    }
                    acall(&response, "json", vec![], None).await
                }
                .await;
                exit_context(&context, response_result).await
            }
            .await;
            let result = match result {
                Ok(v) => v,
                Err(error) => {
                    let sanitize = Python::attach(|py| -> PyResult<bool> {
                        Ok(
                            error.is_instance(py, &py.import("aiohttp")?.getattr("ClientError")?)
                                || error.is_instance_of::<pyo3::exceptions::PyTimeoutError>(py)
                                || error.is_instance_of::<PyValueError>(py),
                        )
                    })?;
                    return Err(if sanitize {
                        suppress_context(PyRuntimeError::new_err(
                            "YCloud request failed; delivery may be uncertain",
                        ))
                    } else {
                        error
                    });
                }
            };
            let message =
                policy::receipt(&source, &phone, text.as_deref().unwrap(), &value(&result)?)
                    .map_err(PyRuntimeError::new_err)?;
            receipt_with_metadata(&source_host, &message)
        })
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    host::register_initialized_class::<YCloud>(module)?;
    module.add_function(wrap_pyfunction!(ycloud_normalize, module)?)?;
    module.add_function(wrap_pyfunction!(ycloud_text_payload, module)?)?;
    Ok(())
}
