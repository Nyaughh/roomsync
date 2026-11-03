'use strict';
const path = require('node:path');
const fs = require('node:fs');
const { spawn } = require('node:child_process');
const { verifyPackage } = require('./verify.cjs');
const ROOT = path.resolve(__dirname, '..');

function parseArgs(args) {
  const [command, ...rest] = args;
  if (!command || ['help', '--help', '-h'].includes(command)) {
    if (rest.length) throw new Error('Unexpected help arguments. Run roomsync --help.');
    return { kind: 'help' };
  }
  if (['--version', '-v'].includes(command) && rest.length === 0) return { kind: 'version' };
  if (command === 'doctor' && rest.length === 0) return { kind: 'doctor' };
  if (command === 'devices' && rest.length === 0) return { kind: 'native', args: ['list-devices'] };
  if (command === 'start') {
    if (rest.length === 0) return { kind: 'native', args: ['web'] };
    if (rest.length === 1 && rest[0] === '--no-browser') return { kind: 'native', args: ['web-headless'] };
    if (rest.length === 1 && ['--help', '-h'].includes(rest[0])) return { kind: 'help' };
    throw new Error('Use roomsync start or roomsync start --no-browser. Other options are not supported.');
  }
  throw new Error(`Unknown command or extra arguments. Run roomsync --help.`);
}

function help(version) {
  return `RoomSync ${version} · Windows host preview\n\n` +
    '  roomsync start               Open the local setup/control page\n' +
    '  roomsync start --no-browser  Print its address without opening a browser\n' +
    '  roomsync devices             List Windows output endpoints\n' +
    '  roomsync doctor              Check platform and bundled-host integrity\n' +
    '  roomsync --version\n\n' +
    'Requires Windows 10 1703+ x64 and Node.js 20+. macOS, Linux, WSL and native\n' +
    'Windows ARM64 are not supported by this package. Run from Windows PowerShell\n' +
    'or Command Prompt, not WSL. Keep the terminal open; Ctrl+C stops the host.\n\n' +
    'Start opens controls on 127.0.0.1 only. Audio sharing starts only after you\n' +
    'create a private setup code and choose Start listening in the control page.\n' +
    'Default capture does not delay or mute original host audio. Optional synced\n' +
    'Windows speakers require explicit virtual routing. This unsigned preview\n' +
    'still needs physical audio/synchronization validation. Timing diagnostics\n' +
    'do not prove the physical cause of silence or cutouts.\n';
}

function runNative(executable, args, options) {
  const { spawnImpl, processImpl, platform, stderr } = options;
  return new Promise(resolve => {
    let child;
    let settled = false;
    const onInterrupt = () => {
      // Windows delivers console Ctrl+C to the inherited native console process.
      // Node child.kill('SIGINT') force-terminates Windows processes, so do not use it.
      if (platform !== 'win32') child.kill('SIGINT');
    };
    const onTerminate = () => { child.kill('SIGTERM'); };
    const onExit = () => { if (!settled && child.exitCode === null) child.kill(); };
    const finish = code => {
      if (settled) return;
      settled = true;
      processImpl.removeListener('SIGINT', onInterrupt);
      processImpl.removeListener('SIGTERM', onTerminate);
      processImpl.removeListener('exit', onExit);
      resolve(code);
    };
    try {
      child = spawnImpl(executable, args, {
        shell: false, stdio: 'inherit', windowsHide: false, detached: false
      });
    } catch (error) {
      stderr.write(`RoomSync could not start: ${error.message}\n`);
      finish(1); return;
    }
    processImpl.on('SIGINT', onInterrupt);
    processImpl.on('SIGTERM', onTerminate);
    processImpl.on('exit', onExit);
    child.once('error', error => {
      stderr.write(`RoomSync could not start its bundled host: ${error.message}\n`);
      finish(1);
    });
    child.once('close', (code, signal) => {
      finish(Number.isInteger(code) ? code : signal === 'SIGINT' ? 130 : signal === 'SIGTERM' ? 143 : 1);
    });
  });
}

async function run(args, options = {}) {
  const {
    root = ROOT, platform = process.platform, arch = process.arch,
    stdout = process.stdout, stderr = process.stderr,
    spawnImpl = spawn, processImpl = process
  } = options;
  let parsed;
  try { parsed = parseArgs(args); }
  catch (error) { stderr.write(`RoomSync: ${error.message}\n`); return 2; }
  const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  if (parsed.kind === 'help') { stdout.write(help(pkg.version)); return 0; }
  if (parsed.kind === 'version') { stdout.write(`${pkg.version}\n`); return 0; }
  const supported = platform === 'win32' && arch === 'x64';
  if (!supported && parsed.kind !== 'doctor') {
    stderr.write(`RoomSync's bundled host requires Windows x64 (detected ${platform}/${arch}). Use Windows PowerShell or Command Prompt with x64 Node.js, not WSL. No host was started.\n`);
    return 1;
  }
  let verified;
  try { verified = verifyPackage(root); }
  catch (error) { stderr.write(`RoomSync: ${error.message}\n`); return 1; }
  if (parsed.kind === 'doctor') {
    stdout.write(`RoomSync ${pkg.version}\nPlatform: ${platform}/${arch} (${supported ? 'supported' : 'unsupported'})\nBundled Windows x64 host: checksum verified\nBundle manifest version: ${verified.manifest.version}\nSource commit: ${verified.manifest.sourceCommit}\nAudio hardware/runtime validation: not performed by this check\n`);
    return supported ? 0 : 1;
  }
  return runNative(verified.executable, parsed.args, { spawnImpl, processImpl, platform, stderr });
}
module.exports = { parseArgs, help, run, runNative };
