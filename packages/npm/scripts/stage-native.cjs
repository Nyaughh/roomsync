#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { inspectWindowsX64, NATIVE_RELATIVE, verifyPackage, MAX_BINARY_BYTES } = require('../lib/verify.cjs');

function stage({ binary, notices, sourceCommit, expectedSha256, root = path.resolve(__dirname, '..') }) {
  if (!/^[a-f0-9]{40}$/.test(sourceCommit || '')) throw new Error('--source-commit must be the exact 40-character source commit.');
  if (!/^[a-f0-9]{64}$/.test(expectedSha256 || '')) throw new Error('--expected-sha256 must be a verified 64-character binary digest.');
  const stat = fs.lstatSync(binary);
  if (!stat.isFile() || stat.size > MAX_BINARY_BYTES) throw new Error('Binary must be a regular file smaller than 100 MiB.');
  const bytes = fs.readFileSync(binary);
  inspectWindowsX64(bytes);
  const sha256 = crypto.createHash('sha256').update(bytes).digest('hex');
  if (sha256 !== expectedSha256) throw new Error('Binary does not match --expected-sha256; no files staged.');
  const noticeBytes = fs.readFileSync(notices);
  if (noticeBytes.length < 100 || noticeBytes.length > 10 * 1024 * 1024) throw new Error('Valid third-party notices are required.');
  const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  const manifest = { schemaVersion: 1, version: pkg.version, platform: 'win32', arch: 'x64', binary: NATIVE_RELATIVE, sourceCommit, sha256, sizeBytes: bytes.length };
  fs.mkdirSync(path.dirname(path.join(root, NATIVE_RELATIVE)), { recursive: true });
  fs.writeFileSync(path.join(root, NATIVE_RELATIVE), bytes);
  fs.writeFileSync(path.join(root, 'THIRD_PARTY_NOTICES.txt'), noticeBytes);
  // Write manifest last: an interrupted stage never validates a mixed binary.
  fs.writeFileSync(path.join(root, 'native/manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  return verifyPackage(root);
}
module.exports = { stage };
if (require.main === module) {
  try {
    const args = process.argv.slice(2); const values = {};
    for (let i = 0; i < args.length; i += 2) {
      if (!['--binary', '--notices', '--source-commit', '--expected-sha256'].includes(args[i]) || !args[i + 1] || values[args[i]]) throw new Error('Use --binary FILE --notices FILE --source-commit SHA --expected-sha256 DIGEST.');
      values[args[i]] = args[i + 1];
    }
    if (Object.keys(values).length !== 4) throw new Error('All four staging options are required.');
    const result = stage({ binary: values['--binary'], notices: values['--notices'], sourceCommit: values['--source-commit'], expectedSha256: values['--expected-sha256'] });
    console.log(`Staged ${result.manifest.sizeBytes} bytes for RoomSync ${result.package.version}; checksum ${result.manifest.sha256}`);
  } catch (error) { console.error(`RoomSync staging: ${error.message}`); process.exitCode = 1; }
}
