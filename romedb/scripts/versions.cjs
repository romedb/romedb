'use strict';
// Release version comparisons shared by the upgrade/restore checks, so a new
// patch or minor candidate needs no edit to scripts that accept any later 0.x.
const assert = require('node:assert/strict');

function parse(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
  assert.ok(match, `not a release version: ${version}`);
  return match.slice(1).map(Number);
}

function compare(a, b) {
  const [x, y] = [parse(a), parse(b)];
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return 0;
}

/** Asserts that a candidate is a RomeDB 0.x release at or after `first`. */
function assertCandidate(version, first) {
  assert.equal(parse(version)[0], 0, `candidate must be a RomeDB 0.x release: ${version}`);
  assert.ok(compare(version, first) >= 0, `candidate ${version} is older than ${first}`);
}

module.exports = { parse, compare, assertCandidate };
