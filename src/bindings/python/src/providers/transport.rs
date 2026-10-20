//! Foreign object calls and asyncio integration; no channel policy lives here.
use crate::{convert, host};
use pyo3::{
    prelude::*,
    types::{PyDict, PyTuple},
};
use serde_json::{json, Value};

pub(crate) fn none() -> Py<PyAny> {
    Python::attach(|py| py.None())
}
pub(crate) fn j(value: Value) -> PyResult<Py<PyAny>> {
    Python::attach(|py| convert::from_json(py, &value))
}
pub(crate) fn get(obj: &Py<PyAny>, key: &str) -> PyResult<Py<PyAny>> {
    Python::attach(|py| obj.bind(py).getattr(key).map(Bound::unbind))
}
pub(crate) fn set(obj: &Py<PyAny>, key: &str, value: Py<PyAny>) -> PyResult<()> {
    Python::attach(|py| obj.bind(py).setattr(key, value))
}
pub(crate) fn clone_obj(obj: &Py<PyAny>) -> Py<PyAny> {
    Python::attach(|py| obj.clone_ref(py))
}
pub(crate) fn call(
    obj: &Py<PyAny>,
    method: &str,
    args: Vec<Py<PyAny>>,
    kwargs: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    Python::attach(|py| {
        let args = PyTuple::new(py, args)?;
        let kw = kwargs
            .as_ref()
            .map(|k| k.bind(py).cast::<PyDict>())
            .transpose()?;
        obj.bind(py)
            .call_method(method, args, kw)
            .map(Bound::unbind)
    })
}
pub(crate) async fn acall(
    obj: &Py<PyAny>,
    method: &str,
    args: Vec<Py<PyAny>>,
    kwargs: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    host::await_py(call(obj, method, args, kwargs)?).await
}
pub(crate) fn module(name: &str) -> PyResult<Py<PyAny>> {
    Python::attach(|py| Ok(py.import(name)?.into_any().unbind()))
}
pub(crate) fn extract<T: for<'a, 'py> FromPyObject<'a, 'py>>(obj: &Py<PyAny>) -> PyResult<T> {
    Python::attach(|py| obj.bind(py).extract().map_err(Into::into))
}
pub(crate) fn dict() -> Py<PyAny> {
    Python::attach(|py| PyDict::new(py).into_any().unbind())
}
pub(crate) fn value(obj: &Py<PyAny>) -> PyResult<Value> {
    Python::attach(|py| convert::to_json(obj.bind(py)))
}
pub(crate) fn is_none(obj: &Py<PyAny>) -> bool {
    Python::attach(|py| obj.bind(py).is_none())
}
pub(crate) fn error_response(name: &str, text: &str) -> PyResult<Py<PyAny>> {
    call(
        &module("aiohttp.web")?,
        name,
        vec![],
        Some(j(json!({"text":text}))?),
    )
}
pub(crate) fn fail_response(name: &str, text: &str) -> PyErr {
    match error_response(name, text) {
        Ok(err) => Python::attach(|py| PyErr::from_value(err.into_bound(py))),
        Err(err) => err,
    }
}
pub(crate) fn log_warning(module_name: &str, text: &str) -> PyResult<()> {
    let logger = call(
        &module("logging")?,
        "getLogger",
        vec![j(json!(module_name))?],
        None,
    )?;
    call(&logger, "warning", vec![j(json!(text))?], None)?;
    Ok(())
}
pub(crate) async fn sleep(seconds: f64) -> PyResult<()> {
    acall(&module("asyncio")?, "sleep", vec![j(json!(seconds))?], None).await?;
    Ok(())
}
pub(crate) fn env(name: &str, default: &str) -> PyResult<String> {
    extract(&call(
        &module("os")?,
        "getenv",
        vec![j(json!(name))?, j(json!(default))?],
        None,
    )?)
}
pub(crate) async fn exit_context<T>(context: &Py<PyAny>, result: PyResult<T>) -> PyResult<T> {
    let args = match &result {
        Ok(_) => vec![none(), none(), none()],
        Err(error) => Python::attach(|py| {
            vec![
                error.get_type(py).into_any().unbind(),
                error.value(py).clone().into_any().unbind(),
                error
                    .traceback(py)
                    .map(|v| v.into_any().unbind())
                    .unwrap_or_else(|| py.None()),
            ]
        }),
    };
    let exited = acall(context, "__aexit__", args, None).await;
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            exited?;
            Ok(value)
        }
    }
}
pub(crate) fn suppress_context(error: PyErr) -> PyErr {
    Python::attach(|py| {
        let _ = error.value(py).setattr("__suppress_context__", true);
    });
    error
}

/// Keep foreign metadata references that the portable policy has not changed.
/// Portable fields still come exclusively from the Rust receipt.
pub(crate) fn receipt_with_metadata(
    source: &Py<PyAny>,
    receipt: &agentchat_core::Message,
) -> PyResult<Py<PyAny>> {
    Python::attach(|py| {
        let original = source.bind(py).getattr("metadata")?;
        let normalized = convert::to_json(&original)?;
        let metadata = py
            .import("builtins")?
            .getattr("dict")?
            .call1((&original,))?;
        let keys: Vec<String> = metadata
            .call_method0("keys")?
            .try_iter()?
            .filter_map(|key| key.ok()?.extract().ok())
            .collect();
        for key in keys {
            if !receipt.metadata.contains_key(&key) {
                metadata.del_item(key)?;
            }
        }
        for (key, value) in &receipt.metadata {
            if normalized.get(key) != Some(value) {
                metadata.set_item(key, convert::from_json(py, value)?)?;
            }
        }
        let output = convert::to_message(py, receipt)?;
        let kwargs = PyDict::new(py);
        kwargs.set_item(
            "metadata",
            py.import("types")?
                .getattr("MappingProxyType")?
                .call1((metadata,))?,
        )?;
        Ok(py
            .import("dataclasses")?
            .getattr("replace")?
            .call((output,), Some(&kwargs))?
            .unbind())
    })
}
