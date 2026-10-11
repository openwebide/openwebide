pub async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function yield_browser_task() {
    if (globalThis.scheduler?.yield) return globalThis.scheduler.yield();
    return new Promise(resolve => {
        const channel = new MessageChannel();
        channel.port1.onmessage = () => {
            channel.port1.close(); channel.port2.close(); resolve();
        };
        channel.port2.postMessage(0);
    });
}
export function yield_browser_background_task() {
    if (globalThis.scheduler?.postTask)
        return globalThis.scheduler.postTask(() => undefined, {priority: 'background'});
    return yield_browser_task();
}
export function next_browser_frame() {
    return new Promise(resolve => {
        if (document.hidden) setTimeout(resolve, 0);
        else requestAnimationFrame(resolve);
    });
}
"#)]
extern "C" {
    fn yield_browser_task() -> js_sys::Promise;
    fn next_browser_frame() -> js_sys::Promise;
    fn yield_browser_background_task() -> js_sys::Promise;
}

/// Browser task scheduling is a runtime primitive. Unlike repeated timers, this
/// does not accumulate the browser's nested-timeout delay between work batches.
pub async fn yield_task() {
    let _ = wasm_bindgen_futures::JsFuture::from(yield_browser_task()).await;
}

/// Background geometry yields to input and visible paint after proved coverage.
/// Browsers without task priorities keep the ordinary cooperative task fallback.
pub async fn yield_background_task() {
    let _ = wasm_bindgen_futures::JsFuture::from(yield_browser_background_task()).await;
}

pub async fn yield_frame() {
    let _ = wasm_bindgen_futures::JsFuture::from(next_browser_frame()).await;
    yield_task().await;
}
