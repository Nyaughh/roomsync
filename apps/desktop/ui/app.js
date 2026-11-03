'use strict';

const $ = id => document.getElementById(id);
const settingIds = ['hostIp', 'count', 'port', 'device', 'latency'];
let busy = false;
let prepared = false;
let supported = false;
let active = false;
let dirty = false;
let hasNetwork = false;
let configured = false;
let online = false;
let phase = 'idle';
let statusRequest = 0;
let appliedStatusRequest = 0;

async function api(path, method = 'GET', body) {
  const response = await fetch(path, {
    method,
    headers: {
      'X-RoomSync-CSRF': window.ROOMSYNC_CSRF,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    cache: 'no-store',
    credentials: 'same-origin',
  });
  const value = await response.json();
  if (!response.ok) throw Error(value.error || `Request failed (${response.status})`);
  return value;
}

function text(id, value) {
  if ($(id).textContent !== value) $(id).textContent = value;
}

function error(value) {
  text('error', value ? String(value.message || value) : '');
  $('error').hidden = !value;
}

function controls() {
  $('prepare').disabled = busy || active || !hasNetwork || !online;
  text('prepare', dirty ? 'Update setup code' : prepared ? 'Create a new code' : 'Create setup code');
  $('prepare').classList.toggle('secondary', prepared && !dirty);
  $('start').disabled = busy || !prepared || dirty || active || !supported || !online;
  $('start').hidden = active;
  $('stop').disabled = busy || !active || phase === 'stopping';
  $('stop').hidden = !active;
  $('peer').disabled = busy;
  $('reveal').disabled = busy || dirty || !prepared || !online;
  for (const id of settingIds) $(id).disabled = busy || active || !configured;
}

function hideSecret() {
  $('secret').hidden = true;
  $('payload').value = '';
  const canvas = $('qr');
  canvas.getContext('2d').clearRect(0, 0, canvas.width, canvas.height);
  text('copy', 'Copy code');
  text('reveal', 'Show private code');
  $('reveal').setAttribute('aria-expanded', 'false');
}

async function task(fn) {
  if (busy) return;
  busy = true;
  controls();
  error(null);
  try {
    await fn();
  } catch (value) {
    error(value);
  } finally {
    busy = false;
    controls();
  }
}

function showStatus(state) {
  const labels = {
    idle: 'Idle', prepared: 'Ready', listening: 'Listening',
    calibrating: 'Calibrating', synchronizing: 'Starting',
    streaming: 'Live', stopping: 'Stopping', error: 'Needs attention',
  };
  const messages = {
    prepared: 'Start listening here and connect in Android, in either order.',
    listening: 'Connect in Android whenever you’re ready. There’s no time limit.',
    calibrating: 'Measuring each device’s output queue. Audio will start shortly.',
    synchronizing: 'Giving your devices a moment to align before audio starts.',
    streaming: 'Sending this PC’s audio to your connected devices.',
    stopping: 'Closing the audio stream and connections.',
  };
  text('state', state.phase === 'prepared' ? 'Ready to connect' : state.title);
  text('statusBadge', labels[state.phase] || state.phase);
  $('statusBadge').dataset.phase = state.phase;
  text('message', !state.windows_supported && state.prepared
    ? 'Setup is ready. Starting audio requires the Windows app.'
    : messages[state.phase] || state.message);
  text('detailMessage', state.message);
  const metrics = state.lead_ms !== null
    ? `Shared lead ${Number.isInteger(state.lead_ms) ? state.lead_ms : state.lead_ms.toFixed(1)} ms · Capture allowance ${state.capture_allowance_ms.toFixed(1)} ms · Source packets ${state.packets.toLocaleString()} · Input peak ${(state.capture_peak * 100).toFixed(1)}% · Encoded peak ${(state.source_peak * 100).toFixed(1)}% · Capture age ${state.capture_age_ms.toFixed(1)} ms`
    : '';
  text('metrics', metrics);
  $('metrics').hidden = !metrics;
  const peerText = state.peers.map(peer => `Device ${peer.index + 1}: ${peer.message}`);
  if (JSON.stringify(peerText) !== $('peers').dataset.content) {
    $('peers').replaceChildren(...peerText.map(message => {
      const li = document.createElement('li');
      li.textContent = message;
      return li;
    }));
    $('peers').dataset.content = JSON.stringify(peerText);
  }
}

async function refresh() {
  const request = ++statusRequest;
  try {
    const state = await api('/api/status');
    if (request < appliedStatusRequest) return;
    appliedStatusRequest = request;
    online = true;
    prepared = state.prepared;
    supported = state.windows_supported;
    phase = state.phase;
    active = ['listening', 'calibrating', 'synchronizing', 'streaming', 'stopping'].includes(phase);
    showStatus(state);
    $('pairing').hidden = !prepared || dirty;
    $('session').hidden = !prepared || dirty;
    $('diagnostics').hidden = !prepared;
    $('peerField').hidden = state.receiver_count < 2;
    if ($('peer').options.length !== state.receiver_count) {
      $('peer').replaceChildren(...Array.from({ length: state.receiver_count }, (_, index) => new Option(`Device ${index + 1}`, String(index))));
      hideSecret();
    }
    controls();
  } catch (value) {
    if (request < appliedStatusRequest) return;
    appliedStatusRequest = request;
    online = false;
    hideSecret();
    $('session').hidden = false;
    text('state', 'Connection lost');
    text('statusBadge', 'Offline');
    $('statusBadge').dataset.phase = 'disconnected';
    text('message', 'Keep the RoomSync console open, or restart the app to reconnect.');
    controls();
  }
}

$('prepare').onclick = () => task(async () => {
  hideSecret();
  const invalid = ['port', 'latency'].map($).find(input => !input.checkValidity());
  if (invalid) {
    $('advanced').open = true;
    invalid.reportValidity();
    return;
  }
  await api('/api/prepare', 'POST', {
    host_ip: $('hostIp').value,
    base_port: Number($('port').value),
    receivers: Number($('count').value),
    latency_ms: Number($('latency').value),
    capture_device: $('device').value || null,
  });
  dirty = false;
  await refresh();
});

$('start').onclick = () => task(async () => {
  await api('/api/start', 'POST', {});
  await refresh();
});

$('stop').onclick = () => task(async () => {
  await api('/api/stop', 'POST', {});
  await refresh();
});

$('peer').onchange = hideSecret;
$('reveal').onclick = () => task(async () => {
  if (!$('secret').hidden) {
    hideSecret();
    return;
  }
  const selected = Number($('peer').value);
  const setup = await api('/api/setup', 'POST', { index: selected });
  if (Number($('peer').value) !== selected || dirty || !online) return;
  $('payload').value = setup.payload;
  const rows = setup.modules;
  const canvas = $('qr');
  const cell = 4;
  const size = (rows.length + 8) * cell;
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext('2d');
  context.fillStyle = '#fff';
  context.fillRect(0, 0, size, size);
  context.fillStyle = '#000';
  rows.forEach((row, y) => [...row].forEach((bit, x) => {
    if (bit === '1') context.fillRect((x + 4) * cell, (y + 4) * cell, cell, cell);
  }));
  $('secret').hidden = false;
  text('reveal', 'Hide private code');
  $('reveal').setAttribute('aria-expanded', 'true');
});

$('copy').onclick = () => task(async () => {
  try {
    await navigator.clipboard.writeText($('payload').value);
    text('copy', 'Copied');
  } catch {
    $('payload').focus();
    $('payload').select();
    throw Error('Clipboard access was unavailable. The complete code is selected; copy it manually.');
  }
});

(async () => {
  try {
    const initial = await api('/api/config');
    supported = initial.windows_supported;
    hasNetwork = initial.interfaces.length > 0;
    $('hostIp').replaceChildren(...initial.interfaces.map(network => new Option(`${network.name} · ${network.address}`, network.address)));
    for (const device of initial.devices) $('device').add(new Option(device.name, device.id));
    if (initial.prepared_settings) {
      const settings = initial.prepared_settings;
      $('hostIp').value = settings.host_ip;
      $('count').value = settings.receivers;
      $('port').value = settings.base_port;
      $('latency').value = settings.latency_ms;
      $('device').value = settings.capture_device || '';
    }
    const notice = !hasNetwork
      ? 'No local network found. Connect to Wi-Fi or Ethernet and restart RoomSync.'
      : initial.ui_test
        ? 'UI test mode. This is a synthetic network address. Audio cannot start here.'
        : supported ? '' : 'Setup only. Real audio capture requires Windows.';
    text('platform', notice);
    $('platform').hidden = !notice;
    configured = true;
    await refresh();
  } catch (value) {
    error(value);
    controls();
  }
  setInterval(refresh, 1000);
})();

window.addEventListener('pagehide', hideSecret);
for (const id of settingIds) {
  $(id).addEventListener('input', () => {
    if (!prepared) return;
    dirty = true;
    hideSecret();
    $('pairing').hidden = true;
    $('session').hidden = true;
    error('Settings changed. Update the setup code, then import the new code in Android.');
    controls();
  });
}
