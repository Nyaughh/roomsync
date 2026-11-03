#!/usr/bin/env node
'use strict';
const { run } = require('../lib/cli.cjs');
run(process.argv.slice(2)).then(
  code => { process.exitCode = code; },
  error => { console.error(`RoomSync: ${error.message}`); process.exitCode = 1; }
);
