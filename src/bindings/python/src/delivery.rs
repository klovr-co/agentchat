use crate::host::{await_py, coroutine};
use pyo3::{
    exceptions::{PyException, PyRuntimeError, PyValueError},
    prelude::*,
    types::{PyDict, PySet, PyTuple},
};
use pyo3::{PyTraverseError, PyVisit};

#[pyclass(dict, subclass, weakref, module = "agentchat._native")]
pub struct DeliveryTasks {
    inner: Option<Box<DeliveryTasksState>>,
}
pub struct DeliveryTasksState {
    receiver: Py<PyAny>,
    tasks: Py<PySet>,
    limit: usize,
    shutdown_timeout: f64,
    closed: bool,
}
impl std::ops::Deref for DeliveryTasks {
    type Target = DeliveryTasksState;
    fn deref(&self) -> &Self::Target {
        self.inner
            .as_deref()
            .expect("native object has been cleared")
    }
}
impl std::ops::DerefMut for DeliveryTasks {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
            .as_deref_mut()
            .expect("native object has been cleared")
    }
}

#[pymethods]
impl DeliveryTasks {
    #[new]
    #[pyo3(signature=(*_args, **_kwargs))]
    fn new(
        _args: &Bound<'_, pyo3::types::PyTuple>,
        _kwargs: Option<&Bound<'_, pyo3::types::PyDict>>,
    ) -> Self {
        Self { inner: None }
    }
    fn initialize(&mut self, py: Python<'_>, limit: isize, shutdown_timeout: f64) -> PyResult<()> {
        *self = Self::build(py, limit, shutdown_timeout)?;
        Ok(())
    }
    fn check_bound(&self, py: Python<'_>) -> PyResult<()> {
        if self.receiver.is_none(py) {
            return Err(PyRuntimeError::new_err(
                "Bind a receiver before running the channel",
            ));
        }
        if self.closed {
            return Err(PyRuntimeError::new_err(
                "A closed channel cannot be restarted",
            ));
        }
        Ok(())
    }
    fn check_reply(&self, py: Python<'_>) -> PyResult<()> {
        if self.closed
            && !self
                .tasks
                .bind(py)
                .contains(py.import("asyncio")?.call_method0("current_task")?)?
        {
            return Err(PyRuntimeError::new_err("Channel is closed"));
        }
        Ok(())
    }
    fn submit(&self, py: Python<'_>, channel: Py<PyAny>, message: Py<PyAny>) -> PyResult<bool> {
        if self.closed || self.tasks.bind(py).len() >= self.limit {
            return Ok(false);
        }
        self.check_bound(py)?;
        let receiver = self.receiver.clone_ref(py);
        let future = coroutine(py, async move {
            let result = async {
                let pending = Python::attach(|py| {
                    receiver
                        .bind(py)
                        .call1((channel.bind(py), message.bind(py)))
                        .map(Bound::unbind)
                })?;
                await_py(pending).await
            }
            .await;
            if let Err(error) = result {
                Python::attach(|py| -> PyResult<()> {
                    if !error.is_instance_of::<PyException>(py) {
                        return Err(error);
                    }
                    let logger = py
                        .import("logging")?
                        .call_method1("getLogger", ("agentchat._delivery",))?;
                    let kwargs = PyDict::new(py);
                    kwargs.set_item(
                        "exc_info",
                        (error.get_type(py), error.value(py), error.traceback(py)),
                    )?;
                    logger.call_method(
                        "error",
                        (
                            "%s message handler failed",
                            channel.bind(py).getattr("name")?,
                        ),
                        Some(&kwargs),
                    )?;
                    Ok(())
                })?;
            }
            Ok(Python::attach(|py| py.None()))
        })?;
        let task = py
            .import("asyncio")?
            .call_method1("create_task", (future,))?;
        self.tasks.bind(py).add(&task)?;
        task.call_method1(
            "add_done_callback",
            (self.tasks.bind(py).getattr("discard")?,),
        )?;
        Ok(true)
    }
    /// Run an application handler's awaitable as accepted work: bounded like
    /// messages, logged on failure and drained by `close()`. False when closed or full.
    fn spawn(&self, py: Python<'_>, channel: Py<PyAny>, awaitable: Py<PyAny>) -> PyResult<bool> {
        if self.closed || self.tasks.bind(py).len() >= self.limit {
            return Ok(false);
        }
        let future = coroutine(py, async move {
            if let Err(error) = await_py(awaitable).await {
                Python::attach(|py| -> PyResult<()> {
                    if !error.is_instance_of::<PyException>(py) {
                        return Err(error);
                    }
                    let kwargs = PyDict::new(py);
                    kwargs.set_item(
                        "exc_info",
                        (error.get_type(py), error.value(py), error.traceback(py)),
                    )?;
                    py.import("logging")?
                        .call_method1("getLogger", ("agentchat._delivery",))?
                        .call_method(
                            "error",
                            ("%s handler failed", channel.bind(py).getattr("name")?),
                            Some(&kwargs),
                        )?;
                    Ok(())
                })?;
            }
            Ok(Python::attach(|py| py.None()))
        })?;
        self.track(
            py,
            py.import("asyncio")?
                .call_method1("create_task", (future,))?
                .unbind(),
        )?;
        Ok(true)
    }
    /// Drain an already running task on `close()`, whatever the limit.
    fn track(&self, py: Python<'_>, task: Py<PyAny>) -> PyResult<()> {
        let task = task.bind(py);
        self.tasks.bind(py).add(task)?;
        task.call_method1(
            "add_done_callback",
            (self.tasks.bind(py).getattr("discard")?,),
        )?;
        Ok(())
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        let Some(state) = &self.inner else {
            return Ok(());
        };

        visit.call(&state.receiver)?;
        visit.call(&state.tasks)
    }
    fn __clear__(&mut self) {
        self.inner = None;
    }
    fn close(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<PyAny>> {
        coroutine(py, async move {
            let (tasks, timeout) = Python::attach(|py| -> PyResult<_> {
                let mut this = slf.borrow_mut(py);
                this.closed = true;
                let current = py.import("asyncio")?.call_method0("current_task")?;
                let tasks = this.tasks.bind(py).call_method0("copy")?;
                tasks.call_method1("discard", (current,))?;
                Ok((tasks.unbind(), this.shutdown_timeout))
            })?;
            if Python::attach(|py| tasks.bind(py).is_truthy())? {
                let waiting = Python::attach(|py| -> PyResult<_> {
                    let kw = PyDict::new(py);
                    kw.set_item("timeout", timeout)?;
                    Ok(py
                        .import("asyncio")?
                        .call_method("wait", (tasks,), Some(&kw))?
                        .unbind())
                })?;
                let result = await_py(waiting).await?;
                let gathered = Python::attach(|py| -> PyResult<_> {
                    let pending = result.bind(py).get_item(1)?;
                    let tasks: Vec<_> = pending.try_iter()?.collect::<PyResult<Vec<_>>>()?;
                    for task in &tasks {
                        task.call_method0("cancel")?;
                    }
                    let kw = PyDict::new(py);
                    kw.set_item("return_exceptions", true)?;
                    Ok(py
                        .import("asyncio")?
                        .call_method("gather", PyTuple::new(py, tasks)?, Some(&kw))?
                        .unbind())
                })?;
                await_py(gathered).await?;
            }
            Ok(Python::attach(|py| py.None()))
        })
    }

    #[getter(limit)]
    fn get_limit(&self) -> usize {
        self.limit
    }
    #[setter(limit)]
    fn set_limit(&mut self, value: usize) {
        self.limit = value;
    }
    #[getter(shutdown_timeout)]
    fn get_shutdown_timeout(&self) -> f64 {
        self.shutdown_timeout
    }
    #[setter(shutdown_timeout)]
    fn set_shutdown_timeout(&mut self, value: f64) {
        self.shutdown_timeout = value;
    }
    #[getter(closed)]
    fn get_closed(&self) -> bool {
        self.closed
    }
    #[setter(closed)]
    fn set_closed(&mut self, value: bool) {
        self.closed = value;
    }
    #[getter(receiver)]
    fn get_receiver(&self, py: Python<'_>) -> Py<PyAny> {
        self.receiver.clone_ref(py)
    }
    #[setter(receiver)]
    fn set_receiver(&mut self, value: Py<PyAny>) {
        self.receiver = value;
    }

    #[getter(tasks)]
    fn get_tasks(&self, py: Python<'_>) -> Py<PySet> {
        self.tasks.clone_ref(py)
    }
    #[setter(tasks)]
    fn set_tasks(&mut self, value: Py<PySet>) {
        self.tasks = value;
    }
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    crate::host::register_initialized_class::<DeliveryTasks>(module)
}

impl DeliveryTasks {
    #[allow(clippy::too_many_arguments)]
    fn build(py: Python<'_>, limit: isize, shutdown_timeout: f64) -> PyResult<Self> {
        if limit < 1 || shutdown_timeout < 0.0 {
            return Err(PyValueError::new_err(
                "max_pending_events must be positive; shutdown_timeout nonnegative",
            ));
        }
        Ok(Self {
            inner: Some(Box::new(DeliveryTasksState {
                receiver: py.None(),
                tasks: PySet::empty(py)?.unbind(),
                limit: limit as usize,
                shutdown_timeout,
                closed: false,
            })),
        })
    }
}
