//! Input abstraction for JSON parsing — the read-side mirror of `JsonSink`.
//!
//! The recursive-descent parser in `json_parse` is generic over a `JsonSource`,
//! monomorphized over `JiterSource` (a pull parser over `str`/`bytes` input) and
//! `PyTreeSource` (a cursor over an already-materialized Python tree, used for
//! `google.protobuf.Any` subtrees and `message_from_json_value`).

use jiter::{Jiter, JiterError, NumberAny, NumberInt, Peek};
use pyo3::{
    Bound, IntoPyObject as _, PyAny, PyErr, PyResult, Python,
    exceptions::PyValueError,
    types::{
        PyAnyMethods as _, PyDictMethods as _, PyFloat, PyInt, PyListMethods as _, PyString,
        PyStringMethods as _,
    },
};

/// The kind of the next JSON value, as reported by `peek`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

/// A streaming source of JSON tokens producing Python values bound to `'py`.
pub(crate) trait JsonSource<'py> {
    /// The GIL token for values produced by this source.
    fn py(&self) -> Python<'py>;
    /// Reports the kind of the next value without consuming it.
    fn peek(&mut self) -> PyResult<JsonKind>;
    fn next_null(&mut self) -> PyResult<()>;
    fn next_bool(&mut self) -> PyResult<bool>;
    /// Reads a JSON number as a Python `int` or `float` object.
    fn next_number(&mut self) -> PyResult<Bound<'py, PyAny>>;
    fn next_float(&mut self) -> PyResult<f64>;
    /// Reads a JSON string as a Python string object.
    fn next_str(&mut self) -> PyResult<Bound<'py, PyString>>;
    /// Reads a JSON string as an owned Rust string (map keys, enum names).
    fn next_string(&mut self) -> PyResult<String>;
    /// Begins an array, returning whether it has a first element (positioned to
    /// read it).
    fn next_array(&mut self) -> PyResult<bool>;
    /// Advances to the next array element, returning whether one exists.
    fn array_step(&mut self) -> PyResult<bool>;
    /// Begins an object, returning its first key (positioned to read the value)
    /// or `None` if empty.
    fn next_object(&mut self) -> PyResult<Option<String>>;
    /// Advances to the next object key, or `None` at the end.
    fn next_key(&mut self) -> PyResult<Option<String>>;
    /// Skips the next value entirely (for ignored unknown fields).
    fn skip(&mut self) -> PyResult<()>;
}

/// A `JsonSource` backed by the `jiter` pull parser over raw JSON bytes.
pub(crate) struct JiterSource<'a> {
    jiter: Jiter<'a>,
    data: &'a [u8],
    py: Python<'a>,
}

impl<'a> JiterSource<'a> {
    pub(crate) fn new(py: Python<'a>, data: &'a [u8]) -> Self {
        // `allow_inf_nan` stays off: bare `NaN`/`Infinity` are invalid ProtoJSON.
        Self {
            jiter: Jiter::new(data),
            data,
            py,
        }
    }

    /// Consumes trailing whitespace and errors on trailing content, matching
    /// `json.loads`.
    pub(crate) fn finish(&mut self) -> PyResult<()> {
        let (py, data) = (self.py, self.data);
        self.jiter
            .finish()
            .map_err(|err| jiter_decode_error(py, data, &err))
    }
}

/// Converts a jiter error into a `json.JSONDecodeError` (a `ValueError`
/// subclass, so `pytest.raises(ValueError, match=...)` still matches). The exact
/// message text differs from `CPython`'s parser (accepted divergence).
fn jiter_decode_error(py: Python<'_>, data: &[u8], err: &JiterError) -> PyErr {
    let message = err.error_type.to_string();
    let doc = String::from_utf8_lossy(data);
    let build = || -> PyResult<PyErr> {
        let json_mod = py.import("json")?;
        let exc = json_mod.getattr("JSONDecodeError")?;
        let obj = exc.call1((message.as_str(), doc.as_ref(), err.index))?;
        Ok(PyErr::from_value(obj))
    };
    match build() {
        Ok(err) | Err(err) => err,
    }
}

fn peek_to_kind(peek: Peek) -> JsonKind {
    match peek {
        Peek::Null => JsonKind::Null,
        Peek::True | Peek::False => JsonKind::Bool,
        Peek::String => JsonKind::String,
        Peek::Array => JsonKind::Array,
        Peek::Object => JsonKind::Object,
        // Numbers (including the bare `Infinity`/`NaN` peeks, which error when
        // actually read since `allow_inf_nan` is off).
        _ => JsonKind::Number,
    }
}

fn number_any_to_py(py: Python<'_>, number: NumberAny) -> PyResult<Bound<'_, PyAny>> {
    match number {
        NumberAny::Int(NumberInt::Int(value)) => Ok(PyInt::new(py, value).into_any()),
        NumberAny::Int(NumberInt::BigInt(value)) => Ok(value.into_pyobject(py)?.into_any()),
        NumberAny::Float(value) => Ok(PyFloat::new(py, value).into_any()),
    }
}

impl<'py> JsonSource<'py> for JiterSource<'py> {
    fn py(&self) -> Python<'py> {
        self.py
    }

    fn peek(&mut self) -> PyResult<JsonKind> {
        let (py, data) = (self.py, self.data);
        let peek = self
            .jiter
            .peek()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(peek_to_kind(peek))
    }

    fn next_null(&mut self) -> PyResult<()> {
        let (py, data) = (self.py, self.data);
        self.jiter
            .next_null()
            .map_err(|err| jiter_decode_error(py, data, &err))
    }

    fn next_bool(&mut self) -> PyResult<bool> {
        let (py, data) = (self.py, self.data);
        self.jiter
            .next_bool()
            .map_err(|err| jiter_decode_error(py, data, &err))
    }

    fn next_number(&mut self) -> PyResult<Bound<'py, PyAny>> {
        let (py, data) = (self.py, self.data);
        let number = self
            .jiter
            .next_number()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        number_any_to_py(py, number)
    }

    fn next_float(&mut self) -> PyResult<f64> {
        let (py, data) = (self.py, self.data);
        self.jiter
            .next_float()
            .map_err(|err| jiter_decode_error(py, data, &err))
    }

    fn next_str(&mut self) -> PyResult<Bound<'py, PyString>> {
        let (py, data) = (self.py, self.data);
        let value = self
            .jiter
            .next_str()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(PyString::new(py, value))
    }

    fn next_string(&mut self) -> PyResult<String> {
        let (py, data) = (self.py, self.data);
        let value = self
            .jiter
            .next_str()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(value.to_owned())
    }

    fn next_array(&mut self) -> PyResult<bool> {
        let (py, data) = (self.py, self.data);
        let first = self
            .jiter
            .next_array()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(first.is_some())
    }

    fn array_step(&mut self) -> PyResult<bool> {
        let (py, data) = (self.py, self.data);
        let next = self
            .jiter
            .array_step()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(next.is_some())
    }

    fn next_object(&mut self) -> PyResult<Option<String>> {
        let (py, data) = (self.py, self.data);
        let key = self
            .jiter
            .next_object()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(key.map(str::to_owned))
    }

    fn next_key(&mut self) -> PyResult<Option<String>> {
        let (py, data) = (self.py, self.data);
        let key = self
            .jiter
            .next_key()
            .map_err(|err| jiter_decode_error(py, data, &err))?;
        Ok(key.map(str::to_owned))
    }

    fn skip(&mut self) -> PyResult<()> {
        let (py, data) = (self.py, self.data);
        self.jiter
            .next_skip()
            .map_err(|err| jiter_decode_error(py, data, &err))
    }
}

/// Raised for a JSON structural type that a field cannot accept.
pub(crate) fn unexpected_json_type() -> PyErr {
    PyValueError::new_err("unexpected json type")
}

/// A container being iterated by `PyTreeSource`.
enum TreeFrame<'py> {
    Array {
        items: Vec<Bound<'py, PyAny>>,
        index: usize,
    },
    Object {
        keys: Vec<String>,
        values: Vec<Bound<'py, PyAny>>,
        index: usize,
    },
}

/// A `JsonSource` that walks an already-materialized Python tree (used for
/// `google.protobuf.Any` subtrees and `message_from_json_value`).
pub(crate) struct PyTreeSource<'py> {
    py: Python<'py>,
    /// The value positioned to be read next, if any.
    current: Option<Bound<'py, PyAny>>,
    stack: Vec<TreeFrame<'py>>,
}

impl<'py> PyTreeSource<'py> {
    pub(crate) fn new(py: Python<'py>, root: Bound<'py, PyAny>) -> Self {
        Self {
            py,
            current: Some(root),
            stack: Vec::new(),
        }
    }

    fn take_current(&mut self) -> PyResult<Bound<'py, PyAny>> {
        self.current
            .take()
            .ok_or_else(|| PyValueError::new_err("unexpected end of JSON tree"))
    }
}

impl<'py> JsonSource<'py> for PyTreeSource<'py> {
    fn py(&self) -> Python<'py> {
        self.py
    }

    fn peek(&mut self) -> PyResult<JsonKind> {
        let value = self
            .current
            .as_ref()
            .ok_or_else(|| PyValueError::new_err("unexpected end of JSON tree"))?;
        if value.is_none() {
            Ok(JsonKind::Null)
        } else if value.is_instance_of::<pyo3::types::PyBool>() {
            Ok(JsonKind::Bool)
        } else if value.is_instance_of::<PyInt>() || value.is_instance_of::<PyFloat>() {
            Ok(JsonKind::Number)
        } else if value.is_instance_of::<PyString>() {
            Ok(JsonKind::String)
        } else if value.is_instance_of::<pyo3::types::PyList>() {
            Ok(JsonKind::Array)
        } else if value.is_instance_of::<pyo3::types::PyDict>() {
            Ok(JsonKind::Object)
        } else {
            Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "unexpected type in JSON tree: {}",
                value.get_type()
            )))
        }
    }

    fn next_null(&mut self) -> PyResult<()> {
        let value = self.take_current()?;
        if value.is_none() {
            Ok(())
        } else {
            Err(unexpected_json_type())
        }
    }

    fn next_bool(&mut self) -> PyResult<bool> {
        self.take_current()?.extract::<bool>()
    }

    fn next_number(&mut self) -> PyResult<Bound<'py, PyAny>> {
        self.take_current()
    }

    fn next_float(&mut self) -> PyResult<f64> {
        self.take_current()?.extract::<f64>()
    }

    fn next_str(&mut self) -> PyResult<Bound<'py, PyString>> {
        let value = self.take_current()?;
        Ok(value.cast_into::<PyString>()?)
    }

    fn next_string(&mut self) -> PyResult<String> {
        let value = self.take_current()?;
        value.cast::<PyString>()?.to_str().map(str::to_owned)
    }

    fn next_array(&mut self) -> PyResult<bool> {
        let value = self.take_current()?;
        let list = value.cast_into::<pyo3::types::PyList>()?;
        let items: Vec<Bound<'py, PyAny>> = list.iter().collect();
        if items.is_empty() {
            // Do not push a frame; `array_step` (which pops) is never called for
            // an empty array, so the frame would otherwise leak.
            return Ok(false);
        }
        self.current = Some(items[0].clone());
        self.stack.push(TreeFrame::Array { items, index: 0 });
        Ok(true)
    }

    fn array_step(&mut self) -> PyResult<bool> {
        let Some(TreeFrame::Array { items, index }) = self.stack.last_mut() else {
            return Err(PyValueError::new_err("array_step outside of array"));
        };
        *index += 1;
        if let Some(next) = items.get(*index) {
            self.current = Some(next.clone());
            Ok(true)
        } else {
            self.stack.pop();
            self.current = None;
            Ok(false)
        }
    }

    fn next_object(&mut self) -> PyResult<Option<String>> {
        let value = self.take_current()?;
        let dict = value.cast_into::<pyo3::types::PyDict>()?;
        let mut keys = Vec::with_capacity(dict.len());
        let mut values = Vec::with_capacity(dict.len());
        for (key, val) in &dict {
            let key = key.cast::<PyString>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err("JSON object keys must be strings")
            })?;
            keys.push(key.to_str()?.to_owned());
            values.push(val);
        }
        if keys.is_empty() {
            return Ok(None);
        }
        let first_key = keys[0].clone();
        self.current = Some(values[0].clone());
        self.stack.push(TreeFrame::Object {
            keys,
            values,
            index: 0,
        });
        Ok(Some(first_key))
    }

    fn next_key(&mut self) -> PyResult<Option<String>> {
        let Some(TreeFrame::Object {
            keys,
            values,
            index,
        }) = self.stack.last_mut()
        else {
            return Err(PyValueError::new_err("next_key outside of object"));
        };
        *index += 1;
        if let (Some(key), Some(value)) = (keys.get(*index), values.get(*index)) {
            let key = key.clone();
            self.current = Some(value.clone());
            Ok(Some(key))
        } else {
            self.stack.pop();
            self.current = None;
            Ok(None)
        }
    }

    fn skip(&mut self) -> PyResult<()> {
        self.current = None;
        Ok(())
    }
}
