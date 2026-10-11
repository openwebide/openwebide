// Runtime initialization only; syntax policy and validation live in Rust.
const bundle = new URL('__EDITOR_MODULE__', import.meta.url);
const binary = new URL('__EDITOR_WASM__', import.meta.url);
const compiled = new Promise(resolve => {
  let timer;
  const finish = module => {
    clearTimeout(timer);
    self.removeEventListener('message', receive);
    resolve(module);
  };
  const receive = event => {
    if (event.data?.kind !== 'openwebide-editor-module:1') return;
    finish(event.data.bundle_url === bundle.href && event.data.module instanceof WebAssembly.Module
      ? event.data.module : null);
  };
  self.addEventListener('message', receive);
  // Standalone clients and platforms that cannot clone a Module use the URL.
  timer = setTimeout(() => finish(null), 25);
});
const {default: init} = await import(bundle.href);
await init({module_or_path: await compiled || binary});
