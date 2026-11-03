'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const NATIVE_RELATIVE = 'native/win32-x64/roomsync-desktop.exe';
const MAX_BINARY_BYTES = 100 * 1024 * 1024;

function readRegularFile(file, maximum) {
  const stat = fs.lstatSync(file);
  if (!stat.isFile() || stat.size > maximum) throw new Error(`Invalid package file: ${path.basename(file)}`);
  return fs.readFileSync(file);
}

function inspectWindowsX64(bytes) {
  if (bytes.length < 64 || bytes.readUInt16LE(0) !== 0x5a4d) throw new Error('Native binary is not a Windows executable.');
  const offset = bytes.readUInt32LE(0x3c);
  if (offset > bytes.length - 26 || bytes.readUInt32LE(offset) !== 0x00004550 ||
      bytes.readUInt16LE(offset + 4) !== 0x8664 || bytes.readUInt16LE(offset + 24) !== 0x20b) {
    throw new Error('Native binary must be a Windows x64 PE32+ executable.');
  }
}

function verifyPackage(root) {
  let pkg, manifest, bytes;
  try {
    pkg = JSON.parse(readRegularFile(path.join(root, 'package.json'), 64 * 1024));
    manifest = JSON.parse(readRegularFile(path.join(root, 'native/manifest.json'), 64 * 1024));
    bytes = readRegularFile(path.join(root, NATIVE_RELATIVE), MAX_BINARY_BYTES);
  } catch (error) {
    if (error.code === 'ENOENT') throw new Error('This package is missing its bundled Windows host. Reinstall a complete RoomSync package; no executable will be downloaded automatically.');
    throw error;
  }
  if (manifest.schemaVersion !== 1 || manifest.platform !== 'win32' || manifest.arch !== 'x64' ||
      manifest.binary !== NATIVE_RELATIVE || manifest.version !== pkg.version ||
      typeof manifest.sourceCommit !== 'string' || !/^[a-f0-9]{40}$/.test(manifest.sourceCommit) ||
      typeof manifest.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(manifest.sha256) ||
      !Number.isSafeInteger(manifest.sizeBytes) || manifest.sizeBytes !== bytes.length) {
    throw new Error('Invalid or mismatched native package manifest. Reinstall a complete RoomSync package.');
  }
  inspectWindowsX64(bytes);
  if (crypto.createHash('sha256').update(bytes).digest('hex') !== manifest.sha256) {
    throw new Error('Bundled Windows host checksum mismatch. Refusing to run it; reinstall the package.');
  }
  const notices = readRegularFile(path.join(root, 'THIRD_PARTY_NOTICES.txt'), 10 * 1024 * 1024);
  if (notices.length < 100) throw new Error('Third-party license notices are missing.');
  return { executable: path.join(root, NATIVE_RELATIVE), manifest, package: pkg };
}

module.exports = { NATIVE_RELATIVE, MAX_BINARY_BYTES, inspectWindowsX64, verifyPackage };
if (require.main === module) {
  try {
    const result = verifyPackage(path.resolve(__dirname, '..'));
    console.error(`Verified RoomSync ${result.package.version}: Windows x64, SHA256 ${result.manifest.sha256}`);
  } catch (error) { console.error(`RoomSync package: ${error.message}`); process.exitCode = 1; }
}
