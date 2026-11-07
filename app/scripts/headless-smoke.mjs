
// Execute the already-built Windows binary only on a GitHub-hosted runner.
import { spawn } from 'node:child_process';
import { mkdir, copyFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';

const selfTest = process.argv.includes('--self-test');
const evidence = resolve(process.env.GITHUB_WORKSPACE || '.', 'build-evidence');
const vmId = 'headless-cloud-fixture';
const config = { id: vmId, name: 'Headless protocol fixture', backend: 'android-emulator',
  memory_mb: 512, cores: 1, console_port: 5554, adb_port: 5555,
  root_manager: 'none', lsp_version: 'none', pending_restart: false };
const cases = [
  { name: 'status-and-shutdown', input: '{"id":1,"op":"status"}\n{"id":2,"op":"shutdown"}\n', exit: 0 },
  { name: 'clean-eof', input: '', exit: 0 },
  { name: 'stop-unowned', input: '{"id":1,"op":"stop"}\n', exit: 0 },
  { name: 'reject-retarget', input: '{"id":1,"op":"status","serial":"other"}\n', exit: 1 },
  { name: 'reject-unknown', input: '{"id":1,"op":"shell"}\n', exit: 1 },
  { name: 'reject-oversize', input: 'x'.repeat(16385) + '\n', exit: 1 },
  { name: 'reject-unowned-capture', input: '{"id":1,"op":"capture"}\n', exit: 1 },
];

function validateOutput(test, code, stdout) {
  assert.equal(code, test.exit, test.name + ': exit code');
  const lines = stdout.trim().split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line));
  assert(lines.length >= 2, test.name + ': ready and closed required');
  assert(lines.every(line => line.protocol === 'roost-headless-v1'));
  assert.equal(lines[0].event, 'ready');
  assert.equal(lines[0].serial, 'emulator-5554');
  const closed = lines.at(-1);
  assert.equal(closed.event, 'closed');
  assert.equal(closed.cleanup_ok, true);
  assert.equal(closed.owned, false);
  if (test.name === 'status-and-shutdown') {
    const status = lines.find(line => line.id === 1);
    assert.equal(status.result.running, false);
    assert.equal(status.result.external_state, 'not_observed');
    assert.equal(status.result.boot_completed, null);
  }
  if (test.name === 'stop-unowned') {
    const response = lines.find(line => line.id === 1);
    assert.equal(response.result.stopped, false);
    assert.equal(response.result.reason, 'not_owned');
  }
  return lines;
}

if (selfTest) {
  for (const test of cases) {
    const result = [{protocol:'roost-headless-v1',event:'ready',serial:'emulator-5554'}];
    if (test.name === 'status-and-shutdown') result.push({protocol:'roost-headless-v1',id:1,result:{running:false,external_state:'not_observed',boot_completed:null}});
    if (test.name === 'stop-unowned') result.push({protocol:'roost-headless-v1',id:1,result:{stopped:false,reason:'not_owned'}});
    result.push({protocol:'roost-headless-v1',event:'closed',cleanup_ok:true,owned:false});
    validateOutput(test,test.exit,result.map(value => JSON.stringify(value)).join('\n'));
  }
  assert.throws(() => validateOutput(cases[0],0,'not-json'));
  console.log('Smoke assertion self-test passed; no native binary executed');
} else {
  assert.equal(process.env.GITHUB_ACTIONS, 'true', 'Native smoke runs only on GitHub Actions');
  assert.equal(process.platform, 'win32', 'Windows runner required');
  const executable = resolve('src-tauri/target/release/droidbox.exe');
  const sandbox = join(process.env.RUNNER_TEMP, `roost-headless-${Date.now()}`);
  await mkdir(join(sandbox, 'data/vms', vmId), { recursive: true });
  const isolatedBinary = join(sandbox, 'droidbox.exe');
  await copyFile(executable, isolatedBinary);
  await writeFile(join(sandbox, 'data/vms', vmId, 'config.json'), JSON.stringify(config), 'utf8');
  await mkdir(evidence, { recursive: true });
  const receipts = [];
  for (const test of cases) {
    const receipt = await new Promise((resolveResult, reject) => {
      const child = spawn(isolatedBinary, ['--headless-control', vmId], {
        cwd: sandbox, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
      });
      let stdout = '', stderr = '';
      const timer = setTimeout(() => { child.kill(); reject(new Error(test.name + ': timeout')); }, 15000);
      child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
      child.stdout.on('data', data => { stdout += data; if (stdout.length > 1048576) { child.kill(); reject(new Error('Output bound exceeded')); } });
      child.stderr.on('data', data => { stderr += data; });
      child.stdin.on('error', () => {});
      child.on('error', error => { clearTimeout(timer); reject(error); });
      child.on('close', (code, signal) => { clearTimeout(timer); resolveResult({name:test.name,code,signal,stdout,stderr}); });
      child.stdin.end(test.input);
    });
    receipts.push(receipt);
    await writeFile(join(evidence, 'headless-smoke.json'), JSON.stringify({ revision: process.env.GITHUB_SHA,
      native_protocol_only: true, emulator_launched: false, local_compilation: false, cases: receipts }, null, 2), 'utf8');
    validateOutput(test,receipt.code,receipt.stdout);
  }
  console.log(`Headless protocol smoke passed: ${receipts.length} cases; no VM launched`);
}