use pyo3::prelude::*;

mod attribute_access;
mod bitset;
mod constants;
mod descriptor;
mod json_parse;
mod json_serialize;
mod json_sink;
mod json_source;
mod marshaler;
mod nativemessage;
mod oneof;
mod parser;
mod reverse_buffer;
mod serializer;
mod wkt;
mod wkt_json;

/// A Python module implemented in Rust.
#[pymodule(name = "_protobuf_ext", gil_used = false)]
mod protobuf_python_ext {
    #[allow(clippy::wildcard_imports)]
    use super::*;

    #[pymodule_export]
    use marshaler::MessageMarshaler;

    #[pymodule_export]
    use nativemessage::{NativeMessage, initialize_message_type};

    #[pymodule_export]
    use oneof::Oneof;

    #[pymodule_export]
    use attribute_access::generic_setattr;
}
