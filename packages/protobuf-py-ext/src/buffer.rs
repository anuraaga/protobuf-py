use bytes::Bytes;
use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyBufferError;
use pyo3::{Borrowed, FromPyObject, PyAny, PyErr, PyResult};

// Wrapper to pass to from_owner satisfying the orphan rule.
struct BufferOwner(PyBuffer<u8>);

impl AsRef<[u8]> for BufferOwner {
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.0.buf_ptr() as *const u8, self.0.len_bytes()) }
    }
}

/// A Bytes view of a Python buffer protocol object.
pub(crate) struct Buffer(Bytes);

impl Buffer {
    pub(crate) fn into_inner(self) -> Bytes {
        self.0
    }
}

impl<'a, 'py> FromPyObject<'a, 'py> for Buffer {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> PyResult<Self> {
        if let Ok(bytes) = obj.extract::<Bytes>() {
            return Ok(Buffer(bytes));
        }
        let buffer = PyBuffer::<u8>::get(&obj)?;
        if !buffer.is_c_contiguous() {
            return Err(PyBufferError::new_err("buffer is not contiguous"));
        }
        Ok(Buffer(Bytes::from_owner(BufferOwner(buffer))))
    }
}
