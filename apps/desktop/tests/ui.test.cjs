'use strict';

// Frontend-only regression tests. Requires Playwright and Chromium in the dev
// environment; neither is a RoomSync runtime dependency. All device data is fake.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const { chromium } = require('playwright');

const { fixture } = require('./ui-fixture.cjs');

async function visible(page, selector) { return page.locator(selector).isVisible(); }
async function waitEnabled(page, selector) { await page.waitForFunction(selector => !document.querySelector(selector).disabled, selector); }

// Keep one browser to make these checks fast without adding a build framework.
test('minimal desktop UI retains setup, privacy, diagnostics and interrupted flows', async t => {
  const mock = await fixture();
  t.after(() => mock.close());
  const executablePath = process.env.CHROMIUM_PATH || (fs.existsSync('/usr/bin/chromium') ? '/usr/bin/chromium' : undefined);
  const browser = await chromium.launch({ headless: true, executablePath });
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  try {
    await page.goto(mock.url);
    await waitEnabled(page, '#prepare');
    assert.equal(await visible(page, '#pairing'), false);
    assert.equal(await visible(page, '#session'), false);
    assert.equal(await visible(page, '#diagnostics'), false);
    assert.equal(await page.locator('#advanced').getAttribute('open'), null);
    assert.equal(await page.locator('#latency').inputValue(), '40');
    assert.equal(await page.locator('#error').isVisible(), false);
    if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: path.join(process.env.UI_SCREENSHOTS, 'desktop-setup.png'), fullPage: true });

    await page.locator('#count').selectOption('2');
    await page.locator('#prepare').click();
    await waitEnabled(page, '#reveal');
    assert.equal(mock.calls.find(call => call.path === '/api/prepare').body.latency_ms, 40);
    assert.equal(await visible(page, '#pairing'), true);
    assert.equal(await visible(page, '#secret'), false);
    assert.equal(await visible(page, '#peerField'), true);
    assert.equal(mock.calls.filter(call => call.path === '/api/setup').length, 0, 'setup secrets are fetched only on explicit reveal');
    if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: path.join(process.env.UI_SCREENSHOTS, 'desktop-ready.png'), fullPage: true });

    await page.locator('#reveal').evaluate(button => { button.click(); button.click(); });
    await page.locator('#secret').waitFor({ state: 'visible' });
    assert.equal(mock.calls.filter(call => call.path === '/api/setup').length, 1, 'repeated clicks are coalesced');
    assert.equal(await page.locator('#reveal').getAttribute('aria-expanded'), 'true');
    assert.match(await page.locator('#payload').inputValue(), /FIXTURE/);
    await page.locator('#peer').selectOption('1');
    assert.equal(await visible(page, '#secret'), false);
    assert.equal(await page.locator('#payload').inputValue(), '');
    assert.equal(await page.locator('#qr').evaluate(canvas => canvas.getContext('2d').getImageData(0, 0, 1, 1).data[3]), 0);
    await page.locator('#reveal').click();
    await page.locator('#secret').waitFor({ state: 'visible' });
    assert.match(await page.locator('#payload').inputValue(), /\|1$/);
    await page.locator('#reveal').click();
    assert.equal(await visible(page, '#secret'), false);

    await page.locator('#reveal').click();
    await page.locator('#secret').waitFor({ state: 'visible' });
    await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async () => { throw Error('Denied in fixture'); } } }));
    await page.locator('#copy').click();
    await page.waitForFunction(() => document.querySelector('#error').textContent.includes('copy it manually'));
    assert.equal(await page.locator('#payload').evaluate(element => element.selectionEnd - element.selectionStart), (await page.locator('#payload').inputValue()).length);
    await page.evaluate(() => Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async value => { window.fixtureCopied = value; } } }));
    await page.locator('#copy').click();
    await page.waitForFunction(() => document.querySelector('#copy').textContent === 'Copied');
    assert.equal(await visible(page, '#error'), false);
    assert.match(await page.evaluate(() => window.fixtureCopied), /FIXTURE/);

    await page.locator('#advanced > summary').click();
    await page.locator('#latency').fill('120');
    assert.equal(await visible(page, '#secret'), false);
    assert.equal(await visible(page, '#pairing'), false);
    assert.equal(await page.locator('#start').isEnabled(), false);
    await page.locator('#prepare').click();
    await waitEnabled(page, '#start');
    assert.equal(mock.calls.filter(call => call.path === '/api/prepare').at(-1).body.latency_ms, 120, 'higher user minimum is preserved');
    await page.locator('#advanced > summary').click();
    await page.locator('#start').evaluate(button => { button.click(); button.click(); });
    await waitEnabled(page, '#stop');
    assert.equal(mock.calls.filter(call => call.path === '/api/start').length, 1);
    assert.equal(await page.locator('#hostIp').isEnabled(), false);
    assert.equal(await page.locator('#prepare').isEnabled(), false);
    assert.equal(await visible(page, '#start'), false);

    mock.state({ phase: 'streaming', title: 'Streaming', lead_ms: 142.5, capture_allowance_ms: 11.2, capture_peak: .4, source_peak: .39, capture_age_ms: 5.1, packets: 1800, peers: [{ index: 0, message: 'Authenticated fixture' }, { index: 1, message: 'Authenticated fixture' }] });
    await page.waitForFunction(() => document.querySelector('#statusBadge').textContent === 'Live');
    assert.equal(await visible(page, '#metrics'), false, 'metrics stay collapsed');
    await page.locator('#diagnostics > summary').click();
    assert.match(await page.locator('#metrics').textContent(), /Shared lead 142.5 ms/);
    assert.equal(await page.locator('#peers li').count(), 2);
    await page.locator('#stop').click();
    await waitEnabled(page, '#start');
    assert.equal(await page.locator('#hostIp').isEnabled(), true);
    assert.equal(await visible(page, '#stop'), false);
    assert.equal(await page.locator('#latency').inputValue(), '120');

    await page.locator('#reveal').click();
    await page.locator('#secret').waitFor({ state: 'visible' });
    mock.disconnect(true);
    await page.waitForFunction(() => document.querySelector('#statusBadge').textContent === 'Offline');
    assert.equal(await visible(page, '#secret'), false);
    assert.equal(await page.locator('#start').isEnabled(), false);
    assert.equal(await page.locator('#prepare').isEnabled(), false);
    mock.disconnect(false);
    await waitEnabled(page, '#start');
    assert.equal(await visible(page, '#secret'), false, 'reconnection never reveals secrets');
    await page.reload();
    await waitEnabled(page, '#start');
    assert.equal(await page.locator('#latency').inputValue(), '120');
    assert.equal(await visible(page, '#secret'), false);

    await page.locator('#reveal').click();
    await page.locator('#secret').waitFor({ state: 'visible' });
    await page.goto(`${mock.url}/away`);
    await page.goBack();
    await waitEnabled(page, '#start');
    assert.equal(await visible(page, '#secret'), false, 'Back does not restore a visible secret');

    await page.setViewportSize({ width: 375, height: 812 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
    if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: path.join(process.env.UI_SCREENSHOTS, 'desktop-mobile.png'), fullPage: true });
    await page.locator('#reveal').click();
    await page.locator('#secret').waitFor({ state: 'visible' });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, 'QR layout does not overflow small screens');
    await page.locator('#reveal').click();

    // Local output is opt-in and cannot use a default, identical or unconfirmed route.
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.locator('#advanced > summary').click();
    await page.locator('#localEnabled').check();
    assert.equal(await visible(page, '#localOptions'), true);
    assert.equal(await page.locator('#routeConfirmed').isChecked(), false);
    await page.locator('#prepare').click();
    await page.waitForFunction(() => document.querySelector('#error').textContent.includes('explicit virtual capture'));
    await page.locator('#device').selectOption('virtual');
    await page.locator('#localOutput').selectOption('virtual');
    await page.locator('#prepare').click();
    await page.waitForFunction(() => document.querySelector('#error').textContent.includes('feedback'));
    await page.locator('#localOutput').selectOption('speakers');
    await page.locator('#prepare').click();
    await page.waitForFunction(() => document.querySelector('#error').textContent.includes('direct monitoring is off'));
    if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: path.join(process.env.UI_SCREENSHOTS, 'desktop-local-routing.png'), fullPage: true });
    await page.locator('#routeConfirmed').check();
    await page.locator('#prepare').click();
    await waitEnabled(page, '#start');
    const localSetup = mock.calls.filter(call => call.path === '/api/prepare').at(-1).body;
    assert.equal(localSetup.capture_device, 'virtual');
    assert.equal(localSetup.local_output_device, 'speakers');
    assert.equal(localSetup.virtual_route_confirmed, true);
    await page.locator('#advanced > summary').click();
    assert.equal(await visible(page, '#routeReminder'), true, 'routing warning remains outside collapsed settings');
    await page.locator('#start').click();
    await waitEnabled(page, '#stop');
    mock.state({ phase: 'streaming', local_output_message: 'Playing on the shared timeline (fixture)', local_horizon_ms: 62.5, local_concealed_frames: 12, local_late_packets: 3, local_too_far_packets: 2, local_output_peak: .5 });
    await page.waitForFunction(() => document.querySelector('#localStatus').textContent.includes('shared timeline'));
    await page.locator('#diagnostics > summary').click();
    assert.match(await page.locator('#localMetrics').textContent(), /PC output queue 62.5 ms/);
    await page.locator('#stop').click();
    await waitEnabled(page, '#start');
    assert.equal(await visible(page, '#routeReminder'), true, 'after Stop warning remains visible');
    await page.reload();
    await waitEnabled(page, '#start');
    assert.equal(await page.locator('#routeConfirmed').isChecked(), true, 'only saved explicit confirmation is restored');
    assert.equal(await page.locator('#localOutput').inputValue(), 'speakers');
    await page.locator('#advanced > summary').click();
    await page.locator('#localOutput').selectOption('headphones');
    assert.equal(await page.locator('#routeConfirmed').isChecked(), false, 'route changes require fresh confirmation');
    assert.equal(await page.locator('#start').isEnabled(), false);
    await page.setViewportSize({ width: 375, height: 812 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, 'local route instructions fit narrow screens');
    if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: path.join(process.env.UI_SCREENSHOTS, 'desktop-local-mobile.png'), fullPage: true });
    await page.locator('#localEnabled').uncheck();
    await page.locator('#prepare').click();
    await waitEnabled(page, '#start');
    assert.equal(mock.calls.filter(call => call.path === '/api/prepare').at(-1).body.local_output_device, null);
    assert.equal(mock.calls.filter(call => call.path === '/api/prepare').at(-1).body.virtual_route_confirmed, false);

    mock.supported(false);
    await page.reload();
    await waitEnabled(page, '#prepare');
    assert.equal(await page.locator('#start').isEnabled(), false);
    assert.match(await page.locator('#platform').textContent(), /requires Windows/);
    mock.interfaces([]);
    await page.reload();
    await page.waitForFunction(() => document.querySelector('#platform').textContent.includes('No local network'));
    assert.equal(await page.locator('#prepare').isEnabled(), false);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});
