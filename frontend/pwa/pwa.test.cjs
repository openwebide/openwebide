const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

function worker(fetch) {
  const handlers = {};
  const entries = new Map([['/', new Response('current build page')], ['/offline.html', new Response('Can’t reach the server')], ['/about.html', new Response('About Open WebIDE · license notices')], ['/app.wasm', new Response('wasm')]]);
  const deleted = [];
  const cache = {match: async key => entries.get(key), addAll: async files => {cache.files = files;}};
  const context = {BUILD_ID: 'test-build', SHELL_FILES: ['/', '/offline.html', '/about.html', '/app.wasm', '/app.js'], URL, Request, fetch,
    self: {location: {origin: 'https://ide.test'}, clients: {claim: async () => {}}, addEventListener: (name, action) => {handlers[name] = action;}},
    caches: {open: async name => {assert.equal(name, 'openwebide-shell-test-build'); return cache;}, keys: async () => ['other-app', 'openwebide-shell-old', 'openwebide-shell-test-build'], delete: async key => deleted.push(key)}};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'service-worker.js'), 'utf8'), context);
  function request(url, method = 'GET', mode = 'cors') {
    let response;
    handlers.fetch({request: {url, method, mode}, respondWith: promise => {response = promise;}});
    return response;
  }
  return {handlers, request, cache, deleted, entries};
}

test('the worker never handles API, bridge, foreign, write or unknown requests', () => {
  const host = worker(() => {throw Error('Unexpected fetch');});
  for (const route of ['/api', '/api/theme.js', '/api/sessions', '/bridge', '/bridge/exec', '/private.txt']) {
    assert.equal(host.request('https://ide.test'+route), undefined, route);
  }
  assert.equal(host.request('https://model.test/app.js'), undefined);
  assert.equal(host.request('https://ide.test/app.js', 'POST'), undefined);
  assert.equal(host.request('https://ide.test/app.js?private=1'), undefined);
});

test('versioned app assets are served from shell cache and offline navigation has guidance', async () => {
  const host = worker(async () => {throw Error('offline');});
  assert.equal(await (await host.request('https://ide.test/app.wasm')).text(), 'wasm');
  assert.match(await (await host.request('https://ide.test/', 'GET', 'navigate')).text(), /reach the server/);
  let installed;
  host.handlers.install({waitUntil: task => {installed = task;}});
  await installed;
  assert.deepEqual(Array.from(host.cache.files, request => new URL(request.url).pathname), ['/', '/offline.html', '/about.html', '/app.wasm', '/app.js']);
  assert.ok(host.cache.files.every(request => request.cache === 'reload'));
  let activated;
  host.handlers.activate({waitUntil: task => {activated = task;}});
  await activated;
  assert.deepEqual(host.deleted, ['openwebide-shell-old']);
});

test('navigation keeps the installed build coherent across server deployments', async () => {
  let probes = 0;
  const host = worker(async (request, options) => {
    probes++;
    assert.equal(options.cache, 'no-store');
    return new Response('new build page');
  });
  assert.equal(await (await host.request('https://ide.test/', 'GET', 'navigate')).text(), 'current build page');
  assert.equal(probes, 1);
  host.entries.delete('/');
  assert.equal(await (await host.request('https://ide.test/', 'GET', 'navigate')).text(), 'new build page');
  const failed = worker(async () => new Response('Server error', {status: 503}));
  assert.equal((await failed.request('https://ide.test/', 'GET', 'navigate')).status, 503);
});

function installation(secure, installed = false) {
  const handlers = {};
  const registrations = [];
  const window = {location: {href: 'https://ide.test/'}, isSecureContext: secure, addEventListener: (name, action) => {const previous = handlers[name]; handlers[name] = event => {previous?.(event); action(event);};}, dispatchEvent: event => handlers[event.type]?.(event)};
  const navigator = {serviceWorker: {register: async (...args) => registrations.push(args)}};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'pwa.js'), 'utf8'), {window, navigator, URL, matchMedia: () => ({matches: installed, addEventListener: () => {}}), Event, console, MutationObserver: class {observe() {}}, document: {documentElement: {}, querySelector: () => null}, getComputedStyle: () => ({getPropertyValue: () => '#1e1f24'})});
  return {window, handlers, registrations};
}

test('HTTP LAN does not register a worker; secure contexts and installed display are distinct capabilities', async () => {
  const lan = installation(false);
  lan.handlers.load();
  assert.equal(lan.registrations.length, 0);
  assert.equal(lan.window.webideInstall.state().secure, false);
  const secure = installation(true);
  secure.handlers.load();
  await Promise.resolve();
  assert.equal(secure.registrations[0][0], '/service-worker.js');
  assert.equal(secure.window.webideInstall.state().available, false);
  assert.equal(installation(true, true).window.webideInstall.state().installed, true);
});

test('installation waits for a user gesture and consumes the browser prompt only once', async () => {
  const host = installation(true);
  let prompted = 0;
  host.handlers.beforeinstallprompt({preventDefault() {}, prompt: async () => {prompted++;}, userChoice: Promise.resolve({outcome: 'accepted'})});
  assert.equal(prompted, 0);
  assert.equal(host.window.webideInstall.state().available, true);
  assert.equal(await host.window.webideInstall.prompt(), true);
  assert.equal(await host.window.webideInstall.prompt(), false);
  assert.equal(prompted, 1);
});

test('phone viewport tracks an overlay keyboard and removes its listeners on disposal', () => {
  const source = fs.readFileSync(path.join(__dirname, '../src/viewport.rs'), 'utf8').match(/inline_js = r#"([\s\S]*?)"#/)[1];
  const viewport = new EventTarget();
  viewport.height = 844;
  const window = new EventTarget();
  window.innerHeight = 844;
  window.visualViewport = viewport;
  const values = new Map();
  const document = {documentElement: {style: {setProperty: (key, value) => values.set(key, value), removeProperty: key => values.delete(key)}}};
  const context = {window, document};
  vm.runInNewContext(source.replace('export function', 'function') + ';globalThis.dispose = observe_visible_height();', context);
  assert.equal(values.get('--visible-height'), '844px');
  viewport.height = 480;
  viewport.dispatchEvent(new Event('resize'));
  assert.equal(window.innerHeight, 844);
  assert.equal(values.get('--visible-height'), '480px');
  context.dispose();
  viewport.height = 844;
  viewport.dispatchEvent(new Event('resize'));
  window.dispatchEvent(new Event('resize'));
  assert.equal(values.has('--visible-height'), false);
});

test('bundled editor fonts are cached offline and font updates change the PWA build identity', () => {
  const os = require('node:os');
  const {execFileSync} = require('node:child_process');
  const dist = fs.mkdtempSync(path.join(os.tmpdir(), 'openwebide-font-cache-'));
  try {
    for (const file of ['index.html', 'styles-test.css', 'openwebide-frontend-test.js', 'openwebide-frontend-test_bg.wasm']) fs.writeFileSync(path.join(dist, file), 'fixture');
    const snippets = path.join(dist, 'snippets', 'bindings');
    fs.mkdirSync(snippets, {recursive:true});
    fs.writeFileSync(path.join(dist, 'openwebide-frontend-test.js'), "import './snippets/bindings/used.js';");
    fs.writeFileSync(path.join(snippets, 'used.js'), "export {value} from './nested.js';");
    fs.writeFileSync(path.join(snippets, 'nested.js'), 'export const value = 1;');
    fs.writeFileSync(path.join(snippets, 'stale.js'), 'export const unusedTestAsset = true;');
    fs.writeFileSync(path.join(dist, 'index.html'), '<!doctype html>文😀\n<link href="/snippets/bindings/stale.js?old" rel="modulepreload" crossorigin>\n<link rel="modulepreload" href="/snippets/bindings/used.js">\n');
    fs.cpSync(path.join(__dirname, '../fonts'), path.join(dist, 'fonts'), {recursive:true});
    const build = () => {
      execFileSync('sh', ['pwa/build.sh'], {cwd:path.join(__dirname, '..'), env:{...process.env, TRUNK_STAGING_DIR:dist}});
      const context = {self:{addEventListener(){}}, Set};
      vm.runInNewContext(fs.readFileSync(path.join(dist, 'service-worker.js'), 'utf8') + ';globalThis.build = BUILD_ID; globalThis.files = SHELL_FILES;', context);
      return context;
    };
    const before = build();
    assert.equal(before.files.some(file => file.includes('editor-worker')), false);
    assert.equal(fs.readdirSync(dist).some(file => file.startsWith('editor-worker')), false);
    assert.ok(before.files.includes('/snippets/bindings/used.js'));
    assert.ok(before.files.includes('/snippets/bindings/nested.js'));
    assert.equal(before.files.includes('/snippets/bindings/stale.js'), false);
    assert.equal(fs.existsSync(path.join(snippets, 'stale.js')), false);
    const currentIndex = fs.readFileSync(path.join(dist, 'index.html'), 'utf8');
    assert.doesNotMatch(currentIndex, /stale\.js/);
    assert.match(currentIndex, /used\.js/);
    assert.match(currentIndex, /<!doctype html>文😀/);
    // A previous generated cache must not keep a snippet alive after its
    // entry module stops importing it on a subsequent build.
    fs.writeFileSync(path.join(dist, 'openwebide-frontend-test.js'), 'export const currentBuild = true;');
    const refreshed = build();
    assert.equal(refreshed.files.includes('/snippets/bindings/used.js'), false);
    assert.equal(refreshed.files.includes('/snippets/bindings/nested.js'), false);
    assert.doesNotMatch(fs.readFileSync(path.join(dist, 'index.html'), 'utf8'), /used\.js/);
    assert.ok(before.files.includes("/about.html"));
    const about = fs.readFileSync(path.join(dist, "about.html"), "utf8");
    assert.doesNotMatch(about, /SIL OPEN FONT LICENSE/);
    const software = before.files.find(file => /^\/about-software-[a-f0-9]+\.html$/.test(file));
    assert.ok(software);
    assert.ok(about.includes(software));
    assert.match(fs.readFileSync(path.join(dist, software.slice(1)), 'utf8'), /SIL OPEN FONT LICENSE/);
    assert.match(about, /styles-test.css/);
    for (const family of ['Neon', 'Argon', 'Xenon', 'Radon', 'Krypton']) assert.ok(before.files.includes(`/fonts/Monaspace${family}-v1.400.woff2`));
    fs.appendFileSync(path.join(dist, 'fonts/MonaspaceNeon-v1.400.woff2'), 'changed');
    assert.notEqual(build().build, refreshed.build);
  } finally { fs.rmSync(dist, {recursive:true, force:true}); }
});


test('About and notices remain available on offline navigation without API requests', async () => {
  const host = worker(async () => {throw Error('offline');});
  assert.match(await (await host.request('https://ide.test/about.html', 'GET', 'navigate')).text(), /license notices/);
  assert.match(fs.readFileSync(path.join(__dirname, 'offline.html'), 'utf8'), /href="\/about.html"/);
});

function pushWorker({user = 7, enabled = true, clients = [], offline = false} = {}) {
  const host = worker(async url => {
    if (offline) throw Error('offline');
    return new Response(JSON.stringify({user_id: user, enabled}));
  });
  // Recreate with browser primitives so the worker exercises its real handlers.
  const shown = [], opened = [], handlers = {};
  const context = {BUILD_ID: 'push', SHELL_FILES: [], URL, Request, MessageChannel, setTimeout, clearTimeout,
    fetch: async (url, options) => {assert.equal(options.headers['x-openwebide'], '1'); if (offline) throw Error('offline'); return new Response(JSON.stringify({user_id: user, enabled}));},
    self: {location: {origin: 'https://ide.test'}, clients: {matchAll: async () => clients, openWindow: async url => opened.push(url)}, registration: {showNotification: async (...args) => shown.push(args)}, addEventListener: (name, action) => {handlers[name] = action;}}};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'service-worker.js'), 'utf8'), context);
  const data = {title: 'Run finished · Project', body: 'Session', tag: 'openwebide-11-done:20', user_id: 7, session_id: 11};
  async function event(type, value = data) {
    let task;
    handlers[type]({data: {json: () => value}, notification: {data: value, close() {}}, waitUntil: promise => {task = promise;}});
    await task;
  }
  return {shown, opened, event, data, host};
}
function pushClient(context) {
  const client = {messages: [], focused: false, focus: async () => {client.focused = true;}, postMessage: (message, ports) => {
    if (ports) {ports[0].postMessage(context); ports[0].close();}
    else client.messages.push(message);
  }};
  return client;
}
test('push checks account and opt-out before showing or opening a notification', async () => {
  for (const options of [{user: 8}, {enabled: false}, {offline: true}]) {
    const host = pushWorker(options);
    await host.event('push'); await host.event('notificationclick');
    assert.equal(host.shown.length, 0); assert.equal(host.opened.length, 0);
  }
  const host = pushWorker();
  await host.event('push');
  assert.equal(host.shown[0][0], 'Run finished · Project');
  assert.equal(host.shown[0][1].body, 'Session');
  await host.event('notificationclick');
  assert.equal(host.opened[0], '/?notification_user=7&notification_session=11');
});
test('attended chat suppresses push; a hidden or different chat still receives it', async () => {
  for (const context of [{user_id: 7, session_id: 11, chat_visible: true, focused: true}, {user_id: 7, session_id: 10, chat_visible: true, focused: true}, {user_id: 7, session_id: 11, chat_visible: true, focused: false}]) {
    const client = pushClient(context), host = pushWorker({clients: [client]});
    await host.event('push');
    assert.equal(host.shown.length, context.session_id === 11 && context.focused ? 0 : 1);
    await host.event('notificationclick');
    assert.equal(client.focused, true);
    assert.equal(client.messages[0].session_id, 11);
    assert.equal(host.opened.length, 0);
  }
});
test('malformed notification payloads cannot display or navigate', async () => {
  const host = pushWorker();
  for (const data of [null, {...host.data, user_id: -1}, {...host.data, session_id: '11'}, {...host.data, title: 'x'.repeat(300)}, {...host.data, tag: 'foreign'}]) {
    await host.event('push', data); await host.event('notificationclick', data);
  }
  assert.equal(host.shown.length, 0); assert.equal(host.opened.length, 0);
});

function pushBrowser(permission = 'granted') {
  let subscriptions = 0, unsubscribed = 0;
  let existing = null;
  const listeners = {};
  const registration = {getNotifications: async () => [], pushManager: {
    getSubscription: async () => existing,
    subscribe: async options => {subscriptions++; existing = {options, toJSON: () => ({endpoint: 'https://fcm.googleapis.com/device', keys: {p256dh: 'key', auth: 'auth'}}), unsubscribe: async () => {unsubscribed++; existing = null;}}; return existing;}
  }};
  const window = {location: {href: 'https://ide.test/'}, isSecureContext: true, PushManager: class {}, addEventListener() {}, history: {replaceState() {}}};
  const navigator = {serviceWorker: {ready: Promise.resolve(registration), getRegistration: async () => registration, addEventListener: (event, callback) => {listeners[event] = callback;}, removeEventListener: event => delete listeners[event]}};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'pwa.js'), 'utf8'), {window, navigator, URL, Notification: {permission}, Uint8Array, atob, setTimeout, clearTimeout, matchMedia: () => ({addEventListener() {}}), MutationObserver: class {observe() {}}, document: {documentElement: {}, visibilityState: 'visible', hasFocus: () => true}});
  return {push: window.webidePush, registration, counts: () => ({subscriptions, unsubscribed}), listeners};
}
test('push subscriptions reuse the browser registration and stop on opt-out', async () => {
  const host = pushBrowser();
  host.push.context({user_id: 7, enabled: true, revision: 1});
  const key = 'BAECAw';
  await host.push.subscribe(key, 1); await host.push.subscribe(key, 1);
  assert.equal(host.counts().subscriptions, 1);
  host.push.context({user_id: 7, enabled: false, revision: 2});
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(host.counts().unsubscribed, 1);
  await assert.rejects(host.push.subscribe(key, 1), /Account changed/);
  const denied = pushBrowser('denied');
  denied.push.context({user_id: 7, enabled: true, revision: 1});
  await assert.rejects(denied.push.subscribe(key, 1), /unavailable/);
  assert.equal(denied.counts().subscriptions, 0);
});
test('a delayed browser subscription cannot become ready for a different account', async () => {
  const host = pushBrowser();
  let release;
  const original = host.registration.pushManager.subscribe;
  host.registration.pushManager.subscribe = async options => {await new Promise(resolve => {release = resolve;}); return original(options);};
  host.push.context({user_id: 7, enabled: true, revision: 1});
  const result = host.push.subscribe('BAECAw', 1);
  await new Promise(resolve => setImmediate(resolve));
  host.push.context({user_id: 8, enabled: true, revision: 2});
  release();
  await assert.rejects(result, /Account changed/);
  await host.push.subscribe('BAECAw', 2);
  assert.equal(host.counts().subscriptions, 1);
});
