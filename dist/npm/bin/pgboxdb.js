#!/usr/bin/env node
'use strict';
require('../lib/pgboxdb.js')
  .main()
  .then((code) => {
    process.exitCode = code;
  })
  .catch((e) => {
    process.stderr.write(`pgboxdb: ${e && e.stack ? e.stack : e}\n`);
    process.exitCode = 1;
  });
