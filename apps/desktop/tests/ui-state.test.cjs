'use strict';

// Dependency-free state tests. These exercise the real app.js against a small
// DOM/API model; the separate Playwright suite verifies browser behavior/layout.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');
const html = fs.readFileSync(path.join(__dirname, '..', 'ui', 'index.html'), 'utf8');
const script = fs.readFileSync(path.join(__dirname, '..', 'ui', 'app.js'), 'utf8');

class Element {
  constructor(id, attributes = '', tag = '') {
    this.id = id;
    this.tag = tag;
    this.hidden = /\bhidden\b/.test(attributes);
    this.disabled = /\bdisabled\b/.test(attributes);
    this.value = attributes.match(/\bvalue="([^"]*)"/)?.[1] || '';
    this.required = /\brequired\b/.test(attributes);
    this.checked = /\bchecked\b/.test(attributes);
    this.min = attributes.match(/\bmin="([^"]*)"/)?.[1];
    this.max = attributes.match(/\bmax="([^"]*)"/)?.[1];
    this.textContent = '';
    this.dataset = {};
    this.options = [];
    this.children = [];
    this.attributes = {};
    this.listeners = {};
    this.width = 270;
    this.height = 270;
    this.classList = { toggle() {} };
    this.context = { clearRect: () => { this.cleared = true; }, fillRect: () => { this.cleared = false; } };
  }
  replaceChildren(...children) { this.children = children; if (this.tag === 'select') { this.options = children; this.value = children[0]?.value || ''; } }
  add(child) { this.options.push(child); }
  setAttribute(name, value) { this.attributes[name] = value; }
  addEventListener(event, handler) { this.listeners[event] = handler; }
  getContext() { return this.context; }
  checkValidity() { return (!this.required || this.value !== '') && (this.min === undefined || Number(this.value) >= Number(this.min)) && (this.max === undefined || Number(this.value) <= Number(this.max)); }
  reportValidity() { this.validityReported = true; return this.checkValidity(); }
  focus() { this.focused = true; }
  select() { this.selected = true; }
}

function app(options = {}) {
  const elements = {};
  for (const match of html.matchAll(/<([a-z0-9]+)\b([^>]*\bid="([^"]+)"[^>]*)>/g)) elements[match[3]] = new Element(match[3], match[2], match[1]);
  elements.count.value = '1';
  elements.device.value = '';
  const calls = [];
  let settings = options.settings || null;
  let state = {
    phase: settings ? 'prepared' : 'idle', title: 'Fixture state', message: 'Fixture details',
    prepared: !!settings, windows_supported: options.supported ?? true,
    receiver_count: settings?.receivers || 0, peers: [], lead_ms: null, packets: 0,
    capture_allowance_ms: 0, capture_peak: 0, source_peak: 0, capture_age_ms: 0,
    local_output_enabled: !!settings?.local_output_device, local_output_message: 'Fixture local output',
    local_horizon_ms: null, local_concealed_frames: 0, local_late_packets: 0, local_too_far_packets: 0, local_output_peak: 0,
  };
  let offline = false;
  let copyFails = false;
  const events = {};
  let copied = '';
  const context = vm.createContext({
    document: { getElementById: id => { assert.ok(elements[id], `Missing HTML element: ${id}`); return elements[id]; }, createElement: tag => new Element('', '', tag) },
    window: { ROOMSYNC_CSRF: 'test-csrf', addEventListener: (event, handler) => { events[event] = handler; } },
    navigator: { clipboard: { writeText: async value => { if (copyFails) throw Error('Clipboard denied'); copied = value; } } },
    Option: class { constructor(text, value) { this.textContent = text; this.value = value; } },
    setInterval: callback => { events.poll = callback; },
    fetch: async (url, request) => {
      assert.equal(request.headers['X-RoomSync-CSRF'], 'test-csrf');
      assert.equal(request.cache, 'no-store');
      assert.equal(request.credentials, 'same-origin');
      const body = request.body && JSON.parse(request.body);
      calls.push({ url, method: request.method, body });
      const ok = value => ({ ok: true, json: async () => structuredClone(value) });
      switch (url) {
        case '/api/config': return ok({ windows_supported: state.windows_supported, interfaces: options.noNetwork ? [] : [{ address: '192.0.2.1', name: 'Synthetic fixture' }], devices: [{ id: 'virtual', name: 'CABLE Input (fixture)' }, { id: 'speakers', name: 'Physical speakers (fixture)' }, { id: 'headphones', name: 'Headphones (fixture)' }], prepared_settings: settings, ui_test: options.uiTest || false });
        case '/api/status': if (offline) throw Error('Offline fixture'); return ok(state);
        case '/api/prepare':
          settings = body;
          state = { ...state, prepared: true, phase: 'prepared', local_output_enabled: !!body.local_output_device, receiver_count: body.receivers, peers: Array.from({ length: body.receivers }, (_, index) => ({ index, message: 'Not listening yet' })) };
          return ok({ ok: true });
        case '/api/setup': return ok({ payload: `SYNTHETIC-SECRET-${body.index}`, modules: ['10', '01'] });
        case '/api/start': state.phase = 'listening'; state.title = 'Waiting for Android'; return ok({ ok: true });
        case '/api/stop': state.phase = 'prepared'; state.title = 'Stopped'; return ok({ ok: true });
        default: throw Error(`Unknown fixture endpoint ${url}`);
      }
    },
  });
  vm.runInContext(script, context);
  return {
    elements, calls, events,
    settle: () => new Promise(resolve => setImmediate(resolve)),
    refresh: () => vm.runInContext('refresh()', context),
    update: next => { state = { ...state, ...next }; },
    offline: value => { offline = value; },
    copyFails: value => { copyFails = value; },
    copied: () => copied,
  };
}

test('setup stays minimal and secrets require an explicit reveal', async () => {
  const model = app(); const e = model.elements; await model.settle();
  assert.equal(e.prepare.disabled, false);
  assert.equal(e.latency.value, '40');
  assert.equal(e.pairing.hidden, true);
  assert.equal(e.session.hidden, true);
  assert.equal(e.diagnostics.hidden, true);
  assert.equal(e.error.hidden, true);
  assert.equal(model.calls.some(call => call.url === '/api/setup'), false);
  await e.prepare.onclick();
  assert.equal(model.calls.find(call => call.url === '/api/prepare').body.latency_ms, 40);
  assert.equal(e.pairing.hidden, false);
  assert.equal(e.secret.hidden, true);
  assert.equal(e.peerField.hidden, true);
  assert.equal(e.start.disabled, false);
  assert.equal(model.calls.some(call => call.url === '/api/setup'), false);
  await e.reveal.onclick();
  assert.equal(e.secret.hidden, false);
  assert.equal(e.reveal.attributes['aria-expanded'], 'true');
  assert.equal(e.payload.value, 'SYNTHETIC-SECRET-0');
  await e.reveal.onclick();
  assert.equal(e.secret.hidden, true);
  assert.equal(e.payload.value, '');
  assert.equal(e.qr.cleared, true);
});

test('device switching, settings changes and page dismissal clear secrets', async () => {
  const model = app(); const e = model.elements; await model.settle();
  e.count.value = '2'; await e.prepare.onclick();
  assert.equal(e.peerField.hidden, false);
  await e.reveal.onclick(); e.peer.value = '1'; e.peer.onchange();
  assert.equal(e.payload.value, '');
  await e.reveal.onclick(); assert.equal(e.payload.value, 'SYNTHETIC-SECRET-1');
  e.latency.value = '120'; e.latency.listeners.input();
  assert.equal(e.secret.hidden, true); assert.equal(e.pairing.hidden, true); assert.equal(e.start.disabled, true);
  assert.match(e.prepare.textContent, /Update/);
  await e.prepare.onclick();
  assert.equal(model.calls.filter(call => call.url === '/api/prepare').at(-1).body.latency_ms, 120);
  await e.reveal.onclick(); model.events.pagehide();
  assert.equal(e.payload.value, ''); assert.equal(e.secret.hidden, true);
});

test('double clicks coalesce; Start and Stop preserve setup; diagnostics retain fractional lead', async () => {
  const model = app(); const e = model.elements; await model.settle(); await e.prepare.onclick();
  await Promise.all([e.reveal.onclick(), e.reveal.onclick()]);
  assert.equal(model.calls.filter(call => call.url === '/api/setup').length, 1);
  await Promise.all([e.start.onclick(), e.start.onclick()]);
  assert.equal(model.calls.filter(call => call.url === '/api/start').length, 1);
  assert.equal(e.start.hidden, true); assert.equal(e.stop.hidden, false); assert.equal(e.hostIp.disabled, true);
  model.update({ phase: 'streaming', lead_ms: 92.5, packets: 900, capture_allowance_ms: 12.5 }); await model.refresh();
  assert.equal(e.statusBadge.textContent, 'Live'); assert.match(e.metrics.textContent, /Shared lead 92.5 ms/);
  assert.match(e.metrics.textContent, /Capture allowance 12.5 ms/);
  assert.equal(e.diagnostics.hidden, false); assert.equal(e.peers.children.length, 1);
  await e.stop.onclick();
  assert.equal(e.start.hidden, false); assert.equal(e.stop.hidden, true); assert.equal(e.hostIp.disabled, false);
  assert.equal(e.pairing.hidden, false);
});

test('controller interruption hides secrets and prevents Start until reconnected', async () => {
  const model = app(); const e = model.elements; await model.settle(); await e.prepare.onclick(); await e.reveal.onclick();
  model.offline(true); await model.refresh();
  assert.equal(e.statusBadge.textContent, 'Offline'); assert.equal(e.secret.hidden, true);
  assert.equal(e.payload.value, ''); assert.equal(e.start.disabled, true); assert.equal(e.prepare.disabled, true);
  model.offline(false); await model.refresh();
  assert.equal(e.start.disabled, false); assert.equal(e.secret.hidden, true);
});

test('copy fallback selects the complete code and clears its error after success', async () => {
  const model = app(); const e = model.elements; await model.settle(); await e.prepare.onclick(); await e.reveal.onclick();
  model.copyFails(true); await e.copy.onclick();
  assert.equal(e.payload.selected, true); assert.equal(e.payload.focused, true);
  assert.equal(e.error.hidden, false); assert.match(e.error.textContent, /copy it manually/);
  model.copyFails(false); await e.copy.onclick();
  assert.equal(model.copied(), e.payload.value); assert.equal(e.copy.textContent, 'Copied'); assert.equal(e.error.hidden, true);
});

test('invalid hidden settings are revealed before validation and never submitted', async () => {
  const model = app(); const e = model.elements; await model.settle(); e.port.value = '';
  await e.prepare.onclick();
  assert.equal(e.advanced.open, true); assert.equal(e.port.validityReported, true);
  assert.equal(model.calls.some(call => call.url === '/api/prepare'), false);
});

test('unsupported platforms and missing networks stay explicit and cannot start audio', async () => {
  for (const options of [{ supported: false }, { noNetwork: true }, { supported: false, uiTest: true }]) {
    const model = app(options); const e = model.elements; await model.settle();
    assert.equal(e.platform.hidden, false); assert.equal(e.start.disabled, true);
    if (options.noNetwork) { assert.equal(e.prepare.disabled, true); assert.match(e.platform.textContent, /No local network/); }
    else { await e.prepare.onclick(); assert.equal(e.start.disabled, true); assert.match(e.message.textContent, /requires the Windows app/); }
    if (options.uiTest) assert.match(e.platform.textContent, /synthetic/);
  }
});

test('saved settings survive page initialization without exposing secrets', async () => {
  const model = app({ settings: { host_ip: '192.0.2.1', receivers: 2, base_port: 5050, latency_ms: 170, capture_device: null } });
  const e = model.elements; await model.settle();
  assert.equal(e.latency.value, 170); assert.equal(e.port.value, 5050); assert.equal(e.count.value, 2);
  assert.equal(e.secret.hidden, true); assert.equal(e.start.disabled, false);
});


function enableLocal(model) {
  const e = model.elements;
  e.localEnabled.checked = true; e.localEnabled.listeners.input();
  e.device.value = 'virtual'; e.device.listeners.input();
  e.localOutput.value = 'speakers'; e.localOutput.listeners.input();
}

test('phone-only is the default and sends no local route authorization', async () => {
  const model = app(); const e = model.elements; await model.settle();
  assert.equal(e.localEnabled.checked, false); assert.equal(e.routeConfirmed.checked, false);
  assert.equal(e.localOptions.hidden, true); assert.equal(e.routeReminder.hidden, true);
  assert.equal(e.localOutput.disabled, true);
  await e.prepare.onclick();
  const sent = model.calls.find(call => call.url === '/api/prepare').body;
  assert.equal(sent.local_output_device, null); assert.equal(sent.virtual_route_confirmed, false);
});

test('local playback requires explicit different endpoints and routing acknowledgment', async () => {
  const model = app(); const e = model.elements; await model.settle();
  e.localEnabled.checked = true; e.localEnabled.listeners.input();
  assert.equal(e.localOptions.hidden, false); assert.equal(e.routeReminder.hidden, false);
  assert.equal(e.deviceLabel.textContent, 'Virtual capture output');
  await e.prepare.onclick(); assert.match(e.error.textContent, /explicit virtual capture/);
  e.device.value = 'virtual'; e.device.listeners.input();
  await e.prepare.onclick(); assert.match(e.error.textContent, /physical speakers/);
  e.localOutput.value = 'virtual'; e.localOutput.listeners.input();
  await e.prepare.onclick(); assert.match(e.error.textContent, /feedback/);
  e.localOutput.value = 'speakers'; e.localOutput.listeners.input();
  await e.prepare.onclick(); assert.match(e.error.textContent, /direct monitoring is off/);
  assert.equal(model.calls.some(call => call.url === '/api/prepare'), false);
  e.routeConfirmed.checked = true; e.routeConfirmed.listeners.input();
  await e.prepare.onclick();
  const sent = model.calls.find(call => call.url === '/api/prepare').body;
  assert.equal(sent.capture_device, 'virtual'); assert.equal(sent.local_output_device, 'speakers');
  assert.equal(sent.virtual_route_confirmed, true); assert.equal(e.start.disabled, false);
});

test('route changes reset acknowledgment, clear private code, and require updated setup', async () => {
  const model = app(); const e = model.elements; await model.settle(); enableLocal(model);
  e.routeConfirmed.checked = true; await e.prepare.onclick(); await e.reveal.onclick();
  e.localOutput.value = 'headphones'; e.localOutput.listeners.input();
  assert.equal(e.routeConfirmed.checked, false); assert.equal(e.secret.hidden, true);
  assert.equal(e.start.disabled, true); assert.equal(e.pairing.hidden, true);
  await e.prepare.onclick(); assert.match(e.error.textContent, /direct monitoring is off/);
  e.routeConfirmed.checked = true; e.routeConfirmed.listeners.input(); await e.prepare.onclick();
  e.device.value = 'speakers'; e.device.listeners.input(); assert.equal(e.routeConfirmed.checked, false);
  e.routeConfirmed.checked = true; e.localEnabled.checked = false; e.localEnabled.listeners.input();
  assert.equal(e.routeConfirmed.checked, false); assert.equal(e.localOptions.hidden, true);
  assert.equal(e.routeReminder.hidden, false); assert.equal(e.deviceLabel.textContent, 'Capture output');
  await e.prepare.onclick();
  const sent = model.calls.filter(call => call.url === '/api/prepare').at(-1).body;
  assert.equal(sent.local_output_device, null); assert.equal(sent.virtual_route_confirmed, false);
});

test('local status and diagnostics stay available; Stop leaves the routing warning visible', async () => {
  const model = app(); const e = model.elements; await model.settle(); enableLocal(model);
  e.routeConfirmed.checked = true; await e.prepare.onclick(); await e.start.onclick();
  assert.equal(e.localEnabled.disabled, true); assert.equal(e.localOutput.disabled, true); assert.equal(e.routeConfirmed.disabled, true);
  model.update({ phase: 'streaming', local_output_message: 'Playing on the shared timeline', local_horizon_ms: 62.5, local_concealed_frames: 12, local_late_packets: 3, local_too_far_packets: 2, local_output_peak: .5 });
  await model.refresh();
  assert.equal(e.localStatus.hidden, false); assert.match(e.localStatus.textContent, /shared timeline/);
  assert.match(e.localMetrics.textContent, /62.5 ms/); assert.match(e.localMetrics.textContent, /Concealed frames 12/);
  assert.match(e.localMetrics.textContent, /Late packets 3/); assert.match(e.localMetrics.textContent, /Output peak 50.0%/);
  assert.match(e.localMetrics.textContent, /not proof of physical speaker alignment/);
  await e.stop.onclick(); assert.equal(e.routeReminder.hidden, false); assert.equal(e.localEnabled.disabled, false);
  assert.equal(e.start.disabled, false);
  model.offline(true); await model.refresh(); assert.match(e.localStatus.textContent, /unavailable/);
});

test('local saved settings restore only their saved explicit acknowledgment', async () => {
  for (const confirmation of [true, false]) {
    const model = app({ settings: { host_ip: '192.0.2.1', receivers: 1, base_port: 5000, latency_ms: 40, capture_device: 'virtual', local_output_device: 'speakers', virtual_route_confirmed: confirmation } });
    const e = model.elements; await model.settle();
    assert.equal(e.localEnabled.checked, true); assert.equal(e.device.value, 'virtual'); assert.equal(e.localOutput.value, 'speakers');
    assert.equal(e.routeConfirmed.checked, confirmation); assert.equal(e.localOptions.hidden, false);
    assert.equal(e.start.disabled, !confirmation);
    assert.equal(e.routeReminder.hidden, false); assert.equal(e.secret.hidden, true);
  }
});
