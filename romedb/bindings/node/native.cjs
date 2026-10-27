'use strict';
const minimumNodeMajor = 22;
const nodeMajor = Number.parseInt(process.versions.node.split('.')[0], 10);
if (nodeMajor < minimumNodeMajor) {
  const error = new Error(`RomeDB requires Node.js ${minimumNodeMajor} or newer; this process is running Node ${process.versions.node}.`);
  error.code = 'FDB_RUNTIME_VERSION';
  throw error;
}
try {
  module.exports = require('./romedb.node');
} catch (cause) {
  const error = new Error(
    `RomeDB could not load its native addon for ${process.platform}/${process.arch} on Node ${process.versions.node}. ` +
    'Use an addon built for this platform; for a source checkout, run romedb/scripts/check-node.sh from the repository root. ' +
    'See the package README for supported build and installation instructions.',
    { cause },
  );
  error.code = 'FDB_NATIVE_LOAD';
  throw error;
}
