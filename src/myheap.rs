use core::{
    alloc::{Allocator, GlobalAlloc, Layout},
    ptr::NonNull,
};

use alloc::alloc::AllocError;
use esp_alloc::EspHeap;

pub static MYHEAP: EspHeap = EspHeap::empty();

#[derive(Clone, Copy)]
pub struct MyHeapAllocator<'a>(pub &'a EspHeap);

pub type MyHeapVec<T> = alloc::vec::Vec<T, MyHeapAllocator<'static>>;

fn empty_slice() -> NonNull<[u8]> {
    NonNull::slice_from_raw_parts(NonNull::<u8>::dangling(), 0)
}

fn slice_from_raw(ptr: *mut u8, size: usize) -> Result<NonNull<[u8]>, AllocError> {
    if size == 0 {
        return Ok(empty_slice());
    }

    NonNull::new(ptr)
        .map(|p| NonNull::slice_from_raw_parts(p, size))
        .ok_or(AllocError)
}

#[macro_export]
macro_rules! vec_in_myheap {
    ($value:expr; $len:expr) => {{
        let mut v = alloc::vec::Vec::with_capacity_in($len, crate::MyHeapAllocator(&crate::MYHEAP));
        v.resize($len, $value);
        v
    }};
}

unsafe impl<'a> Allocator for MyHeapAllocator<'a> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(empty_slice());
        }

        let ptr = unsafe { <EspHeap as GlobalAlloc>::alloc(self.0, layout) };
        slice_from_raw(ptr, layout.size())
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(empty_slice());
        }

        let ptr = unsafe { <EspHeap as GlobalAlloc>::alloc_zeroed(self.0, layout) };
        slice_from_raw(ptr, layout.size())
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() == 0 {
            return;
        }

        unsafe { <EspHeap as GlobalAlloc>::dealloc(self.0, ptr.as_ptr(), layout) };
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if new_layout.size() == 0 {
            unsafe { self.deallocate(ptr, layout) };
            return Ok(empty_slice());
        }

        let raw = unsafe {
            <EspHeap as GlobalAlloc>::realloc(self.0, ptr.as_ptr(), layout, new_layout.size())
        };
        slice_from_raw(raw, new_layout.size())
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if new_layout.size() == 0 {
            unsafe { self.deallocate(ptr, layout) };
            return Ok(empty_slice());
        }

        let raw = unsafe {
            <EspHeap as GlobalAlloc>::realloc(self.0, ptr.as_ptr(), layout, new_layout.size())
        };
        if raw.is_null() {
            return Err(AllocError);
        }

        let old_size = layout.size();
        let new_size = new_layout.size();
        if new_size > old_size {
            unsafe { core::ptr::write_bytes(raw.add(old_size), 0, new_size - old_size) };
        }

        slice_from_raw(raw, new_size)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if new_layout.size() == 0 {
            unsafe { self.deallocate(ptr, layout) };
            return Ok(empty_slice());
        }

        let raw = unsafe {
            <EspHeap as GlobalAlloc>::realloc(self.0, ptr.as_ptr(), layout, new_layout.size())
        };
        slice_from_raw(raw, new_layout.size())
    }
}
