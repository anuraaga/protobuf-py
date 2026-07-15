//! Native `ProtoJSON` parsing, generic over a `JsonSource`.
//!
//! Mirrors the pure-Python `protobuf._from_json`. Error text matches the
//! reference exactly where practical. Registry-backed Any/extension handling
//! calls the Python `Registry` object directly (never ported to Rust).

use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use pyo3::{
    Bound, Py, PyAny, PyErr, PyResult, Python,
    exceptions::{PyOverflowError, PyTypeError, PyValueError},
    types::{
        PyAnyMethods as _, PyBool, PyBytes, PyDict, PyDictMethods as _, PyFloat, PyInt, PyList,
        PyListMethods as _, PyString, PyStringMethods as _, PyTypeMethods as _,
    },
};

use crate::{
    descriptor::{DescEnum, ScalarType},
    json_source::{JiterSource, JsonKind, JsonSource, PyTreeSource},
    marshaler::MessageMarshaler,
    nativemessage::NativeMessage,
    oneof::Oneof,
    parser::{FieldParser, FieldParserValue, ParserFieldType},
    wkt_registry::WktKind,
};

const DEPTH_LIMIT: usize = 100;
const INT32_MIN: i64 = -(1 << 31);
const INT32_MAX: i64 = 1 << 31;
const UINT32_MAX: i64 = 1 << 32;
const FLOAT32_MAX: f64 = 3.402_823_466_385_288_6e38;
const FLOAT32_MIN: f64 = -3.402_823_466_385_288_6e38;

/// Options controlling JSON parsing.
pub(crate) struct FromJsonOpts {
    pub(crate) ignore_unknown_fields: bool,
    pub(crate) registry: Option<Py<PyAny>>,
}

#[derive(Clone, Copy)]
enum Exc {
    Value,
    Type,
    Overflow,
}

/// Parses a JSON document (str/bytes) into an existing message.
pub(crate) fn merge_from_json<'py>(
    py: Python<'py>,
    marshaler: &MessageMarshaler,
    message: &Bound<'py, NativeMessage>,
    data: &[u8],
    opts: &FromJsonOpts,
) -> PyResult<()> {
    let mut src = JiterSource::new(py, data);
    read_message(marshaler, message, &mut src, opts, 0)?;
    src.finish()
}

/// Parses a message from a Python JSON tree (`message_from_json_value`).
pub(crate) fn read_message_from_tree<'py>(
    py: Python<'py>,
    marshaler: &MessageMarshaler,
    message: &Bound<'py, NativeMessage>,
    tree: Bound<'py, PyAny>,
    opts: &FromJsonOpts,
) -> PyResult<()> {
    let mut src = PyTreeSource::new(py, tree);
    read_message(marshaler, message, &mut src, opts, 0)
}

/// Reads a message value, dispatching on its well-known-type kind. Ordinary
/// (non-WKT) messages take the generic object path; WKTs go through
/// `WktKind::read_json`.
pub(crate) fn read_message<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    message: &Bound<'py, NativeMessage>,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
) -> PyResult<()> {
    if depth > DEPTH_LIMIT {
        return Err(pyo3::exceptions::PyRecursionError::new_err(format!(
            "exceeded maximum recursion depth {DEPTH_LIMIT} while parsing message"
        )));
    }
    match &marshaler.wkt {
        Some(wkt) => wkt.read_json(marshaler, message, src, opts, depth),
        None => read_generic_object(marshaler, message, src, opts, depth),
    }
}

fn read_generic_object<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    message: &Bound<'py, NativeMessage>,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
) -> PyResult<()> {
    let py = src.py();
    if src.peek()? != JsonKind::Object {
        let value = read_json_value(src)?;
        let qualname = marshaler.python_type.bind(py).qualname()?;
        return Err(PyTypeError::new_err(format!(
            "cannot decode {qualname} from JSON: {}",
            value.str()?
        )));
    }

    let mut all_keys: HashSet<String> = HashSet::new();
    let mut seen_fields: HashMap<u32, String> = HashMap::new();
    let mut seen_oneofs: HashMap<String, String> = HashMap::new();

    let mut key = src.next_object()?;
    while let Some(raw_key) = key {
        if !all_keys.insert(raw_key.clone()) {
            return Err(PyValueError::new_err(format!("duplicate key: {raw_key}")));
        }
        let field_number = marshaler.json_names.get(raw_key.as_str()).copied();
        if let Some(number) = field_number {
            let parser = marshaler
                .parser
                .field(number)
                .ok_or_else(|| PyValueError::new_err("field table lookup failed"))?;
            if let Some(prev) = seen_fields.get(&number) {
                return Err(PyValueError::new_err(format!(
                    "field set multiple times by {prev} and {raw_key}"
                )));
            }
            seen_fields.insert(number, raw_key.clone());

            if let Some(oneof_name) = &parser.oneof_name {
                let is_scalar = matches!(parser.value, FieldParserValue::Scalar(_));
                if is_scalar && src.peek()? == JsonKind::Null {
                    src.next_null()?;
                    key = src.next_key()?;
                    continue;
                }
                let oneof_local = oneof_name.bind(py).to_str()?.to_owned();
                let field_proto_name = parser.name.bind(py).to_str()?.to_owned();
                if let Some(prev) = seen_oneofs.get(&oneof_local) {
                    return Err(PyValueError::new_err(format!(
                        "oneof set multiple times by {prev} and {field_proto_name}"
                    )));
                }
                seen_oneofs.insert(oneof_local, field_proto_name);
            }
            read_field(marshaler, parser, message, src, opts, depth)?;
        } else {
            handle_unknown_key(marshaler, message, &raw_key, src, opts)?;
        }
        key = src.next_key()?;
    }
    Ok(())
}

fn read_field<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    parser: &FieldParser,
    message: &Bound<'py, NativeMessage>,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
) -> PyResult<()> {
    match &parser.type_ {
        ParserFieldType::Singular {
            oneof_attr,
            requires_presence,
        } => read_singular(
            marshaler,
            parser,
            message,
            src,
            opts,
            depth,
            oneof_attr.as_ref(),
            *requires_presence,
        ),
        ParserFieldType::List { .. } => read_list(marshaler, parser, message, src, opts, depth),
        ParserFieldType::Map {
            key_type,
            value_parser,
            ..
        } => read_map(
            marshaler,
            parser,
            message,
            src,
            opts,
            depth,
            *key_type,
            value_parser,
        ),
    }
}

#[allow(clippy::too_many_arguments, reason = "internal parser")]
fn read_singular<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    parser: &FieldParser,
    message: &Bound<'py, NativeMessage>,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
    oneof_attr: Option<&crate::attribute_access::AttributeAccess>,
    requires_presence: bool,
) -> PyResult<()> {
    let py = src.py();
    match &parser.value {
        FieldParserValue::Scalar(scalar) => {
            if src.peek()? == JsonKind::Null {
                src.next_null()?;
                del_singular(
                    py,
                    parser,
                    message,
                    oneof_attr,
                    &scalar.zero_value(py).into_bound(py),
                )?;
                return Ok(());
            }
            let value = read_scalar(marshaler, parser.name.bind(py), src, *scalar)?;
            parser.assign_singular(py, message, &value, oneof_attr, requires_presence)
        }
        FieldParserValue::Enum(enum_) => {
            if src.peek()? == JsonKind::Null && !enum_.is_null_value {
                src.next_null()?;
                del_singular(
                    py,
                    parser,
                    message,
                    oneof_attr,
                    &enum_.zero_value.bind(py).clone(),
                )?;
                return Ok(());
            }
            if let Some(value) = read_enum(enum_, src, opts)? {
                parser.assign_singular(py, message, &value, oneof_attr, requires_presence)?;
            }
            Ok(())
        }
        FieldParserValue::Message {
            message: msg_desc, ..
        } => {
            let inner = msg_desc.get_marshaler(py)?;
            let is_value = matches!(inner.wkt.as_deref(), Some(WktKind::Value(_)));
            if src.peek()? == JsonKind::Null && !is_value {
                src.next_null()?;
                del_singular(py, parser, message, oneof_attr, &py.None().into_bound(py))?;
                return Ok(());
            }
            let existing = parser.get_field_value(py, message)?;
            let target = match existing {
                Some(value) if !value.is_none() => value.cast_into::<NativeMessage>()?,
                _ => inner.new_empty_message(py, msg_desc.get_python_type(py))?,
            };
            read_message(inner, &target, src, opts, depth + 1)?;
            parser.assign_singular(py, message, target.as_any(), oneof_attr, requires_presence)
        }
    }
}

/// Clears a singular field for a resetting `null`, matching `_del_member`.
fn del_singular<'py>(
    py: Python<'py>,
    parser: &FieldParser,
    message: &Bound<'py, NativeMessage>,
    oneof_attr: Option<&crate::attribute_access::AttributeAccess>,
    default: &Bound<'py, PyAny>,
) -> PyResult<()> {
    if let Some(oneof_attr) = oneof_attr {
        let current = oneof_attr.get(py, message.as_any())?;
        if let Ok(oneof) = current.cast::<Oneof>()
            && oneof
                .get()
                .field
                .bind(py)
                .eq(parser.local_name_py.bind(py))?
        {
            oneof_attr.set(message.as_any(), py.None().bind(py))?;
        }
    } else {
        parser.attr.set(message.as_any(), default)?;
        message.get().clear_present_field(parser.number);
    }
    Ok(())
}

fn read_list<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    parser: &FieldParser,
    message: &Bound<'py, NativeMessage>,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
) -> PyResult<()> {
    let py = src.py();
    if src.peek()? == JsonKind::Null {
        src.next_null()?;
        return Ok(());
    }
    if src.peek()? != JsonKind::Array {
        return Err(field_error(
            py,
            marshaler,
            parser.name.bind(py),
            &format!("expected list got {}", read_json_value(src)?.get_type()),
            Exc::Type,
        ));
    }
    let list_obj = parser.attr.get(py, message.as_any())?;
    let list = list_obj.cast::<PyList>()?;
    let mut has = src.next_array()?;
    while has {
        if let Some(value) = read_container_item(
            marshaler,
            parser.name.bind(py),
            &parser.value,
            src,
            opts,
            depth,
            false,
        )? {
            list.append(value)?;
        }
        has = src.array_step()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, reason = "internal parser")]
fn read_map<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    parser: &FieldParser,
    message: &Bound<'py, NativeMessage>,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
    key_type: ScalarType,
    value_parser: &FieldParser,
) -> PyResult<()> {
    let py = src.py();
    if src.peek()? == JsonKind::Null {
        src.next_null()?;
        return Ok(());
    }
    if src.peek()? != JsonKind::Object {
        return Err(field_error(
            py,
            marshaler,
            parser.name.bind(py),
            &format!("expected dict got {}", read_json_value(src)?.get_type()),
            Exc::Type,
        ));
    }
    let dict_obj = parser.attr.get(py, message.as_any())?;
    let dict = dict_obj.cast::<PyDict>()?;
    let mut key = src.next_object()?;
    while let Some(raw_key) = key {
        let map_key = read_map_key(py, marshaler, parser.name.bind(py), key_type, &raw_key)?;
        if let Some(value) = read_container_item(
            marshaler,
            parser.name.bind(py),
            &value_parser.value,
            src,
            opts,
            depth,
            true,
        )? {
            dict.set_item(map_key, value)?;
        }
        key = src.next_key()?;
    }
    Ok(())
}

/// Reads a list element or map value, matching `_read_container_item`.
#[allow(clippy::too_many_arguments, reason = "internal parser")]
fn read_container_item<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    element: &FieldParserValue,
    src: &mut R,
    opts: &FromJsonOpts,
    depth: usize,
    is_map: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = src.py();
    let is_null = src.peek()? == JsonKind::Null;
    match element {
        FieldParserValue::Scalar(scalar) if !is_null => {
            Ok(Some(read_scalar(marshaler, field_name, src, *scalar)?))
        }
        FieldParserValue::Message {
            message: msg_desc, ..
        } => {
            let inner = msg_desc.get_marshaler(py)?;
            let is_value = matches!(inner.wkt.as_deref(), Some(WktKind::Value(_)));
            if is_null && !is_value {
                src.next_null()?;
                return Err(container_null_error(py, marshaler, field_name, is_map));
            }
            let target = inner.new_empty_message(py, msg_desc.get_python_type(py))?;
            read_message(inner, &target, src, opts, depth + 1)?;
            Ok(Some(target.into_any()))
        }
        FieldParserValue::Enum(enum_) if !is_null || enum_.is_null_value => {
            read_enum(enum_, src, opts)
        }
        _ => {
            // Resetting null for a list item / map value: error.
            src.next_null()?;
            Err(container_null_error(py, marshaler, field_name, is_map))
        }
    }
}

fn container_null_error(
    py: Python<'_>,
    marshaler: &MessageMarshaler,
    field_name: &Bound<'_, PyString>,
    is_map: bool,
) -> PyErr {
    let what = if is_map { "map value" } else { "list item" };
    field_error(
        py,
        marshaler,
        field_name,
        &format!("unexpected null value for {what}"),
        Exc::Value,
    )
}

fn read_map_key<'py>(
    py: Python<'py>,
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    key_type: ScalarType,
    raw_key: &str,
) -> PyResult<Bound<'py, PyAny>> {
    match key_type {
        ScalarType::Bool => match raw_key {
            "true" => Ok(PyBool::new(py, true).to_owned().into_any()),
            "false" => Ok(PyBool::new(py, false).to_owned().into_any()),
            other => Err(field_error(
                py,
                marshaler,
                field_name,
                &format!("unexpected bool map key value {other}"),
                Exc::Value,
            )),
        },
        ScalarType::String => Ok(PyString::new(py, raw_key).into_any()),
        _ => parse_int_string(py, marshaler, field_name, raw_key, key_type),
    }
}

/// Reads a scalar value, matching `_read_scalar`.
pub(crate) fn read_scalar<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    src: &mut R,
    scalar: ScalarType,
) -> PyResult<Bound<'py, PyAny>> {
    let py = src.py();
    match scalar {
        ScalarType::Bool => {
            if src.peek()? != JsonKind::Bool {
                let value = read_json_value(src)?;
                return Err(field_error(
                    py,
                    marshaler,
                    field_name,
                    &format!("unexpected json type: {}", value.get_type()),
                    Exc::Type,
                ));
            }
            Ok(PyBool::new(py, src.next_bool()?).to_owned().into_any())
        }
        ScalarType::Float => {
            let value = parse_float(marshaler, field_name, src)?;
            if value.is_finite() && !(FLOAT32_MIN..=FLOAT32_MAX).contains(&value) {
                return Err(field_error(
                    py,
                    marshaler,
                    field_name,
                    &format!("float value out of range: {value}"),
                    Exc::Overflow,
                ));
            }
            Ok(PyFloat::new(py, value).into_any())
        }
        ScalarType::Double => {
            Ok(PyFloat::new(py, parse_float(marshaler, field_name, src)?).into_any())
        }
        ScalarType::String => Ok(read_string(marshaler, field_name, src)?.into_any()),
        ScalarType::Bytes => read_bytes(marshaler, field_name, src),
        _ => read_int(marshaler, field_name, src, scalar),
    }
}

fn read_string<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    src: &mut R,
) -> PyResult<Bound<'py, PyString>> {
    let py = src.py();
    if src.peek()? != JsonKind::String {
        let value = read_json_value(src)?;
        return Err(field_error(
            py,
            marshaler,
            field_name,
            &format!("expected string got: {}", value.get_type()),
            Exc::Type,
        ));
    }
    src.next_str()
}

fn read_bytes<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    src: &mut R,
) -> PyResult<Bound<'py, PyAny>> {
    let py = src.py();
    if src.peek()? != JsonKind::String {
        let value = read_json_value(src)?;
        return Err(field_error(
            py,
            marshaler,
            field_name,
            &format!("expected base64-encoded string got: {}", value.get_type()),
            Exc::Type,
        ));
    }
    let text = src.next_string()?;
    // Autodetect standard vs URL-safe alphabet, matching `_read_scalar`. The
    // engines are lenient about padding and non-canonical trailing bits, like
    // Python's `base64.b64decode(..., validate=True)`.
    let decoded = if text.contains('-') || text.contains('_') {
        base64_url_safe().decode(text.as_str())
    } else {
        base64_standard().decode(text.as_str())
    };
    match decoded {
        Ok(bytes) => Ok(PyBytes::new(py, &bytes).into_any()),
        Err(_) => Err(field_error(
            py,
            marshaler,
            field_name,
            "invalid base64 data",
            Exc::Value,
        )),
    }
}

/// Lenient base64 decode config matching Python's `b64decode`: optional padding,
/// non-canonical trailing bits allowed.
fn base64_decode_config() -> base64::engine::GeneralPurposeConfig {
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
}

fn base64_standard() -> base64::engine::GeneralPurpose {
    base64::engine::GeneralPurpose::new(&base64::alphabet::STANDARD, base64_decode_config())
}

fn base64_url_safe() -> base64::engine::GeneralPurpose {
    base64::engine::GeneralPurpose::new(&base64::alphabet::URL_SAFE, base64_decode_config())
}

/// Parses a float/double, matching `_parse_float`.
fn parse_float<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    src: &mut R,
) -> PyResult<f64> {
    let py = src.py();
    match src.peek()? {
        JsonKind::Number => {
            let value = src.next_float()?;
            if !value.is_finite() {
                return Err(field_error(
                    py,
                    marshaler,
                    field_name,
                    "unexpected infinite/NaN number",
                    Exc::Value,
                ));
            }
            Ok(value)
        }
        JsonKind::String => {
            let text = src.next_string()?;
            match text.as_str() {
                "Infinity" => Ok(f64::INFINITY),
                "-Infinity" => Ok(f64::NEG_INFINITY),
                "NaN" => Ok(f64::NAN),
                _ => {
                    if text.is_empty() || text.trim() != text {
                        return Err(field_error(
                            py,
                            marshaler,
                            field_name,
                            &format!("invalid float/double value: {text}"),
                            Exc::Value,
                        ));
                    }
                    match text.parse::<f64>() {
                        Ok(value) if value.is_finite() => Ok(value),
                        Ok(_) => Err(field_error(
                            py,
                            marshaler,
                            field_name,
                            "unexpected infinite/NaN number",
                            Exc::Value,
                        )),
                        Err(_) => Err(field_error(
                            py,
                            marshaler,
                            field_name,
                            &format!("invalid float/double value: {text}"),
                            Exc::Value,
                        )),
                    }
                }
            }
        }
        _ => {
            let value = read_json_value(src)?;
            Err(field_error(
                py,
                marshaler,
                field_name,
                &format!("unexpected json type: {}", value.get_type()),
                Exc::Type,
            ))
        }
    }
}

/// Parses an integer scalar, matching `_read_int`/`_parse_int`.
fn read_int<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    src: &mut R,
    int_type: ScalarType,
) -> PyResult<Bound<'py, PyAny>> {
    let py = src.py();
    let value = match src.peek()? {
        JsonKind::Number => {
            let number = src.next_number()?;
            if number.is_instance_of::<PyInt>() {
                number
            } else {
                let float_value = number.extract::<f64>()?;
                if float_value.fract() != 0.0 {
                    return Err(field_error(
                        py,
                        marshaler,
                        field_name,
                        &format!("expected integer, got non-integer float: {}", number.str()?),
                        Exc::Value,
                    ));
                }
                py_int_from_float(py, &number)?
            }
        }
        JsonKind::String => {
            let text = src.next_string()?;
            return parse_int_string(py, marshaler, field_name, &text, int_type)
                .and_then(|value| range_check_int(py, marshaler, field_name, value, int_type));
        }
        _ => {
            let value = read_json_value(src)?;
            return Err(field_error(
                py,
                marshaler,
                field_name,
                &format!("unexpected json type: {}", value.get_type()),
                Exc::Type,
            ));
        }
    };
    range_check_int(py, marshaler, field_name, value, int_type)
}

/// Parses an integer from a quoted string, matching the `str` branch of
/// `_parse_int` (then the caller range-checks).
fn parse_int_string<'py>(
    py: Python<'py>,
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    text: &str,
    _int_type: ScalarType,
) -> PyResult<Bound<'py, PyAny>> {
    if text.is_empty() || text.trim() != text {
        return Err(field_error(
            py,
            marshaler,
            field_name,
            &format!("invalid integer value: {text}"),
            Exc::Value,
        ));
    }
    let int_type = py.get_type::<PyInt>();
    if let Ok(value) = int_type.call1((text, 10)) {
        return Ok(value);
    }
    // Fall back to a float string that is integer-valued (e.g. "3.0").
    match text.parse::<f64>() {
        Ok(float_value) if float_value.fract() == 0.0 => {
            let float_obj = PyFloat::new(py, float_value);
            py_int_from_float(py, &float_obj)
        }
        _ => Err(field_error(
            py,
            marshaler,
            field_name,
            &format!("invalid integer value: '{text}'"),
            Exc::Value,
        )),
    }
}

fn py_int_from_float<'py>(
    py: Python<'py>,
    value: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    py.get_type::<PyInt>().call1((value,))
}

fn range_check_int<'py>(
    py: Python<'py>,
    marshaler: &MessageMarshaler,
    field_name: &Bound<'py, PyString>,
    value: Bound<'py, PyAny>,
    int_type: ScalarType,
) -> PyResult<Bound<'py, PyAny>> {
    let (ok, name) = match int_type {
        ScalarType::Int32 | ScalarType::Sint32 | ScalarType::Sfixed32 => (
            matches!(value.extract::<i64>(), Ok(v) if (INT32_MIN..INT32_MAX).contains(&v)),
            "int32",
        ),
        ScalarType::Uint32 | ScalarType::Fixed32 => (
            matches!(value.extract::<i64>(), Ok(v) if (0..UINT32_MAX).contains(&v)),
            "uint32",
        ),
        ScalarType::Int64 | ScalarType::Sint64 | ScalarType::Sfixed64 => {
            (value.extract::<i64>().is_ok(), "int64")
        }
        ScalarType::Uint64 | ScalarType::Fixed64 => (value.extract::<u64>().is_ok(), "uint64"),
        _ => (true, ""),
    };
    if ok {
        Ok(value)
    } else {
        Err(field_error(
            py,
            marshaler,
            field_name,
            &format!("value {} out of range for {name}", value.str()?),
            Exc::Overflow,
        ))
    }
}

/// Reads an enum value, matching `_read_enum`. Returns `None` when an unknown
/// value is ignored via `ignore_unknown_fields`.
fn read_enum<'py, R: JsonSource<'py>>(
    enum_desc: &DescEnum,
    src: &mut R,
    opts: &FromJsonOpts,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = src.py();
    match src.peek()? {
        JsonKind::Null => {
            src.next_null()?;
            // `_read_enum` returns the first declared value for null.
            Ok(Some(enum_desc.zero_value.bind(py).clone()))
        }
        JsonKind::Number => {
            let number = src.next_number()?;
            if !number.is_instance_of::<PyInt>() {
                return Err(decode_enum_error(py, enum_desc, &number));
            }
            let Ok(int_value) = number.extract::<i32>() else {
                // Out of i32 range: unknown value.
                if opts.ignore_unknown_fields {
                    return Ok(None);
                }
                return Ok(Some(enum_desc.py_type.bind(py).call1((number,))?));
            };
            if let Some(value) = enum_desc.values.get(&int_value) {
                Ok(Some(value.bind(py).clone()))
            } else if opts.ignore_unknown_fields {
                Ok(None)
            } else {
                // Open enum: succeeds; closed enum: raises via Python enum call.
                Ok(Some(enum_desc.py_type.bind(py).call1((int_value,))?))
            }
        }
        JsonKind::String => {
            let name = src.next_string()?;
            if let Some(number) = enum_desc.numbers_by_name.get(&name) {
                let value = enum_desc
                    .values
                    .get(number)
                    .ok_or_else(|| PyValueError::new_err("enum value table lookup failed"))?;
                Ok(Some(value.bind(py).clone()))
            } else if opts.ignore_unknown_fields {
                Ok(None)
            } else {
                let value = read_json_value_from_string(py, &name);
                Err(decode_enum_error(py, enum_desc, &value))
            }
        }
        _ => {
            let value = read_json_value(src)?;
            Err(decode_enum_error(py, enum_desc, &value))
        }
    }
}

fn decode_enum_error(py: Python<'_>, enum_desc: &DescEnum, value: &Bound<'_, PyAny>) -> PyErr {
    let type_name = enum_desc.type_name.bind(py).to_str().unwrap_or_default();
    match value.str() {
        Ok(text) => PyValueError::new_err(format!("cannot decode {type_name} from JSON: {text}")),
        Err(err) => err,
    }
}

fn read_json_value_from_string<'py>(py: Python<'py>, text: &str) -> Bound<'py, PyAny> {
    PyString::new(py, text).into_any()
}

fn handle_unknown_key<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    message: &Bound<'py, NativeMessage>,
    raw_key: &str,
    src: &mut R,
    opts: &FromJsonOpts,
) -> PyResult<()> {
    let py = src.py();
    if raw_key.starts_with('[')
        && raw_key.ends_with(']')
        && let Some(registry) = &opts.registry
    {
        let registry = registry.bind(py);
        let ext_name = &raw_key[1..raw_key.len() - 1];
        let extension = registry.call_method1(&marshaler.constants.extension, (ext_name,))?;
        if !extension.is_none() {
            // Only read if the extendee matches this message.
            let extendee = extension.getattr("extendee")?;
            let extendee_name = extendee.getattr(&marshaler.constants.type_name)?;
            let extendee_name = extendee_name.cast::<PyString>()?.to_str()?;
            if extendee_name == message_type_name(py, marshaler)? {
                read_extension(marshaler, message, &extension, src, opts)?;
            } else {
                src.skip()?;
            }
            return Ok(());
        }
    }
    if opts.ignore_unknown_fields {
        src.skip()
    } else {
        Err(PyValueError::new_err(format!(
            "cannot decode {} from JSON: key: '{raw_key}' is unknown",
            message_type_name(py, marshaler)?
        )))
    }
}

fn read_extension<'py, R: JsonSource<'py>>(
    marshaler: &MessageMarshaler,
    message: &Bound<'py, NativeMessage>,
    extension: &Bound<'py, PyAny>,
    src: &mut R,
    opts: &FromJsonOpts,
) -> PyResult<()> {
    // Materialize the extension value and delegate to the Python read path,
    // reusing the pure-Python extension logic through the message's __setitem__.
    let py = src.py();
    let value = read_json_value(src)?;
    let ext_type = extension.getattr(&marshaler.constants.type_)?;
    // A null clears the extension; otherwise route through message_from_json_value
    // on the extension's value descriptor.
    let _ = opts;
    if value.is_none() {
        message.as_any().del_item(&ext_type)?;
        return Ok(());
    }
    // Defer to Python's _read_extension for correctness (rare path).
    let from_json = py.import("protobuf._from_json")?;
    let read_ext = from_json.getattr("_read_extension")?;
    let opts_obj = make_from_json_options(py, opts)?;
    read_ext.call1((message, extension, value, opts_obj))?;
    Ok(())
}

fn make_from_json_options<'py>(
    py: Python<'py>,
    opts: &FromJsonOpts,
) -> PyResult<Bound<'py, PyAny>> {
    let from_json = py.import("protobuf._from_json")?;
    let options_cls = from_json.getattr("FromJsonOptions")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("ignore_unknown_fields", opts.ignore_unknown_fields)?;
    kwargs.set_item(
        "registry",
        opts.registry
            .as_ref()
            .map_or_else(|| py.None(), |r| r.clone_ref(py)),
    )?;
    options_cls.call((), Some(&kwargs))
}

/// Materializes the next JSON value as a Python object (for Any subtrees and
/// error type reporting).
pub(crate) fn read_json_value<'py, R: JsonSource<'py>>(src: &mut R) -> PyResult<Bound<'py, PyAny>> {
    let py = src.py();
    match src.peek()? {
        JsonKind::Null => {
            src.next_null()?;
            Ok(py.None().into_bound(py))
        }
        JsonKind::Bool => Ok(PyBool::new(py, src.next_bool()?).to_owned().into_any()),
        JsonKind::Number => src.next_number(),
        JsonKind::String => Ok(src.next_str()?.into_any()),
        JsonKind::Array => {
            let list = PyList::empty(py);
            let mut has = src.next_array()?;
            while has {
                list.append(read_json_value(src)?)?;
                has = src.array_step()?;
            }
            Ok(list.into_any())
        }
        JsonKind::Object => {
            let dict = PyDict::new(py);
            let mut key = src.next_object()?;
            while let Some(raw_key) = key {
                let value = read_json_value(src)?;
                dict.set_item(PyString::new(py, &raw_key), value)?;
                key = src.next_key()?;
            }
            Ok(dict.into_any())
        }
    }
}

// ---- helpers ----

pub(crate) fn message_type_name(py: Python<'_>, marshaler: &MessageMarshaler) -> PyResult<String> {
    marshaler
        .python_type
        .bind(py)
        .getattr(&marshaler.constants.desc)?
        .getattr(&marshaler.constants.type_name)?
        .extract()
}

fn field_error(
    py: Python<'_>,
    marshaler: &MessageMarshaler,
    field_name: &Bound<'_, PyString>,
    message: &str,
    exc: Exc,
) -> PyErr {
    let name = field_name.to_str().unwrap_or_default();
    let parent = message_type_name(py, marshaler).unwrap_or_default();
    let full = format!("{message} for field {parent}.{name}");
    match exc {
        Exc::Value => PyValueError::new_err(full),
        Exc::Type => PyTypeError::new_err(full),
        Exc::Overflow => PyOverflowError::new_err(full),
    }
}
