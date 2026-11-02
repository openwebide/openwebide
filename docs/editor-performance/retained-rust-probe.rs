use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackingAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(bytes: usize) {
    let current = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(current, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
            OBJECTS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
            OBJECTS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        OBJECTS.fetch_sub(1, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(ptr, layout, size) };
        if !result.is_null() {
            if size >= layout.size() { add(size - layout.size()); }
            else { LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed); }
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

#[wasm_bindgen::prelude::wasm_bindgen]
pub fn editor_retained_heap_bytes() -> usize { LIVE.load(Ordering::Relaxed) }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn editor_retained_heap_objects() -> usize { OBJECTS.load(Ordering::Relaxed) }
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn editor_peak_heap_bytes() -> usize { PEAK.load(Ordering::Relaxed) }
