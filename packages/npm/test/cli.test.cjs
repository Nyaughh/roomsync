'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { EventEmitter } = require('node:events');
const { parseArgs, run } = require('../lib/cli.cjs');
const { verifyPackage, inspectWindowsX64, NATIVE_RELATIVE } = require('../lib/verify.cjs');
const { stage } = require('../scripts/stage-native.cjs');

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'roomsync package test '));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, 'package.json'), JSON.stringify({ name: 'roomsync', version: '0.1.3' }));
  const bytes = Buffer.alloc(256); bytes.writeUInt16LE(0x5a4d, 0); bytes.writeUInt32LE(64, 0x3c);
  bytes.writeUInt32LE(0x4550, 64); bytes.writeUInt16LE(0x8664, 68); bytes.writeUInt16LE(0x20b, 88);
  const binary = path.join(root, 'input.exe'); fs.writeFileSync(binary, bytes);
  const notices = path.join(root, 'notices.txt'); fs.writeFileSync(notices, 'Test license notice.\n'.repeat(10));
  const options = { root, binary, notices, sourceCommit: 'a'.repeat(40), expectedSha256: crypto.createHash('sha256').update(bytes).digest('hex') };
  stage(options);
  return { root, options, bytes };
}
function stream() { return { text: '', write(text) { this.text += text; } }; }
function mockProcess() { return new EventEmitter(); }
function childProcess() {
  const child = new EventEmitter(); child.exitCode = null; child.signals = [];
  child.kill = signal => { child.signals.push(signal); return true; };
  return child;
}

test('routes only supported commands and exact option combinations', () => {
  assert.deepEqual(parseArgs(['start']), { kind: 'native', args: ['web'] });
  assert.deepEqual(parseArgs(['start', '--no-browser']), { kind: 'native', args: ['web-headless'] });
  assert.deepEqual(parseArgs(['devices']), { kind: 'native', args: ['list-devices'] });
  for (const args of [[], ['help'], ['--help'], ['start', '--help']]) assert.equal(parseArgs(args).kind, 'help');
  for (const args of [['start', '--bad'], ['start', '--no-browser', '--no-browser'], ['devices', 'extra'], ['--version', 'extra'], ['start;calc'], ['host']]) assert.throws(() => parseArgs(args));
});

test('help and version work without native files on any OS', async t => {
  const { root } = fixture(t); fs.rmSync(path.join(root, 'native'), { recursive: true });
  const stdout = stream();
  assert.equal(await run(['--help'], { root, platform: 'linux', stdout }), 0);
  assert.match(stdout.text, /127\.0\.0\.1/);
  assert.match(stdout.text, /Default capture does not delay or mute original host audio/);
  assert.match(stdout.text, /Windows speakers require explicit virtual routing/);
  assert.equal(await run(['--version'], { root, platform: 'linux', stdout }), 0);
  assert.match(stdout.text, /0\.1\.3/);
});

test('Linux, WSL, macOS and ARM64 fail before launching anything', async t => {
  const { root } = fixture(t);
  for (const [platform, arch] of [['linux', 'x64'], ['darwin', 'arm64'], ['win32', 'arm64'], ['win32', 'ia32']]) {
    const stderr = stream();
    const code = await run(['start'], { root, platform, arch, stderr, spawnImpl() { assert.fail('must not spawn'); } });
    assert.equal(code, 1); assert.match(stderr.text, /requires Windows x64/);
  }
});

test('bad arguments are rejected before spawn', async t => {
  const { root } = fixture(t); const stderr = stream();
  assert.equal(await run(['start', '--port', '1234'], { root, stderr, spawnImpl() { assert.fail(); } }), 2);
});

test('Windows path containing spaces is launched as an argv array without a shell', async t => {
  const { root } = fixture(t); const proc = mockProcess(); const child = childProcess(); let call;
  const result = run(['start', '--no-browser'], { root, platform: 'win32', arch: 'x64', processImpl: proc,
    spawnImpl(...args) { call = args; queueMicrotask(() => { child.exitCode = 7; child.emit('close', 7, null); }); return child; } });
  assert.equal(await result, 7);
  assert.equal(call[0], path.join(root, NATIVE_RELATIVE));
  assert.deepEqual(call[1], ['web-headless']);
  assert.deepEqual(call[2], { shell: false, stdio: 'inherit', windowsHide: false, detached: false });
  assert.equal(proc.listenerCount('SIGINT'), 0); assert.equal(proc.listenerCount('SIGTERM'), 0); assert.equal(proc.listenerCount('exit'), 0);
});

test('Windows Ctrl+C is inherited instead of force-killing graceful native handler', async t => {
  const { root } = fixture(t); const proc = mockProcess(); const child = childProcess();
  const result = run(['start'], { root, platform: 'win32', arch: 'x64', processImpl: proc, spawnImpl: () => child });
  proc.emit('SIGINT'); assert.deepEqual(child.signals, []);
  child.exitCode = 0; child.emit('close', 0, null); assert.equal(await result, 0);
});

test('SIGTERM and parent exit do not leave an orphaned host', async t => {
  const { root } = fixture(t); const proc = mockProcess(); const child = childProcess();
  const result = run(['start'], { root, platform: 'win32', arch: 'x64', processImpl: proc, spawnImpl: () => child });
  proc.emit('SIGTERM'); assert.deepEqual(child.signals, ['SIGTERM']);
  proc.emit('exit'); assert.equal(child.signals.length, 2);
  child.emit('close', null, 'SIGTERM'); assert.equal(await result, 143);
});

test('async spawn error resolves once and removes signal handlers', async t => {
  const { root } = fixture(t); const proc = mockProcess(); const child = childProcess(); const stderr = stream();
  const result = run(['devices'], { root, platform: 'win32', arch: 'x64', processImpl: proc, stderr, spawnImpl: () => child });
  child.emit('error', new Error('permission denied')); child.emit('close', -1, null);
  assert.equal(await result, 1); assert.match(stderr.text, /permission denied/); assert.equal(proc.eventNames().length, 0);
});

test('sync spawn error is reported without leaking handlers', async t => {
  const { root } = fixture(t); const proc = mockProcess(); const stderr = stream();
  assert.equal(await run(['start'], { root, platform: 'win32', arch: 'x64', processImpl: proc, stderr,
    spawnImpl() { throw new Error('cannot spawn'); } }), 1);
  assert.equal(proc.eventNames().length, 0);
});

test('doctor verifies integrity without starting the native process', async t => {
  const { root } = fixture(t); const stdout = stream();
  assert.equal(await run(['doctor'], { root, platform: 'win32', arch: 'x64', stdout, spawnImpl() { assert.fail(); } }), 0);
  assert.match(stdout.text, /checksum verified/); assert.match(stdout.text, /not performed/);
  assert.equal(await run(['doctor'], { root, platform: 'linux', arch: 'x64', stdout }), 1);
});

test('missing executable fails closed with no download fallback', async t => {
  const { root } = fixture(t); fs.unlinkSync(path.join(root, NATIVE_RELATIVE));
  const stderr = stream();
  assert.equal(await run(['start'], { root, platform: 'win32', arch: 'x64', stderr, spawnImpl() { assert.fail(); } }), 1);
  assert.match(stderr.text, /no executable will be downloaded/);
});

test('modified binary is refused before spawn', async t => {
  const { root } = fixture(t); const file = path.join(root, NATIVE_RELATIVE);
  const bytes = fs.readFileSync(file); bytes[200] ^= 1; fs.writeFileSync(file, bytes);
  const stderr = stream();
  assert.equal(await run(['start'], { root, platform: 'win32', arch: 'x64', stderr, spawnImpl() { assert.fail(); } }), 1);
  assert.match(stderr.text, /checksum mismatch/);
});

test('manifest version, path, schema, commit and size must match', t => {
  const { root } = fixture(t); const file = path.join(root, 'native/manifest.json');
  const original = JSON.parse(fs.readFileSync(file));
  for (const mutation of [{ version: '0.0.1' }, { binary: '../../evil.exe' }, { schemaVersion: 2 }, { sourceCommit: 'latest' }, { sizeBytes: 1 }, { sha256: 'z'.repeat(64) }, { platform: 'linux' }]) {
    fs.writeFileSync(file, JSON.stringify({ ...original, ...mutation }));
    assert.throws(() => verifyPackage(root), /manifest/);
  }
});

test('PE inspection rejects arbitrary files, x86 and bad offsets', () => {
  assert.throws(() => inspectWindowsX64(Buffer.alloc(10)), /Windows executable/);
  const bytes = Buffer.alloc(256); bytes.writeUInt16LE(0x5a4d); bytes.writeUInt32LE(64, 0x3c); bytes.writeUInt32LE(0x4550, 64); bytes.writeUInt16LE(0x14c, 68);
  assert.throws(() => inspectWindowsX64(bytes), /x64/);
  bytes.writeUInt32LE(0xffffffff, 0x3c); assert.throws(() => inspectWindowsX64(bytes), /x64/);
});

test('staging validates binary digest and source commit before changing package', t => {
  const { root, options } = fixture(t); const before = fs.readFileSync(path.join(root, 'native/manifest.json'));
  assert.throws(() => stage({ ...options, expectedSha256: '0'.repeat(64) }), /does not match/);
  assert.throws(() => stage({ ...options, sourceCommit: 'main' }), /exact 40/);
  assert.deepEqual(fs.readFileSync(path.join(root, 'native/manifest.json')), before);
});

test('third-party notices are required', t => {
  const { root } = fixture(t); fs.writeFileSync(path.join(root, 'THIRD_PARTY_NOTICES.txt'), '');
  assert.throws(() => verifyPackage(root), /license notices/);
});

test('package has no dependencies or install hooks and publication is disabled', () => {
  const pkg = require('../package.json');
  assert.equal(pkg.private, true);
  for (const field of ['dependencies', 'optionalDependencies', 'devDependencies']) assert.equal(pkg[field], undefined);
  for (const hook of ['preinstall', 'install', 'postinstall', 'prepare']) assert.equal(pkg.scripts[hook], undefined);
  assert.equal(pkg.bin.roomsync, 'bin/roomsync.cjs');
  assert.deepEqual(pkg.files, ['bin/', 'lib/', 'native/', 'README.md', 'THIRD_PARTY_NOTICES.txt']);
});
