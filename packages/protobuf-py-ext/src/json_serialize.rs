//! Native `ProtoJSON` serialization, generic over a `JsonSink`.
//!
//! Mirrors the pure-Python `protobuf._to_json`. Validation is inline and native
//! (no Python `validate()` call); error text matches `protobuf._validate`
//! exactly (which differs from the binary serializer's inline text).

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use pyo3::{
    Bound, Py, PyAny, PyErr, PyResult, Python,
    exceptions::{PyOverflowError, PyTypeError, PyValueError},
    types::{
        PyAnyMethods as _, PyBool, PyByteArray, PyByteArrayMethods as _, PyBytes,
        PyBytesMethods as _, PyDict, PyDictMethods as _, PyInt, PyList, PyListMethods as _,
        PyString, PyStringMethods as _, PyType,
    },
};

use crate::{
    constants::Constants,
    descriptor::{DescEnum, DescFieldValue, DescMessage, DescSingleValue, ScalarType},
    json_sink::{JsonSink, StringSink},
    marshaler::MessageMarshaler,
    nativemessage::NativeMessage,
    serializer::{FieldSerializer, FieldSerializerType, FieldSerializerValue, MessageSerializer},
    wkt::WktKind,
    wkt_json::{duration_to_json, proto_camel_case, proto_snake_case, timestamp_to_rfc3339},
};

// Integer range bounds (MIN inclusive, MAX exclusive), matching `_validate.py`.
const INT32_MIN: i64 = -(1 << 31);
const INT32_MAX: i64 = 1 << 31;
const UINT32_MAX: i64 = 1 << 32;
// float32 finite range from `_validate.py`.
const FLOAT32_MAX: f64 = 3.402_823_466_385_288_6e38;
const FLOAT32_MIN: f64 = -3.402_823_466_385_288_6e38;

/// Options controlling JSON output.
pub(crate) struct JsonOpts {
    pub(crate) always_emit_implicit: bool,
    pub(crate) print_enums_as_ints: bool,
    pub(crate) use_proto_field_name: bool,
    pub(crate) registry: Option<Py<PyAny>>,
}

impl MessageMarshaler {
    /// Serializes a message to a compact JSON string.
    pub(crate) fn to_json_string(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        opts: &JsonOpts,
    ) -> PyResult<Py<PyString>> {
        let mut sink = StringSink::new();
        self.write_json(py, message, &mut sink, opts)?;
        Ok(PyString::new(py, &sink.finish()).unbind())
    }

    /// Writes a message as JSON, dispatching on its well-known-type kind.
    pub(crate) fn write_json<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        match &self.wkt {
            WktKind::None | WktKind::FileDescriptorSet => {
                self.write_message_object(py, message, sink, opts)
            }
            WktKind::Timestamp { seconds, nanos } => {
                let sec = self.read_int64_field(py, message, *seconds)?;
                let nan = self.read_int32_field(py, message, *nanos)?;
                sink.str_value(&timestamp_to_rfc3339(sec, nan)?)
            }
            WktKind::Duration { seconds, nanos } => {
                let sec = self.read_int64_field(py, message, *seconds)?;
                let nan = self.read_int32_field(py, message, *nanos)?;
                sink.str_value(&duration_to_json(sec, nan)?)
            }
            WktKind::FieldMask { paths } => self.write_field_mask(py, message, *paths, sink),
            WktKind::Wrapper { field, scalar } => {
                let value = self.field_attr_value(py, message, *field)?;
                write_scalar_json(*scalar, &value, sink)
            }
            WktKind::Struct { fields } => self.write_struct(py, message, *fields, sink, opts),
            WktKind::ListValue { values } => {
                self.write_list_value(py, message, *values, sink, opts)
            }
            WktKind::Value { .. } => self.write_value(py, message, sink, opts),
            WktKind::Any { type_url, value } => {
                self.write_any(py, message, *type_url, *value, sink, opts)
            }
        }
    }

    /// Writes `{ ...fields... }` for a regular message.
    fn write_message_object<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        sink.begin_object()?;
        self.write_message_fields(py, message, sink, opts)?;
        sink.end_object()?;
        Ok(())
    }

    /// Writes a message's fields (and extensions) without the enclosing braces,
    /// so the Any inline form can append `@type` in the same object.
    pub(crate) fn write_message_fields<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        self.serializer.validate_oneofs(py, message.as_any())?;
        for field in self.serializer.fields() {
            let Some(value) =
                MessageSerializer::get_field_value(message, field, opts.always_emit_implicit)?
            else {
                continue;
            };
            let key = if opts.use_proto_field_name {
                field.name.bind(py)
            } else {
                field.json_key.bind(py)
            };
            sink.key(key)?;
            field.serializer.write_json_value(py, &value, sink, opts)?;
        }
        if let Some(registry) = &opts.registry {
            let registry = registry.bind(py);
            self.write_extensions(py, message, sink, opts, registry)?;
        }
        Ok(())
    }

    /// Reads the value of a declaration-order field by index.
    fn field_attr_value<'py>(
        &self,
        py: Python<'py>,
        message: &Bound<'py, NativeMessage>,
        idx: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.serializer.fields()[idx].attr.get(py, message.as_any())
    }

    /// Reads a WKT int64 field, applying `_validate.py`-style int64 checks.
    fn read_int64_field(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        idx: usize,
    ) -> PyResult<i64> {
        let value = self.field_attr_value(py, message, idx)?;
        require_int(&value)?;
        value
            .extract::<i64>()
            .map_err(|_| overflow_value(&value, "int64"))
    }

    /// Reads a WKT int32 field, applying `_validate.py`-style int32 checks.
    fn read_int32_field(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        idx: usize,
    ) -> PyResult<i32> {
        let value = self.field_attr_value(py, message, idx)?;
        require_int(&value)?;
        #[allow(clippy::cast_possible_truncation, reason = "range-checked to i32")]
        Ok(extract_ranged(&value, INT32_MIN, INT32_MAX, "int32")? as i32)
    }

    fn write_field_mask<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        paths_idx: usize,
        sink: &mut S,
    ) -> PyResult<()> {
        let paths = self.field_attr_value(py, message, paths_idx)?;
        let paths = paths.cast::<PyList>()?;
        let mut parts: Vec<String> = Vec::with_capacity(paths.len());
        for path in paths.iter() {
            let path = path.cast::<PyString>()?;
            let path = path.to_str()?;
            let camel = proto_camel_case(path);
            if proto_snake_case(&camel) != path {
                return Err(PyValueError::new_err(format!(
                    "invalid FieldMask path: lowerCamelCase of {path} is irreversible"
                )));
            }
            parts.push(camel);
        }
        sink.str_value(&parts.join(","))
    }

    fn write_struct<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        fields_idx: usize,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        let FieldSerializerType::Map {
            value_serializer, ..
        } = &self.serializer.fields()[fields_idx].serializer.type_
        else {
            return Err(PyValueError::new_err("expected map for Struct.fields"));
        };
        let map = self.field_attr_value(py, message, fields_idx)?;
        let map = map.cast::<PyDict>()?;
        sink.begin_object()?;
        for (key, value) in map {
            sink.key(key.cast::<PyString>()?)?;
            value_serializer.write_single_json_value(py, &value, sink, opts)?;
        }
        sink.end_object()?;
        Ok(())
    }

    fn write_list_value<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        values_idx: usize,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        let element = &self.serializer.fields()[values_idx].serializer;
        let values = self.field_attr_value(py, message, values_idx)?;
        let values = values.cast::<PyList>()?;
        sink.begin_array()?;
        for item in values.iter() {
            element.write_single_json_value(py, &item, sink, opts)?;
        }
        sink.end_array()?;
        Ok(())
    }

    fn write_value<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        let WktKind::Value {
            null_value,
            struct_value,
            list_value,
            ..
        } = &self.wkt
        else {
            return Err(PyValueError::new_err(
                "value must have exactly one field set",
            ));
        };
        // All Value fields share the `kind` oneof accessor.
        let oneof_access = self.serializer.fields()[*null_value]
            .oneof
            .as_ref()
            .ok_or_else(|| PyValueError::new_err("value must have exactly one field set"))?;
        let oneof_any = oneof_access.get(py, message.as_any())?;
        let Ok(oneof) = oneof_any.cast::<crate::oneof::Oneof>() else {
            return Err(PyValueError::new_err(
                "value must have exactly one field set",
            ));
        };
        let oneof = oneof.get();
        let field = oneof.field.bind(py).cast::<PyString>()?;
        let value = oneof.value.bind(py);
        match field.to_str()? {
            "null_value" => sink.null(),
            "number_value" => {
                let number = value.extract::<f64>()?;
                if !number.is_finite() {
                    return Err(PyValueError::new_err("value cannot be NaN or Infinity"));
                }
                sink.py_number(value)
            }
            "string_value" => sink.py_str_value(value.cast::<PyString>()?),
            "bool_value" => sink.bool(value.extract::<bool>()?),
            "struct_value" => self.serializer.fields()[*struct_value]
                .serializer
                .write_single_json_value(py, value, sink, opts),
            "list_value" => self.serializer.fields()[*list_value]
                .serializer
                .write_single_json_value(py, value, sink, opts),
            _ => Err(PyValueError::new_err(
                "value must have exactly one field set",
            )),
        }
    }

    #[allow(clippy::too_many_arguments, reason = "internal writer")]
    fn write_any<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        type_url_idx: usize,
        value_idx: usize,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        let type_url_obj = self.field_attr_value(py, message, type_url_idx)?;
        let type_url = type_url_obj.cast::<PyString>()?.to_str()?.to_owned();
        if type_url.is_empty() {
            sink.begin_object()?;
            sink.end_object()?;
            return Ok(());
        }
        let Some(registry) = &opts.registry else {
            return Err(PyValueError::new_err(format!(
                "any \"{type_url}\" is not in the type registry"
            )));
        };
        let registry = registry.bind(py);
        let type_name = type_url_to_name(&type_url)?;
        let desc = registry.call_method1(self.constants.message.bind(py), (type_name,))?;
        if desc.is_none() {
            return Err(PyValueError::new_err(format!(
                "any: \"{type_url}\" is not in the type registry"
            )));
        }
        let inner_type_obj = desc.getattr(&self.constants.type_)?;
        let inner_type = inner_type_obj.cast::<PyType>()?;
        let inner_marshaler_obj = inner_type.getattr(&self.constants.ext_marshaler)?;
        let inner_marshaler = inner_marshaler_obj
            .cast::<MessageMarshaler>()?
            .get()
            .clone();

        let value_bytes: Vec<u8> = self.field_attr_value(py, message, value_idx)?.extract()?;
        let inner_msg = inner_marshaler.new_empty_message(py, inner_type)?;
        inner_marshaler.merge_from_binary(py, &inner_msg, Bytes::from(value_bytes), false)?;

        sink.begin_object()?;
        if matches!(inner_marshaler.wkt, WktKind::None) {
            // Regular message: inline its fields, then `@type` last.
            inner_marshaler.write_message_fields(py, &inner_msg, sink, opts)?;
            sink.key_str("@type")?;
            sink.str_value(&type_url)?;
        } else {
            // Well-known type (including FileDescriptorSet): wrap in `value`.
            sink.key_str("@type")?;
            sink.str_value(&type_url)?;
            sink.key_str("value")?;
            inner_marshaler.write_json(py, &inner_msg, sink, opts)?;
        }
        sink.end_object()?;
        Ok(())
    }

    fn write_extensions<S: JsonSink>(
        &self,
        py: Python<'_>,
        message: &Bound<'_, NativeMessage>,
        sink: &mut S,
        opts: &JsonOpts,
        registry: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let Some(unknown_fields) = message.get().unknown_fields(py) else {
            return Ok(());
        };
        let unknown_fields = unknown_fields.bind(py);
        if unknown_fields.is_empty() {
            return Ok(());
        }
        let msg_desc = message.as_any().getattr(&self.constants.desc)?;
        for field_number in unknown_fields.keys() {
            let ext_desc = registry.call_method1(
                self.constants.extension_for.bind(py),
                (&msg_desc, &field_number),
            )?;
            if ext_desc.is_none() {
                continue;
            }
            let ext_type = ext_desc.getattr(&self.constants.type_)?;
            let value = message.as_any().get_item(&ext_type)?;
            let ext_value_desc = ext_desc.getattr(&self.constants.value)?;
            let field_value = DescFieldValue::new(py, &ext_value_desc, &self.constants)?;
            let type_name = ext_desc.getattr(&self.constants.type_name)?;
            let type_name = type_name.cast::<PyString>()?;
            sink.key_str(&format!("[{}]", type_name.to_str()?))?;
            write_desc_field_value(py, &field_value, &value, sink, opts)?;
        }
        Ok(())
    }
}

impl FieldSerializer {
    /// Writes a field value as JSON, dispatching on the field's container kind.
    fn write_json_value<S: JsonSink>(
        &self,
        py: Python<'_>,
        value: &Bound<'_, PyAny>,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        match &self.type_ {
            FieldSerializerType::Singular => self.write_single_json_value(py, value, sink, opts),
            FieldSerializerType::List { .. } => {
                let list = value.cast::<PyList>()?;
                sink.begin_array()?;
                for item in list.iter() {
                    self.write_single_json_value(py, &item, sink, opts)?;
                }
                sink.end_array()?;
                Ok(())
            }
            FieldSerializerType::Map {
                key_serializer,
                value_serializer,
            } => {
                let FieldSerializerValue::Scalar(key_type) = &key_serializer.value else {
                    return Err(PyValueError::new_err("invalid map key type"));
                };
                let dict = value.cast::<PyDict>()?;
                sink.begin_object()?;
                for (key, val) in dict {
                    write_map_key(*key_type, &key, sink)?;
                    value_serializer.write_single_json_value(py, &val, sink, opts)?;
                }
                sink.end_object()?;
                Ok(())
            }
        }
    }

    /// Writes a single (non-container) value, per this serializer's value kind.
    fn write_single_json_value<S: JsonSink>(
        &self,
        py: Python<'_>,
        value: &Bound<'_, PyAny>,
        sink: &mut S,
        opts: &JsonOpts,
    ) -> PyResult<()> {
        match &self.value {
            FieldSerializerValue::Scalar(scalar) => write_scalar_json(*scalar, value, sink),
            FieldSerializerValue::Enum(enum_) => write_enum_json(py, enum_, value, sink, opts),
            FieldSerializerValue::Message { message, .. } => {
                write_message_json(py, message, value, sink, opts)
            }
        }
    }
}

/// Writes an extension value from its `DescFieldValue` (extensions are not part
/// of the marshaler's field tables).
fn write_desc_field_value<S: JsonSink>(
    py: Python<'_>,
    field_value: &DescFieldValue,
    value: &Bound<'_, PyAny>,
    sink: &mut S,
    opts: &JsonOpts,
) -> PyResult<()> {
    match field_value {
        DescFieldValue::Scalar { scalar_type, .. } => write_scalar_json(*scalar_type, value, sink),
        DescFieldValue::Enum { enum_, .. } => write_enum_json(py, enum_, value, sink, opts),
        DescFieldValue::Message { message, .. } => {
            write_message_json(py, message, value, sink, opts)
        }
        DescFieldValue::List { element, .. } => {
            let list = value.cast::<PyList>()?;
            sink.begin_array()?;
            for item in list.iter() {
                write_single_desc_value(py, element, &item, sink, opts)?;
            }
            sink.end_array()?;
            Ok(())
        }
        DescFieldValue::Map { .. } => {
            Err(PyValueError::new_err("map extensions are not supported"))
        }
    }
}

fn write_single_desc_value<S: JsonSink>(
    py: Python<'_>,
    element: &DescSingleValue,
    value: &Bound<'_, PyAny>,
    sink: &mut S,
    opts: &JsonOpts,
) -> PyResult<()> {
    match element {
        DescSingleValue::Scalar(scalar) => write_scalar_json(*scalar, value, sink),
        DescSingleValue::Enum(enum_) => write_enum_json(py, enum_, value, sink, opts),
        DescSingleValue::Message { message, .. } => {
            write_message_json(py, message, value, sink, opts)
        }
    }
}

fn write_message_json<S: JsonSink>(
    py: Python<'_>,
    message_desc: &DescMessage,
    value: &Bound<'_, PyAny>,
    sink: &mut S,
    opts: &JsonOpts,
) -> PyResult<()> {
    let expected_type = message_desc.get_python_type(py);
    if !value.is_instance(expected_type)? {
        return Err(message_value_type_error(expected_type, value));
    }
    let native = value.cast::<NativeMessage>()?;
    let marshaler = message_desc.get_marshaler(py)?;
    marshaler.write_json(py, native, sink, opts)
}

fn write_enum_json<S: JsonSink>(
    py: Python<'_>,
    enum_desc: &DescEnum,
    value: &Bound<'_, PyAny>,
    sink: &mut S,
    opts: &JsonOpts,
) -> PyResult<()> {
    if value.is_instance_of::<PyBool>() || !value.is_instance_of::<PyInt>() {
        return Err(PyTypeError::new_err(format!(
            "expected int for enum {}, got {}",
            enum_desc.type_name.bind(py).to_str()?,
            value.get_type()
        )));
    }
    if let Ok(number) = value.extract::<i32>() {
        if !enum_desc.open && !enum_desc.names_by_number.contains_key(&number) {
            return Err(PyValueError::new_err(format!(
                "invalid enum value {number} for enum {}",
                enum_desc.type_name.bind(py).to_str()?
            )));
        }
        if enum_desc.is_null_value {
            return sink.null();
        }
        if opts.print_enums_as_ints {
            return sink.i64(i64::from(number));
        }
        if let Some(name) = enum_desc.names_by_number.get(&number) {
            return sink.py_str_value(name.bind(py));
        }
        // Open enum, unknown value: emit the bare integer.
        sink.i64(i64::from(number))
    } else {
        // An int outside i32 range: only valid for open enums (a closed
        // enum would reject the unknown value).
        if !enum_desc.open {
            return Err(PyValueError::new_err(format!(
                "invalid enum value {} for enum {}",
                value.str()?,
                enum_desc.type_name.bind(py).to_str()?
            )));
        }
        sink.py_number(value)
    }
}

fn write_scalar_json<S: JsonSink>(
    scalar: ScalarType,
    value: &Bound<'_, PyAny>,
    sink: &mut S,
) -> PyResult<()> {
    match scalar {
        ScalarType::Bool => {
            if !value.is_instance_of::<PyBool>() {
                return Err(type_got("expected bool", value));
            }
            sink.bool(value.extract::<bool>()?)
        }
        ScalarType::Int32 | ScalarType::Sint32 | ScalarType::Sfixed32 => {
            require_int(value)?;
            sink.i64(extract_ranged(value, INT32_MIN, INT32_MAX, "int32")?)
        }
        ScalarType::Uint32 | ScalarType::Fixed32 => {
            require_int(value)?;
            sink.i64(extract_ranged(value, 0, UINT32_MAX, "uint32")?)
        }
        ScalarType::Int64 | ScalarType::Sint64 | ScalarType::Sfixed64 => {
            require_int(value)?;
            let v = value
                .extract::<i64>()
                .map_err(|_| overflow_value(value, "int64"))?;
            let mut buf = itoa::Buffer::new();
            sink.str_value(buf.format(v))
        }
        ScalarType::Uint64 | ScalarType::Fixed64 => {
            require_int(value)?;
            let v = value
                .extract::<u64>()
                .map_err(|_| overflow_value(value, "uint64"))?;
            let mut buf = itoa::Buffer::new();
            sink.str_value(buf.format(v))
        }
        ScalarType::Float => {
            let f = require_float(value)?;
            if f.is_finite() && !(FLOAT32_MIN..=FLOAT32_MAX).contains(&f) {
                return Err(overflow_value(value, "float"));
            }
            write_double(value, f, sink)
        }
        ScalarType::Double => {
            let f = require_float(value)?;
            write_double(value, f, sink)
        }
        ScalarType::String => {
            let s = value
                .cast::<PyString>()
                .map_err(|_| type_got("expected str", value))?;
            sink.py_str_value(s)
        }
        ScalarType::Bytes => {
            let bytes = extract_bytes(value)?;
            sink.str_value(&BASE64_STANDARD.encode(&bytes))
        }
    }
}

/// Writes a double/float value: non-finite as `ProtoJSON` string literals, finite
/// via the Python object's `repr` (matching `json.dumps`, and preserving
/// int-valued doubles).
fn write_double<S: JsonSink>(value: &Bound<'_, PyAny>, f: f64, sink: &mut S) -> PyResult<()> {
    if f.is_nan() {
        sink.str_value("NaN")
    } else if f.is_infinite() {
        sink.str_value(if f > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        sink.py_number(value)
    }
}

fn write_map_key<S: JsonSink>(
    key_type: ScalarType,
    key: &Bound<'_, PyAny>,
    sink: &mut S,
) -> PyResult<()> {
    match key_type {
        ScalarType::String => sink.key_str(key.cast::<PyString>()?.to_str()?),
        ScalarType::Bool => sink.key_str(if key.extract::<bool>()? {
            "true"
        } else {
            "false"
        }),
        _ => sink.key_str(key.str()?.to_str()?),
    }
}

fn require_int(value: &Bound<'_, PyAny>) -> PyResult<()> {
    if value.is_instance_of::<PyBool>() || !value.is_instance_of::<PyInt>() {
        return Err(type_got("expected int", value));
    }
    Ok(())
}

fn require_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    if value.is_instance_of::<PyBool>() {
        return Err(type_got("expected float", value));
    }
    value
        .extract::<f64>()
        .map_err(|_| type_got("expected float", value))
}

fn extract_ranged(value: &Bound<'_, PyAny>, min: i64, max: i64, ty: &str) -> PyResult<i64> {
    match value.extract::<i64>() {
        Ok(v) if v >= min && v < max => Ok(v),
        _ => Err(overflow_value(value, ty)),
    }
}

fn extract_bytes(value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(bytes) = value.cast::<PyBytes>() {
        Ok(bytes.as_bytes().to_vec())
    } else if let Ok(bytearray) = value.cast::<PyByteArray>() {
        Ok(bytearray.to_vec())
    } else {
        Err(type_got("expected bytes", value))
    }
}

fn type_got(prefix: &str, value: &Bound<'_, PyAny>) -> PyErr {
    PyTypeError::new_err(format!("{prefix}, got {}", value.get_type()))
}

fn overflow_value(value: &Bound<'_, PyAny>, ty: &str) -> PyErr {
    match value.str() {
        Ok(s) => PyOverflowError::new_err(format!("value {s} out of range for {ty}")),
        Err(err) => err,
    }
}

fn message_value_type_error(expected_type: &Bound<'_, PyType>, value: &Bound<'_, PyAny>) -> PyErr {
    let py = expected_type.py();
    let constants = match Constants::get(py) {
        Ok(constants) => constants,
        Err(err) => return err,
    };
    let expected_name = expected_type
        .getattr(&constants.desc)
        .and_then(|desc| desc.getattr(&constants.type_name))
        .and_then(|name| Ok(name.str()?.to_string()));
    let expected_name = match expected_name {
        Ok(name) => name,
        Err(err) => return err,
    };
    // If the value is itself a message, report its type_name; else its type.
    if let Ok(other_desc) = value
        .getattr(&constants.desc)
        .and_then(|desc| desc.getattr(&constants.type_name))
    {
        match other_desc.str() {
            Ok(other) => PyTypeError::new_err(format!("expected '{expected_name}', got '{other}'")),
            Err(err) => err,
        }
    } else {
        PyTypeError::new_err(format!(
            "expected '{expected_name}', got {}",
            value.get_type()
        ))
    }
}

fn type_url_to_name(url: &str) -> PyResult<&str> {
    let name = match url.rfind('/') {
        Some(index) => &url[index + 1..],
        None => url,
    };
    if name.is_empty() {
        return Err(PyValueError::new_err(format!("invalid type url: {url}")));
    }
    Ok(name)
}
