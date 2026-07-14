//! Output abstraction for JSON serialization, to output either to a string or
//! Python object.

use pyo3::{
    Bound, PyAny, PyResult, Python,
    exceptions::PyValueError,
    types::{
        PyAnyMethods as _, PyBool, PyDict, PyDictMethods as _, PyFloat, PyInt, PyList,
        PyListMethods as _, PyString, PyStringMethods as _,
    },
};

/// A streaming sink for JSON output.
pub(crate) trait JsonSink {
    /// Begin a JSON object.
    fn begin_object(&mut self) -> PyResult<()>;
    /// End a JSON object.
    fn end_object(&mut self) -> PyResult<()>;
    /// Begin a JSON array.
    fn begin_array(&mut self) -> PyResult<()>;
    /// End a JSON array.
    fn end_array(&mut self) -> PyResult<()>;
    /// Emit a key.
    fn key(&mut self, key: &str) -> PyResult<()>;
    /// Emit a key from a Python string.
    fn py_key(&mut self, key: &Bound<'_, PyString>) -> PyResult<()>;
    /// Emit a null value.
    fn null(&mut self) -> PyResult<()>;
    /// Emit a boolean value.
    fn bool(&mut self, value: bool) -> PyResult<()>;
    /// Emit a JSON integer value (value must be within 32-bit).
    fn i64(&mut self, value: i64) -> PyResult<()>;
    /// Emit a JSON number from a Python object (int or float).
    fn py_number(&mut self, value: &Bound<'_, PyAny>) -> PyResult<()>;
    /// Emit a JSON string value.
    fn str(&mut self, value: &str) -> PyResult<()>;
    /// Emit a JSON string value from a Python string.
    fn py_str(&mut self, value: &Bound<'_, PyString>) -> PyResult<()>;
}

/// A container frame tracking whether the next element needs a leading comma.
struct Frame {
    /// Whether this container is an array (values self-separate) vs an object
    /// (keys handle separation).
    array: bool,
    /// Whether the container is still empty.
    empty: bool,
}

/// A `JsonSink` that builds a compact JSON string.
pub(crate) struct StringSink {
    out: String,
    stack: Vec<Frame>,
}

impl StringSink {
    pub(crate) fn new() -> Self {
        Self {
            out: String::new(),
            stack: Vec::new(),
        }
    }

    pub(crate) fn finish(self) -> String {
        self.out
    }

    /// Emits a separator before a value if the enclosing container is an array
    /// and already has an element. Object values follow a key that already
    /// wrote the separator and colon, so no-op there.
    fn before_value(&mut self) {
        if let Some(frame) = self.stack.last_mut()
            && frame.array
        {
            if !frame.empty {
                self.out.push(',');
            }
            frame.empty = false;
        }
    }
}

impl JsonSink for StringSink {
    fn begin_object(&mut self) -> PyResult<()> {
        self.before_value();
        self.out.push('{');
        self.stack.push(Frame {
            array: false,
            empty: true,
        });
        Ok(())
    }

    fn end_object(&mut self) -> PyResult<()> {
        self.stack.pop();
        self.out.push('}');
        Ok(())
    }

    fn begin_array(&mut self) -> PyResult<()> {
        self.before_value();
        self.out.push('[');
        self.stack.push(Frame {
            array: true,
            empty: true,
        });
        Ok(())
    }

    fn end_array(&mut self) -> PyResult<()> {
        self.stack.pop();
        self.out.push(']');
        Ok(())
    }

    fn key(&mut self, key: &str) -> PyResult<()> {
        if let Some(frame) = self.stack.last_mut() {
            if !frame.empty {
                self.out.push(',');
            }
            frame.empty = false;
        }
        write_escaped(&mut self.out, key);
        self.out.push(':');
        Ok(())
    }

    fn py_key(&mut self, key: &Bound<'_, PyString>) -> PyResult<()> {
        self.key(key.to_str()?)
    }

    fn null(&mut self) -> PyResult<()> {
        self.before_value();
        self.out.push_str("null");
        Ok(())
    }

    fn bool(&mut self, value: bool) -> PyResult<()> {
        self.before_value();
        self.out.push_str(if value { "true" } else { "false" });
        Ok(())
    }

    fn i64(&mut self, value: i64) -> PyResult<()> {
        self.before_value();
        let mut buf = itoa::Buffer::new();
        self.out.push_str(buf.format(value));
        Ok(())
    }

    fn py_number(&mut self, value: &Bound<'_, PyAny>) -> PyResult<()> {
        // Fast paths avoiding a Python `repr` call, both byte-identical to
        // `json.dumps`: ryu for a finite float in Python's fixed-notation range,
        // and itoa for an int that fits `i64` (common when an int is assigned to
        // a float field, or a large open-enum value). Scientific-notation floats
        // and out-of-`i64` ints fall back to `repr` for exact parity.
        if value.is_instance_of::<PyFloat>() {
            let float_value = value.extract::<f64>()?;
            if let Some(text) = ryu_fixed_notation(float_value) {
                self.before_value();
                self.out.push_str(&text);
                return Ok(());
            }
        } else if let Ok(int_value) = value.extract::<i64>() {
            return self.i64(int_value);
        }
        let repr = value.repr()?;
        self.before_value();
        self.out.push_str(repr.to_str()?);
        Ok(())
    }

    fn str(&mut self, value: &str) -> PyResult<()> {
        self.before_value();
        write_escaped(&mut self.out, value);
        Ok(())
    }

    fn py_str(&mut self, value: &Bound<'_, PyString>) -> PyResult<()> {
        self.str(value.to_str()?)
    }
}

/// Returns `ryu`'s formatting of `f` iff it is byte-identical to `CPython`'s
/// `repr(f)` (and therefore `json.dumps`): `CPython` uses fixed notation for
/// `0.0` and `1e-4 <= |f| < 1e16` (decimal point in `-3..=16`), and a
/// fixed-notation shortest representation is canonical. When `ryu` chooses
/// scientific notation (its threshold differs) or the value is outside the fixed
/// range, returns `None` so the caller uses `repr` (`CPython` writes
/// `1e+30`/`1e-05`; `ryu` writes `1e30`/`1e-5`).
fn ryu_fixed_notation(f: f64) -> Option<String> {
    let abs = f.abs();
    if f != 0.0 && !(1e-4..1e16).contains(&abs) {
        return None;
    }
    let mut buffer = ryu::Buffer::new();
    let formatted = buffer.format_finite(f);
    if formatted.bytes().any(|byte| byte == b'e' || byte == b'E') {
        None
    } else {
        Some(formatted.to_owned())
    }
}

/// A container being built by `PyValueSink`.
enum ValueFrame<'py> {
    Object {
        dict: Bound<'py, PyDict>,
        pending_key: Option<Bound<'py, PyString>>,
    },
    Array {
        list: Bound<'py, PyList>,
    },
}

/// A `JsonSink` that builds a Python object tree (for `message_to_json_value`).
pub(crate) struct PyValueSink<'py> {
    py: Python<'py>,
    root: Option<Bound<'py, PyAny>>,
    stack: Vec<ValueFrame<'py>>,
}

impl<'py> PyValueSink<'py> {
    pub(crate) fn new(py: Python<'py>) -> Self {
        Self {
            py,
            root: None,
            stack: Vec::new(),
        }
    }

    pub(crate) fn finish(self) -> PyResult<Bound<'py, PyAny>> {
        self.root
            .ok_or_else(|| PyValueError::new_err("no JSON value produced"))
    }

    /// Attaches a produced value to the enclosing container (or the root).
    fn attach(&mut self, value: Bound<'py, PyAny>) -> PyResult<()> {
        match self.stack.last_mut() {
            Some(ValueFrame::Object { dict, pending_key }) => {
                let key = pending_key
                    .take()
                    .ok_or_else(|| PyValueError::new_err("object value without key"))?;
                dict.set_item(key, value)?;
            }
            Some(ValueFrame::Array { list }) => list.append(value)?,
            None => self.root = Some(value),
        }
        Ok(())
    }
}

impl JsonSink for PyValueSink<'_> {
    fn begin_object(&mut self) -> PyResult<()> {
        let dict = PyDict::new(self.py);
        self.attach(dict.clone().into_any())?;
        self.stack.push(ValueFrame::Object {
            dict,
            pending_key: None,
        });
        Ok(())
    }

    fn end_object(&mut self) -> PyResult<()> {
        self.stack.pop();
        Ok(())
    }

    fn begin_array(&mut self) -> PyResult<()> {
        let list = PyList::empty(self.py);
        self.attach(list.clone().into_any())?;
        self.stack.push(ValueFrame::Array { list });
        Ok(())
    }

    fn end_array(&mut self) -> PyResult<()> {
        self.stack.pop();
        Ok(())
    }

    fn key(&mut self, key: &str) -> PyResult<()> {
        self.set_pending_key(PyString::new(self.py, key))
    }

    fn py_key(&mut self, key: &Bound<'_, PyString>) -> PyResult<()> {
        // Reuse the existing Python string as the dict key (rebind to the sink's
        // GIL lifetime), avoiding a fresh allocation.
        let key = key.clone().unbind().into_bound(self.py);
        self.set_pending_key(key)
    }

    fn null(&mut self) -> PyResult<()> {
        let value = self.py.None().into_bound(self.py);
        self.attach(value)
    }

    fn bool(&mut self, value: bool) -> PyResult<()> {
        let value = PyBool::new(self.py, value).to_owned().into_any();
        self.attach(value)
    }

    fn i64(&mut self, value: i64) -> PyResult<()> {
        let value = PyInt::new(self.py, value).into_any();
        self.attach(value)
    }

    fn py_number(&mut self, value: &Bound<'_, PyAny>) -> PyResult<()> {
        // Rebind to the sink's GIL lifetime (same object, longer-lived token).
        let value = value.clone().unbind().into_bound(self.py);
        self.attach(value)
    }

    fn str(&mut self, value: &str) -> PyResult<()> {
        let value = PyString::new(self.py, value).into_any();
        self.attach(value)
    }

    fn py_str(&mut self, value: &Bound<'_, PyString>) -> PyResult<()> {
        let value = value.clone().unbind().into_bound(self.py).into_any();
        self.attach(value)
    }
}

impl<'py> PyValueSink<'py> {
    fn set_pending_key(&mut self, key: Bound<'py, PyString>) -> PyResult<()> {
        match self.stack.last_mut() {
            Some(ValueFrame::Object { pending_key, .. }) => {
                *pending_key = Some(key);
                Ok(())
            }
            _ => Err(PyValueError::new_err("object key outside of object")),
        }
    }
}

/// Writes `s` as a quoted, escaped JSON string, matching `CPython`'s
/// `json.dumps(s, ensure_ascii=False)`: escape `"`, `\`, and control
/// characters; pass everything else (including non-ASCII) through unchanged.
fn write_escaped(out: &mut String, s: &str) {
    use std::fmt::Write as _;

    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
