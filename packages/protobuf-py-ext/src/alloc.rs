//! A global allocator backed by `CPython`'s raw memory domain (`PyMem_Raw*`).
//!
//! Routing Rust allocations through the interpreter's allocator makes them
//! visible to `tracemalloc` and other `PyMem_SetAllocator` hooks. On
//! free-threaded builds it also picks up a thread-scalable allocator without
//! linking one into the extension. On GIL builds the raw
//! domain is plain `malloc` — the same allocator Rust uses by default — so
//! the only cost is a function-pointer indirection on the small number of
//! Rust allocations per operation, which does not measure in benchmarks.
//!
//! `PyMem_Raw*` is not in the limited API before 3.13, so abi3 wheels fall
//! back to Rust's default allocator and do not show up in tracemalloc.

use std::alloc::{GlobalAlloc, Layout};
use std::ptr;

use pyo3::ffi;

#[global_allocator]
static ALLOCATOR: PyMemRaw = PyMemRaw;

struct PyMemRaw;

/// Alignment the raw allocator provides on its own (that of `max_align_t`).
const RAW_ALIGN: usize = if size_of::<usize>() >= 8 { 16 } else { 8 };

/// Space stashed in front of an over-aligned block to recover the pointer
/// originally returned by [`ffi::PyMem_RawMalloc`].
const HEADER: usize = size_of::<*mut u8>();

/// Whether the raw allocator satisfies `layout` directly. Like the standard
/// library's `System` allocator, also requires `align <= size` because
/// allocators may align blocks smaller than `max_align_t` less strictly.
fn direct(layout: Layout) -> bool {
    layout.align() <= RAW_ALIGN && layout.align() <= layout.size()
}

/// Allocates a block whose alignment exceeds what the raw allocator
/// guarantees by over-allocating and stashing the original pointer in the
/// [`HEADER`] bytes just in front of the returned block.
unsafe fn alloc_over_aligned(layout: Layout) -> *mut u8 {
    let Some(total) = layout
        .size()
        .checked_add(layout.align())
        .and_then(|total| total.checked_add(HEADER))
    else {
        return ptr::null_mut();
    };
    let raw: *mut u8 = unsafe { ffi::PyMem_RawMalloc(total) }.cast();
    if raw.is_null() {
        return ptr::null_mut();
    }
    let addr = raw.addr() + HEADER;
    let aligned = addr + (addr.wrapping_neg() & (layout.align() - 1));
    let block = unsafe { raw.add(aligned - raw.addr()) };
    // The header slot is only as aligned as the block, which for alignments
    // below HEADER may not be enough for a pointer-sized write.
    unsafe { block.sub(HEADER).cast::<*mut u8>().write_unaligned(raw) };
    block
}

unsafe impl GlobalAlloc for PyMemRaw {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if direct(layout) {
            unsafe { ffi::PyMem_RawMalloc(layout.size()) }.cast()
        } else {
            unsafe { alloc_over_aligned(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if direct(layout) {
            unsafe { ffi::PyMem_RawCalloc(layout.size(), 1) }.cast()
        } else {
            let block = unsafe { alloc_over_aligned(layout) };
            if !block.is_null() {
                unsafe { block.write_bytes(0, layout.size()) };
            }
            block
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if direct(layout) {
            unsafe { ffi::PyMem_RawFree(ptr.cast()) };
        } else {
            let raw = unsafe { ptr.sub(HEADER).cast::<*mut u8>().read_unaligned() };
            unsafe { ffi::PyMem_RawFree(raw.cast()) };
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        if direct(layout) && direct(new_layout) {
            return unsafe { ffi::PyMem_RawRealloc(ptr.cast(), new_size) }.cast();
        }
        // The old and new blocks need different strategies, so move rather
        // than resize, freeing through the path that matches each layout.
        let new_ptr = unsafe { self.alloc(new_layout) };
        if !new_ptr.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
        }
        new_ptr
    }
}
