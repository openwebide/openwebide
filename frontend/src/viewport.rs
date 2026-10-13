//! Browser viewport primitives; layout policy remains in the shared layout store.
use leptos::prelude::*;
use wasm_bindgen::{JsCast, prelude::*};

#[cfg(target_arch = "wasm32")]
pub fn current_editor_target(
    actions: crate::state_actions::editor::EditorActions,
    textarea: &web_sys::HtmlTextAreaElement,
) -> bool {
    textarea.is_connected()
        && textarea
            .parent_element()
            .and_then(|parent| parent.get_attribute("data-editor-account"))
            .and_then(|account| account.parse::<u64>().ok())
            == Some(actions.account_generation())
        && textarea
            .get_attribute("data-editor-project")
            .and_then(|project| project.parse().ok())
            .zip(textarea.get_attribute("data-editor-path"))
            .is_some_and(|(project, path)| actions.is_current(project, &path))
}

#[wasm_bindgen(inline_js = r#"
export function observe_visible_height() {
    const viewport = window.visualViewport;
    const update = () => document.documentElement.style.setProperty('--visible-height', `${viewport?.height || window.innerHeight}px`);
    window.addEventListener('resize', update);
    viewport?.addEventListener('resize', update);
    update();
    return () => {
        window.removeEventListener('resize', update);
        viewport?.removeEventListener('resize', update);
        document.documentElement.style.removeProperty('--visible-height');
    };
}
"#)]
extern "C" {
    fn observe_visible_height() -> JsValue;
}

/// Keep the phone composer above on-screen keyboards, including Safari's overlay viewport.
pub fn install() {
    let stop = StoredValue::new_local(observe_visible_height());
    on_cleanup(move || {
        stop.with_value(|callback| {
            let _ = callback
                .unchecked_ref::<js_sys::Function>()
                .call0(&JsValue::NULL);
        });
    });
}

#[wasm_bindgen(inline_js = r#"
export function fit_composer(input) {
    const pane = input.closest('.chat-pane');
    if (!pane || !pane.clientHeight || !input.clientWidth) return;
    const composer = input.closest('.composer');
    const chrome = [...pane.children].filter(el => el !== composer && !el.matches('.messages, .tui-empty-state, .chat-welcome')).reduce((sum, el) => sum + el.getBoundingClientRect().height, 0);
    const maximum = Math.max(44, Math.min(pane.clientHeight * .8, pane.clientHeight - chrome - 100));
    input.style.overflowY = input.value ? 'auto' : 'hidden';
    if (!input.value) { input.style.height = '44px'; return; }
    input.style.height = '0px';
    input.style.height = `${Math.min(maximum, Math.max(44, input.scrollHeight + 2))}px`;
}
export function observe_composer(input) {
    const pane = input.closest('.chat-pane');
    let frame = 0;
    const update = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(() => fit_composer(input)); };
    const observer = new ResizeObserver(update);
    observer.observe(pane);
    let width = 0;
    const inputObserver = new ResizeObserver(() => { if (input.clientWidth !== width) { width = input.clientWidth; update(); } });
    inputObserver.observe(input);
    const mutation = new MutationObserver(update);
    mutation.observe(pane, {childList: true});
    input.addEventListener('input', update);
    update();
    return () => { cancelAnimationFrame(frame); observer.disconnect(); inputObserver.disconnect(); mutation.disconnect(); input.removeEventListener('input', update); };
}
"#)]
extern "C" {
    pub fn fit_composer(input: &web_sys::HtmlTextAreaElement);
    fn observe_composer(input: &web_sys::HtmlTextAreaElement) -> JsValue;
}

pub fn install_composer(input: NodeRef<leptos::html::Textarea>) {
    let stop = StoredValue::new_local(None::<JsValue>);
    Effect::new(move |_| {
        if let Some(input) = input.get()
            && stop.get_value().is_none()
        {
            stop.set_value(Some(observe_composer(&input)));
        }
    });
    on_cleanup(move || {
        stop.with_value(|stop| {
            if let Some(stop) = stop {
                let _ = stop
                    .unchecked_ref::<js_sys::Function>()
                    .call0(&JsValue::NULL);
            }
        });
    });
}

#[wasm_bindgen(inline_js = r#"
export function install_tooltips() {
    const tip = document.createElement('div');
    tip.className = 'ui-tooltip'; tip.id = 'ui-action-tooltip'; tip.setAttribute('role', 'tooltip'); tip.hidden = true;
    document.body.append(tip);
    let current = null, timer = 0, title = null, described = null;
    let keyboardFocus = true, dismissed = null, dismissedTitle = null;
    const restoreDismissed = () => {
        if (dismissed && dismissedTitle !== null) dismissed.setAttribute('title', dismissedTitle);
        dismissed = null; dismissedTitle = null;
    };
    const hide = () => {
        clearTimeout(timer); tip.hidden = true;
        if (current) {
            if (title !== null) current.setAttribute('title', title);
            if (described === null) current.removeAttribute('aria-describedby'); else current.setAttribute('aria-describedby', described);
        }
        current = null; title = null;
    };
    const enter = event => {
        const target = event.target instanceof Element ? event.target.closest('button[title],button[data-tooltip]') : null;
        if (!target || current === target || target === dismissed || (event.type === 'focus' && !keyboardFocus)) return;
        hide(); current = target; title = target.getAttribute('title'); described = target.getAttribute('aria-describedby');
        const text = target.getAttribute('data-tooltip') || title;
        if (!text) return;
        target.removeAttribute('title');
        if (!target.hasAttribute('aria-label') && target.classList.contains('icon-btn')) target.setAttribute('aria-label', text);
        timer = setTimeout(() => {
            if (!target.isConnected) { hide(); return; }
            tip.textContent = text; tip.hidden = false;
            const rect = target.getBoundingClientRect();
            const width = tip.offsetWidth, height = tip.offsetHeight;
            tip.style.left = `${Math.max(8, Math.min(innerWidth - width - 8, rect.left + (rect.width - width) / 2))}px`;
            tip.style.top = `${rect.bottom + height + 8 < innerHeight ? rect.bottom + 6 : Math.max(8, rect.top - height - 6)}px`;
            target.setAttribute('aria-describedby', [described, tip.id].filter(Boolean).join(' '));
        }, event.type === 'focus' ? 0 : 200);
    };
    const leave = event => {
        if (event.type === 'pointerout' && dismissed && !(event.relatedTarget instanceof Node && dismissed.contains(event.relatedTarget))) restoreDismissed();
        if (current && !(event.relatedTarget instanceof Node && current.contains(event.relatedTarget))) hide();
    };
    const pointerDown = event => {
        keyboardFocus = false;
        const target = event.target instanceof Element ? event.target.closest('button[title],button[data-tooltip]') : null;
        const hovered = current;
        hide(); restoreDismissed();
        dismissed = target || hovered;
        dismissedTitle = dismissed ? dismissed.getAttribute('title') : null;
        if (dismissed) dismissed.removeAttribute('title');
    };
    const key = event => {
        if (event.key === 'Escape') { hide(); return; }
        keyboardFocus = true; restoreDismissed();
    };
    document.addEventListener('pointerover', enter); document.addEventListener('pointerout', leave);
    document.addEventListener('focus', enter, true); document.addEventListener('blur', leave, true);
    document.addEventListener('pointerdown', pointerDown); document.addEventListener('keydown', key);
    const scroll = hide;
    window.addEventListener('scroll', scroll, true); window.addEventListener('resize', hide);
    return () => {
        hide(); restoreDismissed(); tip.remove(); document.removeEventListener('pointerover', enter); document.removeEventListener('pointerout', leave);
        document.removeEventListener('focus', enter, true); document.removeEventListener('blur', leave, true);
        document.removeEventListener('pointerdown', pointerDown); document.removeEventListener('keydown', key);
        window.removeEventListener('scroll', scroll, true); window.removeEventListener('resize', hide);
    };
}
"#)]
extern "C" {
    fn install_tooltips() -> JsValue;
}

pub fn install_action_tooltips() {
    let stop = StoredValue::new_local(install_tooltips());
    on_cleanup(move || {
        stop.with_value(|stop| {
            let _ = stop
                .unchecked_ref::<js_sys::Function>()
                .call0(&JsValue::NULL);
        });
    });
}

#[wasm_bindgen(inline_js = r#"
const editor_scroll_surfaces = new WeakMap();
export function editor_scroll_element(input) {
    const pane = input.parentElement;
    const previous = editor_scroll_surfaces.get(input);
    // The editor component owns this absolute surface for its mounted lifetime.
    // Repeated getters must not force style resolution after every DOM write.
    if (previous && previous.pane === pane && previous.scroll.parentElement === pane && previous.scroll.classList.contains('editor-scroll-surface')) return previous.scroll;
    editor_scroll_surfaces.delete(input);
    const scroll = pane?.querySelector('.editor-scroll-surface');
    if (!scroll || getComputedStyle(scroll).position !== 'absolute') return input;
    editor_scroll_surfaces.set(input, {pane, scroll});
    return scroll;
}
export function refresh_editor_scroll(input) {
    const scroll = editor_scroll_element(input);
    if (scroll === input) return;
    const extent = scroll.firstElementChild;
    if (!extent) return;
    const set = (element, name, value) => {
        if (element.style.getPropertyValue(name) !== value) element.style.setProperty(name, value);
    };
    // Read the viewport together before changing either input dimension. A
    // scrollbar/resize observer will refresh again if its client area changes.
    const viewportWidth = scroll.clientWidth, viewportHeight = scroll.clientHeight;
    set(input.parentElement, '--editor-input-width', `${viewportWidth}px`);
    set(input.parentElement, '--editor-input-height', `${viewportHeight}px`);
    const source = extent.dataset.editorScope === input.dataset.editorScope && extent.dataset.editorView === input.parentElement.dataset.editorView && extent.dataset.editorAccount === input.parentElement.dataset.editorAccount;
    const width = source ? Number(extent.dataset.sourceWidth) : NaN;
    const height = source ? Number(extent.dataset.sourceHeight) : NaN;
    const widthReady = Number.isFinite(width) && width >= 0;
    const heightReady = Number.isFinite(height) && height >= 0;
    // Bound native surrounding text has local dimensions. Retain the last
    // source extents until replacement source measurements are ready.
    const bounded = input.dataset.editorNativeBound === 'true';
    const previousWidth = parseFloat(extent.style.width) || viewportWidth;
    const previousHeight = parseFloat(extent.style.height) || viewportHeight;
    // Native fallback dimensions depend on the new input width. Read both
    // before changing the extent, rather than interleaving layout reads/writes.
    const sourceWidth = widthReady ? width : bounded ? previousWidth : input.scrollWidth;
    const sourceHeight = heightReady ? height : bounded ? previousHeight : input.scrollHeight;
    set(extent, 'width', `${Math.max(viewportWidth, sourceWidth)}px`);
    set(extent, 'height', `${Math.max(viewportHeight, sourceHeight)}px`);
}
// Position the browser's composition caret using bounded surrounding text.
// Paint supplies the source point; native scrolling remains local.
export function align_editor_native_input(input, x, y, composing) {
    input.style.removeProperty('--editor-native-ime-x');
    input.style.removeProperty('--editor-native-ime-y');
    if (!composing || input.dataset.editorNativeBound !== 'true' || !input.isConnected) return;
    const style = getComputedStyle(input), probe = document.createElement('div');
    probe.style.cssText = `position:fixed;left:-10000px;top:0;width:${input.clientWidth}px;height:auto;visibility:hidden;pointer-events:none;box-sizing:border-box;`;
    for (const name of ['font-family','font-size','font-style','font-weight','font-stretch',
        'font-variation-settings','font-feature-settings','font-variant-ligatures',
        'font-kerning','letter-spacing','line-height','tab-size','white-space','overflow-wrap',
        'padding-top','padding-bottom','padding-left','padding-right','direction']) {
        probe.style.setProperty(name, style.getPropertyValue(name));
    }
    const value = input.value, text = document.createTextNode(value + '\u200b');
    probe.append(text); document.body.append(probe);
    try {
        const head = input.selectionDirection === 'backward' ? input.selectionStart : input.selectionEnd;
        const range = document.createRange(); range.setStart(text, Math.min(value.length, head)); range.collapse(true);
        let caret = range.getBoundingClientRect();
        if (!caret.height) { range.setEnd(text, Math.min(text.length, head + 1)); caret = range.getBoundingClientRect(); }
        const origin = probe.getBoundingClientRect(), bounds = input.getBoundingClientRect();
        const localX = caret.left - origin.left, localY = caret.top - origin.top;
        input.scrollLeft = Math.max(0, localX - (x - bounds.left));
        input.scrollTop = Math.max(0, localY - (y - bounds.top));
        input.style.setProperty('--editor-native-ime-x', `${x - bounds.left - localX + input.scrollLeft}px`);
        input.style.setProperty('--editor-native-ime-y', `${y - bounds.top - localY + input.scrollTop}px`);
    } finally { probe.remove(); }
}
export function check_editor_extent(width, height) {
    const probe = document.createElement('div'), extent = document.createElement('div');
    probe.style.cssText = 'position:fixed;left:-10000px;top:0;width:1px;height:1px;overflow:hidden;padding:0;border:0;visibility:hidden;pointer-events:none';
    extent.style.cssText = `width:${width}px;height:${height}px;padding:0;border:0`;
    probe.append(extent); document.body.append(probe);
    try { return Math.abs(probe.scrollWidth - Math.max(1, width)) <= 2 && Math.abs(probe.scrollHeight - Math.max(1, height)) <= 2; }
    finally { probe.remove(); }
}
export function set_editor_scroll_position(input, value, horizontal) {
    refresh_editor_scroll(input);
    const scroll = editor_scroll_element(input);
    apply_editor_scroll(input, scroll, horizontal ? null : value, horizontal ? value : null);
}
export function set_editor_scroll(input, top, left) {
    refresh_editor_scroll(input);
    apply_editor_scroll(input, editor_scroll_element(input), top, left);
}
function apply_editor_scroll(input, scroll, top, left) {
    if (top !== null) scroll.scrollTop = top;
    if (left !== null) scroll.scrollLeft = left;
    if (input.dataset.editorNativeBound === 'true') return;
    if (top !== null) input.scrollTop = scroll.scrollTop;
    if (left !== null) input.scrollLeft = scroll.scrollLeft;
    if (scroll !== input) editor_native_echo.set(input, {top:input.scrollTop, left:input.scrollLeft, scope:input.dataset.editorScope, view:input.parentElement.dataset.editorView, account:input.parentElement.dataset.editorAccount});
}
const editor_native_echo = new WeakMap();
export function sync_editor_scroll(input, fromNative) {
    const scroll = editor_scroll_element(input);
    if (scroll === input) return false;
    refresh_editor_scroll(input);
    if (input.dataset.editorNativeBound === 'true') return false;
    if (fromNative) {
        const echo = editor_native_echo.get(input);
        if (echo && echo.scope === input.dataset.editorScope && echo.view === input.parentElement.dataset.editorView && echo.account === input.parentElement.dataset.editorAccount && Math.abs(input.scrollTop - echo.top) <= .25 && Math.abs(input.scrollLeft - echo.left) <= .25) return false;
        editor_native_echo.delete(input);
    }
    const source = fromNative ? input : scroll;
    const target = fromNative ? scroll : input;
    const changed = Math.abs(source.scrollTop - target.scrollTop) > .25 || Math.abs(source.scrollLeft - target.scrollLeft) > .25;
    if (changed) {
        target.scrollTop = source.scrollTop; target.scrollLeft = source.scrollLeft;
        if (!fromNative) editor_native_echo.set(input, {top:input.scrollTop, left:input.scrollLeft, scope:input.dataset.editorScope, view:input.parentElement.dataset.editorView, account:input.parentElement.dataset.editorAccount});
    }
    return changed;
}
export function forward_editor_wheel(input, event) {
    const scroll = editor_scroll_element(input);
    if (scroll === input || event.ctrlKey || event.metaKey) return;
    let x = event.deltaX, y = event.deltaY;
    if (event.shiftKey && !x) { x = y; y = 0; }
    if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) {
        const line = parseFloat(getComputedStyle(input).lineHeight) || 19.5;
        x *= line; y *= line;
    } else if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
        x *= scroll.clientWidth; y *= scroll.clientHeight;
    }
    event.preventDefault();
    forward_editor_scroll_delta(input, x, y);
}
function forward_editor_scroll_delta(input, x, y) {
    const scroll = editor_scroll_element(input);
    const extent = input.parentElement.querySelector('.editor-scroll-extent');
    const current = extent && extent.dataset.editorScope === input.dataset.editorScope && extent.dataset.editorView === input.parentElement.dataset.editorView && extent.dataset.editorAccount === input.parentElement.dataset.editorAccount;
    const width = current ? Number(extent.dataset.sourceWidth) : NaN;
    if (input.dataset.editorNativeBound === 'true' && !(Number.isFinite(width) && width >= 0)) {
        input.dispatchEvent(new CustomEvent('editor-scroll-intent', {detail:{x, y}, bubbles:true}));
    }
    scroll.scrollLeft += x; scroll.scrollTop += y;
    sync_editor_scroll(input, false);
}
const editor_touch_gestures = new WeakMap();
export function editor_native_value_matches(input, value) {
    return input.value === value;
}
export function set_editor_native_value(input, value) {
    input.value = value;
}
export function forward_editor_touch(input, event) {
    if (event.type === 'touchend' || event.type === 'touchcancel' || event.touches.length !== 1) {
        editor_touch_gestures.delete(input);
        return;
    }
    const touch = event.touches[0];
    const identity = JSON.stringify([input.dataset.editorProject, input.dataset.editorPath,
        input.dataset.editorScope, input.parentElement.dataset.editorView, input.parentElement.dataset.editorAccount]);
    if (event.type === 'touchstart') {
        editor_touch_gestures.set(input, {id:touch.identifier, x:touch.clientX, y:touch.clientY, identity, panning:false});
        return;
    }
    const gesture = editor_touch_gestures.get(input);
    if (!gesture || gesture.id !== touch.identifier || gesture.identity !== identity || !input.isConnected) {
        editor_touch_gestures.delete(input);
        return;
    }
    const x = gesture.x - touch.clientX, y = gesture.y - touch.clientY;
    if (!gesture.panning && Math.hypot(x, y) < 6) return;
    // The input is layered over a sibling scroll surface: the browser cannot
    // pan that sibling itself. Leave taps and pinch gestures to native input.
    if (!event.cancelable || editor_scroll_element(input) === input) return;
    event.preventDefault();
    gesture.panning = true;
    gesture.x = touch.clientX; gesture.y = touch.clientY;
    forward_editor_scroll_delta(input, x, y);
}
function editor_font_style_identity(style) {
    return ['font-family', 'font-size', 'font-style', 'font-weight',
        'font-stretch', 'line-height', 'letter-spacing', 'font-kerning',
        'font-feature-settings', 'font-variant-ligatures', 'font-variation-settings',
        'font-variant-caps', 'font-variant-numeric'].map(name => style.getPropertyValue(name));
}
function editor_primary_font_faces(style) {
    const match = style.fontFamily.match(/^\s*(?:"([^"]+)"|'([^']+)'|([^,]+))/);
    const family = match && (match[1] || match[2] || match[3]).trim();
    return Array.from(document.fonts || []).filter(face =>
        face.family.replace(/^['"]|['"]$/g, '').toLowerCase() === family?.toLowerCase());
}
const editor_font_face_ids = new WeakMap();
let editor_font_face_id = 0;
export function editor_font_identity(input) {
    const style = getComputedStyle(input);
    const faces = editor_primary_font_faces(style).map(face => {
        if (!editor_font_face_ids.has(face)) editor_font_face_ids.set(face, ++editor_font_face_id);
        return [editor_font_face_ids.get(face), face.status, face.style, face.weight, face.stretch];
    });
    return JSON.stringify([editor_font_style_identity(style), faces]);
}
export async function settle_editor_font(input, budgetMs) {
    if (!input.isConnected || !document.fonts) return false;
    const style = getComputedStyle(input);
    // Availability checks may succeed through fallback without loading the
    // primary registered face. Match its family and inspect the faces directly.
    const faces = editor_primary_font_faces(style).filter(face =>
        face.status !== 'loaded' && face.status !== 'error');
    if (!faces.length) return false;
    let timer;
    try {
        await Promise.race([
            Promise.all(faces.map(face => face.load())),
            new Promise(resolve => { timer = setTimeout(resolve, budgetMs); })
        ]);
    } catch (_) {
        // Failed downloads retain browser fallback.
    } finally { clearTimeout(timer); }
    return true;
}
function editor_preference_style(value) {
    const style = document.createElement('span').style;
    style.cssText = value || '';
    const geometry = new Set(['--editor-input-width', '--editor-input-height', '--editor-scroll-x', '--editor-scroll-y']);
    return Array.from(style).filter(name => !geometry.has(name)).sort()
        .map(name => [name, style.getPropertyValue(name), style.getPropertyPriority(name)]);
}
export function observe_editor_viewport(input, overlay, onLayout) {
    const pane = input.parentElement;
    const scroll = editor_scroll_element(input);
    let frame = 0, fontChanged = false, fontLoaded = false, active = true;
    const update = () => {
        frame = 0;
        if (!active || !input.isConnected || !overlay.isConnected) return;
        refresh_editor_scroll(input);
        // Apply viewport changes before measuring wrapped rows, then snapshot
        // all row heights before writing gutters to avoid repeated layout flushes.
        const height = `${scroll.clientHeight}px`, width = `${scroll.clientWidth}px`;
        const set = (element, name, value) => {
            if (element.style.getPropertyValue(name) !== value) element.style.setProperty(name, value);
        };
        set(overlay, '--editor-viewport-height', height);
        set(overlay, '--editor-text-width', width);
        const rows = overlay.querySelectorAll('.editor-source-line');
        const grips = pane.querySelectorAll('.editor-fold-row');
        const heights = Array.from(rows, row => `${row.getBoundingClientRect().height}px`);
        heights.forEach((height, index) => {
            if (grips[index]) set(grips[index], '--editor-row-height', height);
        });
        const changed = fontChanged, loaded = fontLoaded; fontChanged = false; fontLoaded = false;
        onLayout(changed, loaded);
    };
    const schedule = (changed = false, loaded = false) => { fontChanged ||= changed; fontLoaded ||= loaded; if (active && !frame) frame = requestAnimationFrame(update); };
    const observer = new ResizeObserver(() => schedule());
    observer.observe(input);
    if (scroll !== input) observer.observe(scroll);
    const mutation = new MutationObserver(() => schedule());
    mutation.observe(overlay, {childList: true, subtree: true});
    // Offscreen edits can change complete-source extents without changing any
    // painted rows. Apply newly proved dimensions even when paint stays intact.
    const extents = new MutationObserver(() => schedule());
    if (scroll !== input && scroll.firstElementChild) {
        extents.observe(scroll.firstElementChild, {attributes:true, attributeFilter:[
            'data-source-width', 'data-source-height', 'data-editor-scope',
            'data-editor-view', 'data-editor-account'
        ]});
    }
    const fontIdentity = () => JSON.stringify(editor_font_style_identity(getComputedStyle(input)));
    let preferenceFont = fontIdentity();
    const preferences = new MutationObserver(records => {
        // Viewport/scroll writes already have resize and scroll observers. Their
        // variables cannot change fonts; resolving computed style here would
        // flush the newly written paint and schedule a redundant layout frame.
        const meaningful = records.some(record => record.target !== pane || record.attributeName !== 'style'
            || JSON.stringify(editor_preference_style(record.oldValue)) !== JSON.stringify(editor_preference_style(pane.getAttribute('style'))));
        if (!meaningful) return;
        const next = fontIdentity(); schedule(next !== preferenceFont); preferenceFont = next;
    });
    preferences.observe(pane, {attributes: true, attributeOldValue: true, attributeFilter: ['class', 'style']});
    preferences.observe(document.documentElement, {attributes: true});
    const editor = input.closest('.editor');
    if (editor) preferences.observe(editor, {attributes: true, attributeFilter: ['style']});
    const fontsChanged = event => schedule(!event.isTrusted, event.isTrusted);
    document.fonts?.addEventListener('loadingdone', fontsChanged);
    // Readiness alone does not change glyphs. Observe actual loading transitions,
    // including loads already in progress when this observer is installed.
    document.fonts?.addEventListener('loadingerror', fontsChanged);
    update();
    return () => { active = false; cancelAnimationFrame(frame); observer.disconnect(); mutation.disconnect(); extents.disconnect(); preferences.disconnect(); document.fonts?.removeEventListener('loadingdone', fontsChanged); document.fonts?.removeEventListener('loadingerror', fontsChanged); };
}
"#)]
extern "C" {
    pub fn set_editor_scroll(input: &web_sys::HtmlTextAreaElement, top: f64, left: f64);
    fn set_editor_scroll_position(
        input: &web_sys::HtmlTextAreaElement,
        value: f64,
        horizontal: bool,
    );
    fn editor_scroll_element(input: &web_sys::HtmlTextAreaElement) -> web_sys::HtmlElement;
    pub fn refresh_editor_scroll(input: &web_sys::HtmlTextAreaElement);
    pub fn check_editor_extent(width: f64, height: f64) -> bool;
    pub fn align_editor_native_input(
        input: &web_sys::HtmlTextAreaElement,
        x: f64,
        y: f64,
        composing: bool,
    );
    pub fn sync_editor_scroll(input: &web_sys::HtmlTextAreaElement, from_native: bool) -> bool;
    pub fn forward_editor_wheel(input: &web_sys::HtmlTextAreaElement, event: &web_sys::WheelEvent);
    pub fn forward_editor_touch(input: &web_sys::HtmlTextAreaElement, event: &web_sys::TouchEvent);
    pub fn editor_native_value_matches(
        input: &web_sys::HtmlTextAreaElement,
        value: &js_sys::JsString,
    ) -> bool;
    pub fn set_editor_native_value(input: &web_sys::HtmlTextAreaElement, value: &js_sys::JsString);
    pub fn editor_font_identity(input: &web_sys::Element) -> String;
    #[wasm_bindgen(catch)]
    pub async fn settle_editor_font(
        input: &web_sys::HtmlTextAreaElement,
        budget_ms: u32,
    ) -> Result<bool, JsValue>;
    pub fn observe_editor_viewport(
        input: &web_sys::HtmlTextAreaElement,
        overlay: &web_sys::HtmlElement,
        on_layout: &js_sys::Function,
    ) -> JsValue;
}

/// One document scroll viewport, independent of the native input's value.
/// Standalone measurement inputs retain their native scroll primitive.
pub fn editor_scroll(input: &web_sys::HtmlTextAreaElement) -> web_sys::HtmlElement {
    editor_scroll_element(input)
}

pub fn set_editor_scroll_top(input: &web_sys::HtmlTextAreaElement, top: f64) {
    set_editor_scroll_position(input, top, false);
}

pub fn set_editor_scroll_left(input: &web_sys::HtmlTextAreaElement, left: f64) {
    set_editor_scroll_position(input, left, true);
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(inline_js = r#"
export function editor_point(input, x, y, dragging) {
    const paint = input.parentElement?.querySelector('.editor-highlight-content');
    const overlay = paint?.parentElement;
    if (!paint || !overlay) return undefined;
    const bounds = input.getBoundingClientRect();
    if (bounds.width <= 2 || bounds.height <= 2) return undefined;
    if (!dragging && (x < bounds.left || x > bounds.right || y < bounds.top || y > bounds.bottom)) return undefined;
    let hitRow;
    {
        let nearest, distance = Infinity;
        for (const row of paint.querySelectorAll('.editor-source-line')) {
            const rect = row.getBoundingClientRect();
            const top = Math.max(bounds.top, rect.top), bottom = Math.min(bounds.bottom, rect.bottom);
            if (bottom <= top) continue;
            const at = Math.max(top + Math.min(.5, (bottom - top) / 2),
                Math.min(bottom - Math.min(.5, (bottom - top) / 2), y));
            if (Math.abs(at - y) < distance) { hitRow = row; nearest = at; distance = Math.abs(at - y); }
        }
        if (nearest === undefined) return undefined;
        x = Math.max(bounds.left + 1, Math.min(bounds.right - 1, x));
        // Clicks in a visual row's leading/descender space still belong to
        // that row. Query its glyph band, otherwise rectangle binary search
        // can ignore x when y sits above the font's actual ink rectangle.
        const lineHeight = parseFloat(getComputedStyle(input).lineHeight);
        const row = hitRow.getBoundingClientRect();
        const center = Number.isFinite(lineHeight) && lineHeight > 0
            ? row.top + (Math.floor((nearest - row.top) / lineHeight) + .5) * lineHeight
            : nearest;
        y = Math.max(bounds.top + .5, Math.min(bounds.bottom - .5, center));
    }
    const inputEvents = input.style.pointerEvents, paintEvents = overlay.style.pointerEvents;
    try {
        input.style.pointerEvents = 'none'; overlay.style.pointerEvents = 'auto';
        const position = document.caretPositionFromPoint?.(x, y);
        const caret = position ? {startContainer:position.offsetNode, startOffset:position.offset} : document.caretRangeFromPoint?.(x, y);
        // Validate click and drag hits alike: browser caret APIs can return
        // a token start or row/container boundary for blank
        // space or generated gutters. Resolve such hits from measured text
        // boundaries instead of treating the container offset as character zero.
        if (caret && hitRow.contains(caret.startContainer) && caret.startContainer.nodeType === Node.TEXT_NODE) {
            const hit = document.createRange();
            hit.setStart(caret.startContainer, caret.startOffset); hit.collapse(true);
            const rect = hit.getBoundingClientRect();
            if (rect.height && y >= rect.top && y <= rect.bottom && Math.abs(rect.left - x) <= rect.height / 2) {
                return [caret.startContainer, caret.startOffset];
            }
            // At bidi run boundaries the collapsed caret can have another
            // visual affinity; adjacent glyph rectangles validate that hit.
            hit.setStart(caret.startContainer, Math.max(0, caret.startOffset - 1));
            hit.setEnd(caret.startContainer, Math.min(caret.startContainer.length, caret.startOffset + 1));
            for (const rect of hit.getClientRects()) {
                if (rect.height && y >= rect.top && y <= rect.bottom && x >= rect.left && x <= rect.right) {
                    return [caret.startContainer, caret.startOffset];
                }
            }
        }
        const walker = document.createTreeWalker(hitRow, NodeFilter.SHOW_TEXT);
        const range = document.createRange();
        let node, best, distance = Infinity;
        const rectAt = offset => {
            range.setStart(node, offset); range.collapse(true);
            return range.getBoundingClientRect();
        };
        while ((node = walker.nextNode())) {
            let low = 0, high = node.length;
            while (low < high) {
                const middle = (low + high) >>> 1, rect = rectAt(middle);
                if (rect.bottom <= y || (rect.top <= y && rect.left < x)) low = middle + 1;
                else high = middle;
            }
            for (const offset of new Set([low, Math.max(0, low - 1), node.length])) {
                const rect = rectAt(offset);
                if (!rect.height) continue;
                const vertical = Math.max(rect.top - y, y - rect.bottom, 0);
                const score = vertical * 10000 + Math.abs(rect.left - x);
                if (score < distance) { best = [node, offset]; distance = score; }
            }
        }
        return best;
    } finally {
        input.style.pointerEvents = inputEvents; overlay.style.pointerEvents = paintEvents;
    }
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn editor_point(
        input: &web_sys::HtmlTextAreaElement,
        x: f64,
        y: f64,
        dragging: bool,
    ) -> Result<JsValue, JsValue>;
}

#[cfg(target_arch = "wasm32")]
pub fn editor_caret_from_point(
    input: &web_sys::HtmlTextAreaElement,
    x: f64,
    y: f64,
) -> Option<u32> {
    editor_caret_at_point(input, x, y, false)
}

/// Dragging can leave the text's painted area. Resolve against the nearest
/// visible row without introducing source-unit or selection policy in the DOM.
#[cfg(target_arch = "wasm32")]
pub fn editor_caret_from_drag_point(
    input: &web_sys::HtmlTextAreaElement,
    x: f64,
    y: f64,
) -> Option<u32> {
    editor_caret_at_point(input, x, y, true)
}

#[cfg(target_arch = "wasm32")]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "DOM point offsets are checked for integer value and the u32 range before conversion"
)]
fn editor_caret_at_point(
    input: &web_sys::HtmlTextAreaElement,
    x: f64,
    y: f64,
    dragging: bool,
) -> Option<u32> {
    let paint = input
        .parent_element()?
        .query_selector(".editor-highlight-content")
        .ok()??;
    if paint.get_attribute("data-editor-scope") != input.get_attribute("data-editor-scope") {
        return None;
    }
    let point = editor_point(input, x, y, dragging).ok()?;
    if !js_sys::Array::is_array(&point) {
        return None;
    }
    let point = point.unchecked_into::<js_sys::Array>();
    if point.length() != 2 {
        return None;
    }
    let node = point.get(0).dyn_into::<web_sys::Node>().ok()?;
    let offset = point.get(1).as_f64()?;
    if !offset.is_finite()
        || offset.fract() != 0.0
        || !(0.0..=f64::from(u32::MAX)).contains(&offset)
    {
        return None;
    }
    crate::components::editor_paint::native_offset(&paint, &node, offset as u32)
}

#[wasm_bindgen(inline_js = r#"
export function observe_element_width(element, update) {
    let measuredWidth;
    let contentWidth;
    const publish = () => {
        const width = element.clientWidth;
        if (width !== measuredWidth) { measuredWidth = width; update(width); }
    };
    const observer = new ResizeObserver(entries => {
        const width = entries.find(entry => entry.target === element)?.contentRect.width;
        // ResizeObserver also reports height changes. Its already-computed box
        // proves whether another native width read is necessary; do not force
        // layout or invalidate panel bounds for an unchanged width.
        if (width === undefined || width === contentWidth) return;
        contentWidth = width;
        publish();
    });
    observer.observe(element);
    publish();
    return () => observer.disconnect();
}
"#)]
extern "C" {
    fn observe_element_width(element: &web_sys::Element, update: &js_sys::Function) -> JsValue;
}

pub fn track_width(element: web_sys::Element, width: RwSignal<f64>) {
    let update = Closure::<dyn Fn(f64)>::new(move |value| {
        let _ = width.try_set(value);
    });
    let stop = StoredValue::new_local(observe_element_width(
        &element,
        update.as_ref().unchecked_ref(),
    ));
    let update = StoredValue::new_local(update);
    on_cleanup(move || {
        stop.with_value(|stop| {
            let _ = stop
                .unchecked_ref::<js_sys::Function>()
                .call0(&JsValue::NULL);
        });
        update.with_value(|_| ());
    });
}

#[cfg(all(test, target_arch = "wasm32"))]
mod width_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen(inline_js = r#"
export function viewport_preferences_fixture() {
    const pane = document.createElement('div');
    pane.style.cssText = 'position:relative;width:300px;height:200px;';
    const scroll = document.createElement('div');
    scroll.className = 'editor-scroll-surface';
    scroll.style.cssText = 'position:absolute;inset:0;overflow:auto;';
    const extent = document.createElement('div');
    extent.dataset.sourceWidth = '1000'; extent.dataset.sourceHeight = '1000';
    scroll.append(extent);
    const input = document.createElement('textarea');
    input.style.cssText = 'font:inherit;position:absolute;width:200px;height:100px;';
    input.dataset.editorNativeBound = 'true';
    const overlay = document.createElement('div');
    pane.append(scroll, input, overlay); document.body.append(pane);
    const original = window.getComputedStyle;
    const state = {reads:0, pane, input, overlay};
    window.getComputedStyle = function(element, ...args) {
        if (element === input) state.reads++;
        return original.call(this, element, ...args);
    };
    state.dispose = () => {window.getComputedStyle = original; pane.remove();};
    return state;
}
export function viewport_preferences_change(state, kind) {
    if (kind === 'geometry') {
        for (const name of ['--editor-input-width','--editor-input-height','--editor-scroll-x','--editor-scroll-y'])
            state.pane.style.setProperty(name, '123px');
    } else if (kind === 'font') state.pane.style.fontSize = '17px';
    else if (kind === 'class') state.pane.classList.add('viewport-fixture-changed');
    else state.pane.style.setProperty('--future-editor-setting', 'changed');
}
export function viewport_preferences_dispose(state) {state.dispose();}
export function instrument_width(element) {
    const state = {reads:0, notifications:0};
    const getter = Object.getOwnPropertyDescriptor(Element.prototype, 'clientWidth').get;
    Object.defineProperty(element, 'clientWidth', {configurable:true, get() {
        state.reads++; return getter.call(this);
    }});
    const observer = new ResizeObserver(() => state.notifications++);
    observer.observe(element);
    state.stop = () => { observer.disconnect(); delete element.clientWidth; };
    return state;
}
"#)]
    extern "C" {
        fn viewport_preferences_fixture() -> JsValue;
        fn viewport_preferences_change(state: &JsValue, kind: &str);
        fn viewport_preferences_dispose(state: &JsValue);
        fn instrument_width(element: &web_sys::Element) -> JsValue;
    }

    fn count(state: &JsValue, key: &str) -> f64 {
        js_sys::Reflect::get(state, &key.into())
            .unwrap()
            .as_f64()
            .unwrap()
    }
    async fn wait(ready: impl Fn() -> bool) {
        let deadline = js_sys::Date::now() + 3000.0;
        while !ready() {
            assert!(js_sys::Date::now() < deadline, "width observer timed out");
            crate::util::sleep_ms(10).await;
        }
    }

    #[wasm_bindgen_test]
    #[allow(
        clippy::float_cmp,
        reason = "Integral fixture read counts prove no computed-style calls occurred"
    )]
    async fn viewport_geometry_style_changes_skip_font_reads_and_preserve_preferences_cleanup() {
        let state = viewport_preferences_fixture();
        let input = js_sys::Reflect::get(&state, &"input".into())
            .unwrap()
            .unchecked_into::<web_sys::HtmlTextAreaElement>();
        let overlay = js_sys::Reflect::get(&state, &"overlay".into())
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>();
        let calls = Rc::new(Cell::new(0));
        let fonts = Rc::new(Cell::new(0));
        let callback = Closure::<dyn Fn(bool, bool)>::new({
            let calls = calls.clone();
            let fonts = fonts.clone();
            move |changed, _loaded| {
                calls.set(calls.get() + 1);
                if changed {
                    fonts.set(fonts.get() + 1);
                }
            }
        });
        let stop = observe_editor_viewport(&input, &overlay, callback.as_ref().unchecked_ref());
        crate::util::sleep_ms(100).await;
        let reads = count(&state, "reads");
        let layouts = calls.get();
        viewport_preferences_change(&state, "geometry");
        crate::util::sleep_ms(70).await;
        assert_eq!(count(&state, "reads"), reads);
        assert_eq!(calls.get(), layouts);
        viewport_preferences_change(&state, "font");
        wait(|| fonts.get() > 0).await;
        for kind in ["class", "future"] {
            let before = calls.get();
            viewport_preferences_change(&state, kind);
            wait(|| calls.get() > before).await;
        }
        stop.unchecked_ref::<js_sys::Function>()
            .call0(&JsValue::NULL)
            .unwrap();
        let reads = count(&state, "reads");
        let layouts = calls.get();
        viewport_preferences_change(&state, "geometry");
        viewport_preferences_change(&state, "font");
        crate::util::sleep_ms(70).await;
        assert_eq!(count(&state, "reads"), reads);
        assert_eq!(calls.get(), layouts);
        viewport_preferences_dispose(&state);
    }

    #[wasm_bindgen_test]
    #[allow(
        clippy::float_cmp,
        reason = "DOM clientWidth and fixture counters are integral JS numbers; exact equality proves skipped reads"
    )]
    async fn width_tracking_skips_height_only_layout_reads_and_preserves_resize_cleanup() {
        let element = document().create_element("div").unwrap();
        element
            .set_attribute("style", "width:100px;height:10px;padding:5px;border:2px solid;box-sizing:content-box;overflow:auto")
            .unwrap();
        document().body().unwrap().append_child(&element).unwrap();
        let state = instrument_width(&element);
        let value = Rc::new(Cell::new(-1.0));
        let calls = Rc::new(Cell::new(0));
        let update = Closure::<dyn Fn(f64)>::new({
            let value = value.clone();
            let calls = calls.clone();
            move |width| {
                value.set(width);
                calls.set(calls.get() + 1);
            }
        });
        let stop = observe_element_width(&element, update.as_ref().unchecked_ref());
        wait(|| count(&state, "notifications") > 0.0).await;
        assert_eq!(value.get(), 110.0);
        assert_eq!(calls.get(), 1);
        let reads = count(&state, "reads");
        for height in [20, 30] {
            let notifications = count(&state, "notifications");
            element
                .unchecked_ref::<web_sys::HtmlElement>()
                .style()
                .set_property("height", &format!("{height}px"))
                .unwrap();
            wait(|| count(&state, "notifications") > notifications).await;
            assert_eq!(count(&state, "reads"), reads);
            assert_eq!(calls.get(), 1);
        }
        let style = element.unchecked_ref::<web_sys::HtmlElement>().style();
        style.set_property("width", "200px").unwrap();
        wait(|| value.get() == 210.0).await;
        style.set_property("display", "none").unwrap();
        wait(|| value.get() == 0.0).await;
        style.remove_property("display").unwrap();
        wait(|| value.get() == 210.0).await;
        stop.unchecked_ref::<js_sys::Function>()
            .call0(&JsValue::NULL)
            .unwrap();
        let reads = count(&state, "reads");
        let calls_before = calls.get();
        let notifications = count(&state, "notifications");
        style.set_property("width", "300px").unwrap();
        wait(|| count(&state, "notifications") > notifications).await;
        assert_eq!(count(&state, "reads"), reads);
        assert_eq!(calls.get(), calls_before);
        js_sys::Reflect::get(&state, &"stop".into())
            .unwrap()
            .unchecked_ref::<js_sys::Function>()
            .call0(&JsValue::NULL)
            .unwrap();
        element.remove();
    }
}
