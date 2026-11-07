
import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequestGate } from '../src/guest-request-gate.mjs';

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test('observing an empty gate performs no request', () => {
  const gate = createRequestGate();
  assert.equal(gate.get('vm-a'), undefined);
  assert.equal(gate.has('vm-a'), false);
});
test('same VM coalesces pending requests and survives a new observer', async () => {
  const gate = createRequestGate(), work = deferred();
  let calls = 0;
  const first = gate.start('vm-a', () => { calls++; return work.promise; });
  assert.equal(gate.get('vm-a'), first);
  const second = gate.start('vm-a', () => { calls++; throw new Error('duplicate factory ran'); });
  assert.equal(second, first);
  assert.equal(calls, 1);
  work.resolve({ ok: true });
  assert.deepEqual(await second, { ok: true });
  assert.equal(gate.has('vm-a'), false);
});
test('different VMs progress independently and retain exact identities', async () => {
  const gate = createRequestGate(), a = deferred(), b = deferred();
  const first = gate.start('vm-a', () => a.promise);
  const second = gate.start('vm-b', () => b.promise);
  assert.notEqual(first, second);
  a.resolve('a'); await first;
  assert.equal(gate.has('vm-a'), false);
  assert.equal(gate.get('vm-b'), second);
  b.resolve('b'); assert.equal(await second, 'b');
  assert.equal(gate.has('vm-b'), false);
});
test('a rejected request is released without an automatic retry', async () => {
  const gate = createRequestGate(), work = deferred();
  let calls = 0;
  const request = gate.start('vm-a', () => { calls++; return work.promise; });
  const rejected = assert.rejects(request, /explicit timeout/);
  work.reject(new Error('explicit timeout'));
  await rejected;
  assert.equal(calls, 1);
  assert.equal(gate.has('vm-a'), false);
  const retry = gate.start('vm-a', () => { calls++; return Promise.resolve('manual retry'); });
  assert.equal(await retry, 'manual retry');
  assert.equal(calls, 2);
});
test('a synchronous factory failure leaves no stale pending entry', () => {
  const gate = createRequestGate();
  assert.throws(() => gate.start('vm-a', () => { throw new Error('startup failed'); }), /startup failed/);
  assert.equal(gate.has('vm-a'), false);
});
test('settled values are not cached as current authorization proof', async () => {
  const gate = createRequestGate();
  const first = gate.start('vm-a', () => Promise.resolve('first boot'));
  await first;
  assert.equal(gate.get('vm-a'), undefined);
  const second = gate.start('vm-a', () => Promise.resolve('next explicit request'));
  assert.notEqual(first, second);
  assert.equal(await second, 'next explicit request');
});