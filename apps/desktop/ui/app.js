'use strict';

const $ = id => document.getElementById(id);
const settingIds = ['hostIp', 'count', 'port', 'device', 'latency', 'localEnabled', 'localOutput', 'routeConfirmed'];
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
let localRouteWasUsed = false;

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
  const localEnabled = $('localEnabled').checked;
  const routeReady = !localEnabled || ($('device').value && $('localOutput').value
    && $('device').value !== $('localOutput').value && $('routeConfirmed').checked);
  $('start').disabled = busy || !prepared || dirty || active || !supported || !online || !routeReady;
  $('start').hidden = active;
  $('stop').disabled = busy || !active || phase === 'stopping';
  $('stop').hidden = !active;
  $('peer').disabled = busy;
  $('reveal').disabled = busy || dirty || !prepared || !online;
  for (const id of settingIds) $(id).disabled = busy || active || !configured;
  $('localOutput').disabled ||= !localEnabled;
  $('routeConfirmed').disabled ||= !localEnabled;
  $('localOptions').hidden = !localEnabled;
  $('capture-help').hidden = !localEnabled;
  $('routeReminder').hidden = !localEnabled && !localRouteWasUsed;
  text('deviceLabel', localEnabled ? 'Virtual capture output' : 'Capture output');
  text('playbackNote', localEnabled
    ? 'This PC and Android use one playback timeline. Physical speaker alignment is not yet verified.'
    : 'Keep the PC’s speakers inaudible to avoid echo. Headphones work.');
}

function validLocalRoute() {
  if (!$('localEnabled').checked) return true;
  $('advanced').open = true;
  if (!$('device').value) {
    error('Choose an explicit virtual capture output, such as CABLE Input. Windows default output is not allowed for local speaker playback.');
    $('device').focus();
    return false;
  }
  if (!$('localOutput').value) {
    error('Choose the physical speakers or headphones RoomSync should play through.');
    $('localOutput').focus();
    return false;
  }
  if ($('device').value === $('localOutput').value) {
    error('Capture and speakers must be different devices. Capturing RoomSync’s own output can cause feedback.');
    $('localOutput').focus();
    return false;
  }
  if (!$('routeConfirmed').checked) {
    error('Check the Windows routing steps and confirm that direct monitoring is off before creating the setup code.');
    $('routeConfirmed').focus();
    return false;
  }
  return true;
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
  $('localStatus').hidden = !state.local_output_enabled;
  text('localStatus', state.local_output_enabled ? `This PC: ${state.local_output_message}` : '');
  $('localMetrics').hidden = !state.local_output_enabled;
  text('localMetrics', state.local_output_enabled
    ? `PC output queue ${state.local_horizon_ms == null ? 'not measured yet' : `${state.local_horizon_ms.toFixed(1)} ms`} · Concealed frames ${(state.local_concealed_frames ?? 0).toLocaleString()} · Late packets ${(state.local_late_packets ?? 0).toLocaleString()} · Beyond-window packets ${(state.local_too_far_packets ?? 0).toLocaleString()} · Output peak ${((state.local_output_peak ?? 0) * 100).toFixed(1)}%. Digital output data is not proof of physical speaker alignment.`
    : '');
  const metrics = state.lead_ms !== null
    ? `Shared lead ${Number.isInteger(state.lead_ms) ? state.lead_ms : state.lead_ms.toFixed(1)} ms · Capture allowance ${state.capture_allowance_ms.toFixed(1)} ms · Source packets ${state.packets.toLocaleString()} · Input peak ${(state.capture_peak * 100).toFixed(1)}% · Encoded peak ${(state.source_peak * 100).toFixed(1)}% · Capture age ${state.capture_age_ms.toFixed(1)} ms`
    : '';
  text('metrics', metrics);
  $('metrics').hidden = !metrics;
  const peerText = state.peers.map(peer => {
    const d = peer.dispatch;
    const dispatch = d ? ` · Host sent ${d.packets_sent.toLocaleString()} packets; maximums this session: queue wait ${d.queue_wait_max_ms.toFixed(1)} ms, send call ${d.send_call_max_ms.toFixed(1)} ms, worker gap ${d.worker_gap_max_ms.toFixed(1)} ms; observed queue ${d.observed_queue_high_water}/128. These are host timings, not Wi-Fi or speaker measurements.` : '';
    return `Device ${peer.index + 1}: ${peer.message}${dispatch}`;
  });
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
    if (state.local_output_enabled) localRouteWasUsed = true;
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
    if (!$('localStatus').hidden) text('localStatus', 'This PC: Status unavailable while the controller is disconnected.');
    text('detailMessage', 'Last reported details. The controller connection is currently lost.');
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
  if (!validLocalRoute()) return;
  await api('/api/prepare', 'POST', {
    host_ip: $('hostIp').value,
    base_port: Number($('port').value),
    receivers: Number($('count').value),
    latency_ms: Number($('latency').value),
    capture_device: $('device').value || null,
    local_output_device: $('localEnabled').checked ? $('localOutput').value : null,
    virtual_route_confirmed: $('localEnabled').checked && $('routeConfirmed').checked,
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
    for (const device of initial.devices) {
      $('device').add(new Option(device.name, device.id));
      $('localOutput').add(new Option(device.name, device.id));
    }
    if (initial.prepared_settings) {
      const settings = initial.prepared_settings;
      $('hostIp').value = settings.host_ip;
      $('count').value = settings.receivers;
      $('port').value = settings.base_port;
      $('latency').value = settings.latency_ms;
      $('device').value = settings.capture_device || '';
      $('localEnabled').checked = !!settings.local_output_device;
      $('localOutput').value = settings.local_output_device || '';
      $('routeConfirmed').checked = !!settings.local_output_device && settings.virtual_route_confirmed === true;
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
    if (['localEnabled', 'localOutput', 'device'].includes(id)) $('routeConfirmed').checked = false;
    controls();
    if (!prepared) return;
    dirty = true;
    hideSecret();
    $('pairing').hidden = true;
    $('session').hidden = true;
    error('Settings changed. Update the setup code, then import the new code in Android.');
    controls();
  });
}
