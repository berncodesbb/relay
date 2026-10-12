'use strict';

const assert = require('node:assert');
const { greet } = require('../src/greet.js');

assert.strictEqual(greet('World'), 'Hello, World!');
console.log('all checks passed');
