//! Well-known-type classification for a message type.
//!
//! Mirrors `match_wkt` in `protobuf._wkt_registry`: a message is a WKT only if
//! its fully-qualified name and descriptor file are under `google.protobuf.` /
//! `google/protobuf/` and its fields have the expected shape. The classification
//! is stored on the marshaler (which holds the complete declaration-order field
//! list) rather than on `DescMessage`, whose per-reference memoization would hit
//! the recursive-message initialization hazard.
//!
//! Field positions are stored as indices into the declaration-order field list,
//! which matches the serializer's and parser's field ordering.

use std::collections::HashMap;

use pyo3::{
    Bound, PyAny, PyResult, Python,
    types::{PyAnyMethods as _, PyString, PyStringMethods as _},
};

use crate::{
    constants::Constants,
    descriptor::{DescField, DescFieldValue, DescSingleValue, ScalarType},
};

/// Well-known-type classification with pre-resolved field indices.
pub(crate) enum WktKind {
    /// Not a well-known type; use the generic path.
    None,
    /// `google.protobuf.Timestamp`.
    Timestamp { seconds: usize, nanos: usize },
    /// `google.protobuf.Duration`.
    Duration { seconds: usize, nanos: usize },
    /// `google.protobuf.Any`.
    Any { type_url: usize, value: usize },
    /// `google.protobuf.FieldMask`.
    FieldMask { paths: usize },
    /// `google.protobuf.Struct`.
    Struct { fields: usize },
    /// `google.protobuf.ListValue`.
    ListValue { values: usize },
    /// `google.protobuf.Value`.
    Value {
        null_value: usize,
        number_value: usize,
        string_value: usize,
        bool_value: usize,
        struct_value: usize,
        list_value: usize,
    },
    /// A wrapper type (exactly one scalar field named `value`).
    Wrapper { field: usize, scalar: ScalarType },
    /// `google.protobuf.FileDescriptorSet` (no special JSON for its own fields,
    /// but classified so Any value-wrapping can detect it).
    FileDescriptorSet,
}

impl WktKind {
    /// Classifies a message type from its descriptor and parsed fields.
    pub(crate) fn detect(
        py: Python<'_>,
        message_desc: &Bound<'_, PyAny>,
        fields: &[DescField],
        constants: &Constants,
    ) -> PyResult<WktKind> {
        let type_name_any = message_desc.getattr(&constants.type_name)?;
        let type_name = type_name_any.cast::<PyString>()?;
        let type_name = type_name.to_str()?;
        if !type_name.starts_with("google.protobuf.") {
            return Ok(WktKind::None);
        }
        let file_name_any = message_desc
            .getattr(&constants.file)?
            .getattr(&constants.name)?;
        if !file_name_any
            .cast::<PyString>()?
            .to_str()?
            .starts_with("google/protobuf/")
        {
            return Ok(WktKind::None);
        }

        // Proto field name -> declaration-order index.
        let mut by_name: HashMap<String, usize> = HashMap::with_capacity(fields.len());
        for (i, field) in fields.iter().enumerate() {
            by_name.insert(field.name.bind(py).to_str()?.to_owned(), i);
        }

        let kind = match type_name {
            "google.protobuf.Timestamp" => match ts_dur_indices(fields, &by_name) {
                Some((seconds, nanos)) => WktKind::Timestamp { seconds, nanos },
                None => WktKind::None,
            },
            "google.protobuf.Duration" => match ts_dur_indices(fields, &by_name) {
                Some((seconds, nanos)) => WktKind::Duration { seconds, nanos },
                None => WktKind::None,
            },
            "google.protobuf.Any" => detect_any(fields, &by_name),
            "google.protobuf.FieldMask" => detect_field_mask(fields, &by_name),
            "google.protobuf.Struct" => detect_struct(fields, &by_name),
            "google.protobuf.ListValue" => detect_list_value(fields, &by_name),
            "google.protobuf.Value" => detect_value(fields, &by_name),
            "google.protobuf.FileDescriptorSet" => detect_file_descriptor_set(fields, &by_name),
            _ => detect_wrapper(fields, &by_name),
        };
        Ok(kind)
    }
}

fn idx(by_name: &HashMap<String, usize>, name: &str) -> Option<usize> {
    by_name.get(name).copied()
}

fn is_scalar(value: &DescFieldValue, want: ScalarType) -> bool {
    matches!(value, DescFieldValue::Scalar { scalar_type, .. } if *scalar_type == want)
}

fn is_enum(value: &DescFieldValue) -> bool {
    matches!(value, DescFieldValue::Enum { .. })
}

fn is_message(value: &DescFieldValue) -> bool {
    matches!(value, DescFieldValue::Message { .. })
}

fn is_list_scalar(value: &DescFieldValue, want: ScalarType) -> bool {
    matches!(value, DescFieldValue::List { element: DescSingleValue::Scalar(t), .. } if *t == want)
}

fn is_list_message(value: &DescFieldValue) -> bool {
    matches!(
        value,
        DescFieldValue::List {
            element: DescSingleValue::Message { .. },
            ..
        }
    )
}

fn is_map_key(value: &DescFieldValue, want: ScalarType) -> bool {
    matches!(value, DescFieldValue::Map { key_type, .. } if *key_type == want)
}

/// Timestamp/Duration share the same shape: int64 `seconds` + int32 `nanos`.
fn ts_dur_indices(
    fields: &[DescField],
    by_name: &HashMap<String, usize>,
) -> Option<(usize, usize)> {
    let seconds = idx(by_name, "seconds")?;
    let nanos = idx(by_name, "nanos")?;
    if is_scalar(&fields[seconds].value, ScalarType::Int64)
        && is_scalar(&fields[nanos].value, ScalarType::Int32)
    {
        Some((seconds, nanos))
    } else {
        None
    }
}

fn detect_any(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    let (Some(type_url), Some(value)) = (idx(by_name, "type_url"), idx(by_name, "value")) else {
        return WktKind::None;
    };
    if is_scalar(&fields[type_url].value, ScalarType::String)
        && is_scalar(&fields[value].value, ScalarType::Bytes)
    {
        WktKind::Any { type_url, value }
    } else {
        WktKind::None
    }
}

fn detect_field_mask(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    let Some(paths) = idx(by_name, "paths") else {
        return WktKind::None;
    };
    if is_list_scalar(&fields[paths].value, ScalarType::String) {
        WktKind::FieldMask { paths }
    } else {
        WktKind::None
    }
}

fn detect_struct(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    let Some(fields_idx) = idx(by_name, "fields") else {
        return WktKind::None;
    };
    if is_map_key(&fields[fields_idx].value, ScalarType::String) {
        WktKind::Struct { fields: fields_idx }
    } else {
        WktKind::None
    }
}

fn detect_list_value(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    let Some(values) = idx(by_name, "values") else {
        return WktKind::None;
    };
    if is_list_message(&fields[values].value) {
        WktKind::ListValue { values }
    } else {
        WktKind::None
    }
}

fn detect_value(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    let (
        Some(null_value),
        Some(number_value),
        Some(string_value),
        Some(bool_value),
        Some(struct_value),
        Some(list_value),
    ) = (
        idx(by_name, "null_value"),
        idx(by_name, "number_value"),
        idx(by_name, "string_value"),
        idx(by_name, "bool_value"),
        idx(by_name, "struct_value"),
        idx(by_name, "list_value"),
    )
    else {
        return WktKind::None;
    };
    if is_enum(&fields[null_value].value)
        && is_scalar(&fields[number_value].value, ScalarType::Double)
        && is_scalar(&fields[string_value].value, ScalarType::String)
        && is_scalar(&fields[bool_value].value, ScalarType::Bool)
        && is_message(&fields[struct_value].value)
        && is_message(&fields[list_value].value)
    {
        WktKind::Value {
            null_value,
            number_value,
            string_value,
            bool_value,
            struct_value,
            list_value,
        }
    } else {
        WktKind::None
    }
}

fn detect_file_descriptor_set(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    let Some(file) = idx(by_name, "file") else {
        return WktKind::None;
    };
    if is_list_message(&fields[file].value) {
        WktKind::FileDescriptorSet
    } else {
        WktKind::None
    }
}

fn detect_wrapper(fields: &[DescField], by_name: &HashMap<String, usize>) -> WktKind {
    // Structural fallthrough: exactly one scalar field named `value`.
    if fields.len() != 1 || idx(by_name, "value") != Some(0) {
        return WktKind::None;
    }
    if let DescFieldValue::Scalar { scalar_type, .. } = &fields[0].value {
        WktKind::Wrapper {
            field: 0,
            scalar: *scalar_type,
        }
    } else {
        WktKind::None
    }
}
