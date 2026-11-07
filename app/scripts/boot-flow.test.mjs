
// Pure source-function harness. No transpilation, native app, ADB, network or real timers.
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { createContext, runInContext } from 'node:vm';

const source = readFileSync(new URL('../src/main.ts', import.meta.url), 'utf8');
const startAt = source.indexOf('async function startVm() {');
const stopAt = source.indexOf('async function stopVm() {', startAt);
const endAt = source.indexOf('// ------------------------------------------------------------------ 启动', stopAt);
assert(startAt >= 0 && stopAt > startAt && endAt > stopAt, 'Source function anchors changed; review harness');
const functions = source.slice(startAt, endAt);
function deferred() {
  let resolve;
  const promise = new Promise(yes => { resolve = yes; });
  return { promise, resolve };
}
function harness(overrides = {}) {
  const vm = { id: 'vm-a', name: 'fixture VM' };
  const calls = [], logs = [];
  const state = { vms: [vm], currentVmId: vm.id, headless: true, running: new Set(), starting: new Set(), bootSessions: new Map() };
  let now = 0;
  const defaults = {
    launchVm: async () => ({ ok: true, pid: 123, backend: 'fixture', serial: 'emulator-5554' }),
    listVms: async () => [vm], runningVms: async () => [vm.id], adbConnect: async () => true,
    vmBootStatus: async () => ({ boot_completed: true, serial: 'emulator-5554' }),
    adbDeviceInfo: async () => ({ connected: true, serial: 'emulator-5554', model: 'fixture', sdk: '30', abi: 'x86_64', root: false, root_checked: false }),
    stopVm: async () => 'fixture stopped',
    verifyVmRuntime: async () => { throw new Error('automatic privileged verification'); },
    checkRoot: async () => { throw new Error('automatic su'); },
    requestVmRoot: async () => { throw new Error('automatic root request'); },
  };
  const api = Object.fromEntries(Object.entries({ ...defaults, ...overrides }).map(([name, fn]) => [name, (...args) => { calls.push({ name, args }); return fn(...args); }]));
  const context = createContext({ state, api, Symbol, Set, Date: { now: () => now += 1000 },
    setTimeout: callback => { callback(); return 0; },
    log: (text, level) => logs.push({ text, level }), paintView: () => {} });
  const actions = runInContext(functions + '\n({ startVm, stopVm });', context);
  return { state, calls, logs, ...actions };
}
const privileged = h => h.calls.filter(call => ['verifyVmRuntime', 'checkRoot', 'requestVmRoot'].includes(call.name));

test('boot completion uses exact serial and ordinary properties, never automatic su', async () => {
  const h = harness(); await h.startVm();
  assert.equal(privileged(h).length, 0);
  assert(h.logs.some(entry => entry.text.startsWith('Android 启动核验完成')));
  assert.equal(h.state.starting.size, 0); assert.equal(h.state.bootSessions.size, 0);
});
test('ADB connection alone is not boot completion', async () => {
  let boots = 0;
  const h = harness({ vmBootStatus: async () => ({ boot_completed: ++boots > 1, serial: 'emulator-5554' }) });
  await h.startVm();
  assert.equal(boots, 2);
  assert.equal(h.calls.filter(call => call.name === 'adbDeviceInfo').length, 1);
  assert.equal(privileged(h).length, 0);
});
test('authorization lane contention is retried without declaring launch failure', async () => {
  let connections = 0;
  const h = harness({ adbConnect: async () => {
    if (++connections === 1) throw new Error('device lane busy while explicit Root waits');
    return true;
  } });
  await h.startVm();
  assert.equal(connections, 2);
  assert(h.logs.some(entry => entry.text.startsWith('Android 启动核验完成')));
  assert(!h.logs.some(entry => entry.text.startsWith('启动失败')));
  assert.equal(privileged(h).length, 0);
});
test('wrong serial never reaches device-info or successful boot logging', async () => {
  const h = harness({ vmBootStatus: async () => ({ boot_completed: true, serial: 'different-fixture' }) });
  await h.startVm();
  assert.equal(h.calls.filter(call => call.name === 'adbDeviceInfo').length, 0);
  assert(!h.logs.some(entry => entry.text.startsWith('Android 启动核验完成')));
  assert(h.logs.some(entry => entry.text.startsWith('Android 启动核验超时')));
});
test('disconnected property response is not reported as successful boot', async () => {
  const h = harness({ adbDeviceInfo: async () => ({ connected: false, serial: 'emulator-5554' }) });
  await h.startVm();
  assert(!h.logs.some(entry => entry.text.startsWith('Android 启动核验完成')));
  assert.equal(privileged(h).length, 0);
});
test('old completion never clears a newer start session', async () => {
  const waiting = deferred(), entered = deferred();
  const h = harness({ adbConnect: async () => { entered.resolve(); return waiting.promise; } });
  const first = h.startVm(); await entered.promise;
  const replacement = Symbol('new launch');
  h.state.bootSessions.set('vm-a', replacement); h.state.starting.add('vm-a');
  waiting.resolve(true); await first;
  assert.equal(h.state.bootSessions.get('vm-a'), replacement);
  assert(h.state.starting.has('vm-a'));
  assert.equal(h.calls.filter(call => call.name === 'vmBootStatus').length, 0);
});
test('explicit stop cancels old boot polling and releases starting marker', async () => {
  const waiting = deferred(), entered = deferred();
  const h = harness({ adbConnect: async () => { entered.resolve(); return waiting.promise; } });
  const first = h.startVm(); await entered.promise;
  await h.stopVm(); waiting.resolve(true); await first;
  assert.equal(h.state.starting.size, 0); assert.equal(h.state.bootSessions.size, 0);
  assert.equal(h.state.running.size, 0);
  assert.equal(h.calls.filter(call => call.name === 'vmBootStatus').length, 0);
});
test('cancelled launch-list response does not overwrite a replacement VM list', async () => {
  const waiting = deferred(), entered = deferred();
  const h = harness({ listVms: async () => { entered.resolve(); return waiting.promise; } });
  const first = h.startVm(); await entered.promise;
  const replacement = [{ id: 'new-fixture', name: 'new' }];
  h.state.bootSessions.delete('vm-a'); h.state.starting.delete('vm-a'); h.state.vms = replacement;
  waiting.resolve([{ id: 'stale-fixture' }]); await first;
  assert.equal(h.state.vms, replacement);
});