'use strict';
// Isolated test process: replace only the Worker transport. Native query tests
// remain in test.cjs; this fixture does not claim native crash recovery.
const assert = require('node:assert/strict');
const { EventEmitter } = require('node:events');
const threads = require('node:worker_threads');
let latest;
let startupFailure;
class FaultWorker extends EventEmitter {
  messages = []; stopped = false; rejectClose = false; rejectRequest = false; exitBeforeCloseAck = false;
  constructor() {
    super(); latest = this;
    const failure = startupFailure;
    queueMicrotask(() => {
      if (failure === 'exit') { this.stopped = true; this.emit('exit', 7); }
      else if (failure === 'error') {
        this.emit('error', new Error('startup worker error'));
        this.stopped = true; this.emit('exit', 1);
      } else if (failure === 'messageerror') {
        this.emit('messageerror', new Error('startup response decoding error'));
      } else this.emit('message', { ready: true, interruptKey: '0' });
    });
  }
  postMessage(message) {
    this.messages.push(message);
    if (this.rejectRequest && message.method !== 'close') throw new Error('injected send failure');
    if (message.method === 'close') {
      if (this.rejectClose) throw new Error('transport closed');
      queueMicrotask(() => {
        if (this.exitBeforeCloseAck) { this.stopped = true; this.emit('exit', 7); return; }
        this.emit('message', { id: message.id });
        this.stopped = true; this.emit('exit', 0);
      });
    }
  }
  async terminate() { this.stopped = true; this.emit('exit', 1); return 1; }
}
threads.Worker = FaultWorker;
const { AsyncDatabase, isRomeDBError } = require('./index.cjs');
const { cancelOperation } = require('./romedb.node');
const { getEventListeners } = require('node:events');
(async () => {
  for (const failure of ['error','exit','messageerror']) {
    startupFailure = failure;
    await assert.rejects(AsyncDatabase.open(), error => error.code === 'FDB_WORKER');
    assert.equal(latest.stopped,true);
    assert.equal(latest.messages.filter(message => message.method === 'close').length, failure === 'messageerror' ? 1 : 0);
  }
  startupFailure = undefined;

  for (const brokenSend of [false, true]) {
    const db = await AsyncDatabase.open();
    const worker = latest; worker.rejectClose = brokenSend;
    const controller = new AbortController();
    const first = db.execute('INSERT INTO example VALUES (1)', {}, {signal:controller.signal});
    const token = worker.messages[0].cancellationKey;
    const second = db.execute('SELECT 1');
    const settled = Promise.allSettled([first, second]);
    const cause = new Error('injected message decoding failure');
    worker.emit('messageerror', cause);
    const outcomes = await settled;
    assert(outcomes.every(r => r.status === 'rejected' && isRomeDBError(r.reason) && r.reason.code === 'FDB_WORKER'));
    assert.equal(outcomes[0].reason, outcomes[1].reason);
    assert.equal(outcomes[0].reason.cause, cause);
    assert.equal(cancelOperation(token),false);
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    const close = db.close(); assert.equal(db.close(), close); await close;
    assert.equal(worker.stopped, true);
    assert.equal(worker.messages.filter(m => m.method === 'close').length, 1);
    await assert.rejects(db.execute('SELECT 2'), error => error === outcomes[0].reason);
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    const pending = db.execute('SELECT 1');
    const rejected = assert.rejects(pending, error => error.code === 'FDB_WORKER' && error.message.includes('original worker error'));
    worker.emit('error', new Error('original worker error'));
    worker.stopped = true; worker.emit('exit', 1);
    await rejected; await db.close();
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    const pending = db.execute('SELECT 1');
    const rejected = assert.rejects(pending, error => error.code === 'FDB_WORKER');
    worker.stopped = true; worker.emit('exit', 7);
    await rejected; await db.close();
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    const close = db.close();
    const rejected = assert.rejects(close, error => error.code === 'FDB_WORKER');
    worker.emit('messageerror', new Error('lost close acknowledgement'));
    await rejected;
    assert.equal(worker.stopped, true);
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    worker.rejectRequest = true;
    const controller = new AbortController();
    await assert.rejects(db.execute('SELECT 1', {}, {signal:controller.signal}), /injected send failure/);
    assert.equal(cancelOperation(worker.messages[0].cancellationKey), false);
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    await db.close();
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    const controllers = Array.from({length:256},()=>new AbortController());
    const accepted = controllers.map(controller=>db.execute('SELECT 1',{}, {signal:controller.signal}));
    const settled = Promise.allSettled(accepted);
    const excess = new AbortController();
    await assert.rejects(db.execute('SELECT 2',{}, {signal:excess.signal}), e=>isRomeDBError(e) && e.code==='FDB_LIMIT' && e.transaction===undefined);
    assert.equal(getEventListeners(excess.signal,'abort').length,0);
    assert.equal(worker.messages.length,256);
    controllers.forEach(controller=>controller.abort());
    // Aborted queued work still owns its queue slot until a response/failure.
    await assert.rejects(db.execute('SELECT 3'), e=>isRomeDBError(e) && e.code==='FDB_LIMIT' && e.transaction===undefined);
    const tokens = worker.messages.map(message=>message.cancellationKey);
    worker.emit('messageerror',new Error('queue response channel failed'));
    const outcomes = await settled;
    assert(outcomes.every(outcome=>outcome.status==='rejected' && outcome.reason.code==='FDB_WORKER'));
    for (let i=0;i<tokens.length;i++) {
      assert.equal(cancelOperation(tokens[i]),false);
      assert.equal(getEventListeners(controllers[i].signal,'abort').length,0);
    }
    await db.close();
    assert.equal(worker.stopped,true);
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    // Reuse one string: the mock does not clone payloads. Count UTF-8 bytes,
    // not JS code units, while avoiding 128 MiB of distinct test allocations.
    const script = 'é'.repeat(4 * 1024 * 1024);
    assert.equal(Buffer.byteLength(script), 8 * 1024 * 1024);
    const controllers = Array.from({length:16}, () => new AbortController());
    const accepted = controllers.map(controller => db.executeBatch(script, {signal:controller.signal}));
    const settled = Promise.allSettled(accepted);
    assert.equal(worker.messages.length,16);
    const excess = new AbortController();
    await assert.rejects(db.executeBatch('x', {signal:excess.signal}), error => isRomeDBError(error) && error.code === 'FDB_LIMIT' && error.transaction === undefined);
    assert.equal(getEventListeners(excess.signal,'abort').length,0);
    controllers[0].abort();
    await assert.rejects(db.executeBatch('x'), error => error.code === 'FDB_LIMIT');
    assert.equal(worker.messages.length,16);
    const first = worker.messages[0];
    worker.emit('message', {id:first.id, error:{message:'completed request failure'}});
    assert.equal(cancelOperation(first.cancellationKey),false);
    assert.equal(getEventListeners(controllers[0].signal,'abort').length,0);
    // A failed send must also return its reserved bytes and token.
    worker.rejectRequest = true;
    const sendController = new AbortController();
    await assert.rejects(db.executeBatch(script, {signal:sendController.signal}), /injected send failure/);
    const failedSend = worker.messages.pop();
    assert.equal(cancelOperation(failedSend.cancellationKey),false);
    assert.equal(getEventListeners(sendController.signal,'abort').length,0);
    worker.rejectRequest = false;
    // A completed error frees exactly its accepted bytes for subsequent work.
    const replacementController = new AbortController();
    const replacement = db.executeBatch(script, {signal:replacementController.signal});
    const replacementSettled = Promise.allSettled([replacement]);
    assert.equal(worker.messages.length,17);
    await assert.rejects(db.executeBatch('x'), error => error.code === 'FDB_LIMIT');
    worker.emit('messageerror',new Error('byte-limited queue response failure'));
    const outcomes = await settled;
    assert.equal(outcomes[0].reason.message,'completed request failure');
    assert(outcomes.slice(1).every(outcome => outcome.status === 'rejected' && outcome.reason.code === 'FDB_WORKER'));
    assert.equal((await replacementSettled)[0].reason.code,'FDB_WORKER');
    for (let i=0;i<controllers.length;i++) {
      assert.equal(getEventListeners(controllers[i].signal,'abort').length,0);
      assert.equal(cancelOperation(worker.messages[i].cancellationKey),false);
    }
    assert.equal(getEventListeners(replacementController.signal,'abort').length,0);
    assert.equal(cancelOperation(worker.messages[16].cancellationKey),false);
    await db.close();
    assert.equal(worker.stopped,true);
  }
  for (const failure of ['send','exit']) {
    const db = await AsyncDatabase.open(); const worker = latest;
    const controller = new AbortController();
    const operation = db.execute('SELECT 1', {}, {signal:controller.signal});
    const token = worker.messages[0].cancellationKey;
    worker.rejectClose = failure === 'send';
    worker.exitBeforeCloseAck = failure === 'exit';
    const close = db.close();
    assert.equal(db.close(), close);
    const outcomes = await Promise.allSettled([operation, close]);
    assert(outcomes.every(outcome => outcome.status === 'rejected' && outcome.reason.code === 'FDB_WORKER'));
    assert.equal(outcomes[0].reason, outcomes[1].reason);
    assert.equal(worker.stopped, true);
    assert.equal(cancelOperation(token), false);
    assert.equal(getEventListeners(controller.signal,'abort').length, 0);
    assert.equal(db.close(), close);
    await assert.rejects(db.execute('SELECT 2'), error => error === outcomes[0].reason);
  }
  {
    const db = await AsyncDatabase.open(); const worker = latest;
    const close = db.close();
    const controller = new AbortController();
    const before = worker.messages.length;
    await assert.rejects(db.execute('SELECT 1', {}, {signal:controller.signal}), error => error.code === 'FDB_CLOSED' && !Object.hasOwn(error,'transaction'));
    assert.equal(worker.messages.length,before);
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    controller.abort();
    await close;
    assert.equal(worker.stopped,true);
    await assert.rejects(db.profileSelect('SELECT 1', {}, {signal:controller.signal}), error => error.code === 'FDB_CLOSED');
    assert.equal(getEventListeners(controller.signal,'abort').length,0);
    assert.equal(worker.messages.length,before);
  }
  for (const signalEnabled of [false,true]) {
    for (const failure of ['send','exit','error','messageerror']) {
      const db = await AsyncDatabase.open();
      const worker = latest;
      const controller = new AbortController();
      const options = {timeoutMs:60000, ...(signalEnabled ? {signal:controller.signal} : {})};
      worker.rejectRequest = failure === 'send';
      const operation = db.execute('DELETE FROM docs',{},options);
      const settled = Promise.allSettled([operation]);
      const token = worker.messages[0].cancellationKey;
      assert.equal(typeof token,'string');
      if (failure === 'exit') { worker.stopped=true; worker.emit('exit',7); }
      if (failure === 'error') {
        worker.emit('error',new Error('timed worker failure'));
        worker.stopped=true; worker.emit('exit',1);
      }
      if (failure === 'messageerror') worker.emit('messageerror',new Error('timed response failure'));
      const [outcome] = await settled;
      assert.equal(outcome.status,'rejected');
      if (failure === 'send') assert.match(outcome.reason.message,/injected send failure/);
      else assert.equal(outcome.reason.code,'FDB_WORKER');
      assert.equal(outcome.reason.transaction,undefined);
      assert.equal(cancelOperation(token),false);
      assert.equal(getEventListeners(controller.signal,'abort').length,0);
      controller.abort();
      await db.close();
      assert.equal(worker.stopped,true);
    }
  }
  process.stdout.write('worker-faults-complete\n');
})().catch(error => { console.error(error); process.exitCode = 1; });
