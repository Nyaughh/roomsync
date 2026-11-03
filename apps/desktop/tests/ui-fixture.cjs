'use strict';
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');

const uiPath = path.join(__dirname, '..', 'ui');
const freshState = () => ({
  phase: 'idle', title: 'Ready to set up', message: 'Synthetic UI test fixture.',
  peers: [], packets: 0, lead_ms: null, capture_allowance_ms: 0,
  capture_peak: 0, source_peak: 0, capture_age_ms: 0,
  receiver_count: 0, prepared: false, windows_supported: true,
});

async function fixture() {
  let state = freshState();
  let settings = null;
  let disconnected = false;
  let supported = true;
  let interfaces = [{ address: '192.0.2.20', name: 'Wi-Fi (test fixture)' }];
  const calls = [];
  const server = http.createServer(async (request, response) => {
    if (!request.url.startsWith('/api/')) {
      if (request.url === '/away') {
        response.end('<!doctype html><title>Other page</title><p>Other page</p>');
        return;
      }
      const file = request.url === '/' ? 'index.html' : request.url.slice(1);
      if (!['index.html', 'app.js', 'style.css'].includes(file)) {
        response.writeHead(404); response.end(); return;
      }
      response.setHeader('Content-Type', file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html');
      response.end(fs.readFileSync(path.join(uiPath, file), 'utf8').replace('__NONCE__', 'fixture-nonce').replace('__CSRF__', 'fixture-control-token'));
      return;
    }
    response.setHeader('Content-Type', 'application/json');
    if (request.headers['x-roomsync-csrf'] !== 'fixture-control-token') {
      response.writeHead(403); response.end(JSON.stringify({ error: 'Missing control token' })); return;
    }
    if (request.method === 'POST' && request.headers.origin !== `http://${request.headers.host}`) {
      response.writeHead(403); response.end(JSON.stringify({ error: 'Wrong origin' })); return;
    }
    let body = '';
    for await (const chunk of request) body += chunk;
    const data = body ? JSON.parse(body) : null;
    calls.push({ path: request.url, method: request.method, body: data });
    const reply = value => response.end(JSON.stringify(value));
    switch (request.url) {
      case '/api/config':
        reply({ windows_supported: supported, ui_test: false, interfaces, devices: [{ id: 'headphones', name: 'Headphones' }], prepared_settings: settings });
        break;
      case '/api/status':
        if (disconnected) { response.writeHead(503); reply({ error: 'Fixture offline' }); }
        else reply({ ...state, windows_supported: supported });
        break;
      case '/api/prepare':
        settings = data;
        state = { ...freshState(), phase: 'prepared', title: 'Your private setup is ready', prepared: true, receiver_count: data.receivers, peers: Array.from({ length: data.receivers }, (_, index) => ({ index, message: 'Not listening yet' })) };
        reply({ ok: true });
        break;
      case '/api/setup':
        await new Promise(resolve => setTimeout(resolve, 70));
        reply({ payload: `RS1|SYNTHETIC-UI-FIXTURE-NOT-A-REAL-CODE|${data.index}`, modules: Array.from({ length: 21 }, () => '101010101010101010101') });
        break;
      case '/api/start':
        state = { ...state, phase: 'listening', title: 'Waiting for Android' };
        reply({ ok: true });
        break;
      case '/api/stop':
        state = { ...state, phase: 'prepared', title: 'Stopped' };
        reply({ ok: true });
        break;
      default:
        response.writeHead(404); reply({ error: 'Unknown fixture endpoint' });
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    calls,
    state: update => { state = { ...state, ...update }; },
    disconnect: value => { disconnected = value; },
    supported: value => { supported = value; },
    interfaces: value => { interfaces = value; },
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }),
  };
}

module.exports = { fixture };
if (require.main === module) fixture().then(mock => console.log(mock.url));
