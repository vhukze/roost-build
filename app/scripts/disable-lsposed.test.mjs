
// Pure filesystem fixtures only. Never ADB, /data/adb, an app, grants or a VM.
import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const sourcePath = fileURLToPath(new URL('../src-tauri/src/disable-lsposed.sh', import.meta.url));
const source = readFileSync(sourcePath, 'utf8');
const rootLine = "base='/data/adb'";
const marker = 'ROOST_LSPOSED_DISABLE_STAGED';
const sh = process.env.ROOST_FIXTURE_SHELL ?? (process.platform === 'win32' ? 'C:/Program Files/Git/bin/bash.exe' : '/bin/sh');
assert(existsSync(sh), 'Fixture shell missing; do not silently omit script tests');
assert(!source.includes('\r'), 'Internal shell source must remain LF; inspect scoped .gitattributes');
assert.equal(source.split(rootLine).length, 2, 'Exactly one internal base assignment required');
assert.equal((source.match(/\/data\/adb/g) ?? []).length, 1, 'All accesses must derive from the fixed base');
assert(!/^\s*(?:rm|reboot|adb|su|eval|exec)(?:\s|$)/m.test(source), 'Unexpected action in fixed staging script');

const paths = ['modules/zygisk_lsposed', 'modules/riru_lsposed', 'modules_update/zygisk_lsposed', 'modules_update/riru_lsposed'];
function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'roost-disable-fixture-'));
  writeFileSync(join(root, 'FIXTURE-ONLY.txt'), 'Retained pure filesystem fixture. No device data.\n');
  const adb = join(root, 'adb'); mkdirSync(adb);
  return { root, adb, path: name => join(adb, name) };
}
function populate(f) { for (const name of paths) mkdirSync(f.path(name), { recursive: true }); }
function execute(f, prefix = '') {
  const base = f.adb.replaceAll('\\', '/');
  const quoted = "'" + base.replaceAll("'", "'\\''") + "'";
  const script = source.replace(rootLine, 'base=' + quoted);
  assert(!script.includes('/data/adb'), 'Real guest path survived fixture mapping');
  const env = { ...process.env, LC_ALL: 'C', MSYS_NO_PATHCONV: '1' };
  delete env.BASH_ENV; delete env.ENV;
  const args = process.platform === 'win32' ? ['--noprofile', '--norc', '-c', prefix + '\n' + script] : ['-c', prefix + '\n' + script];
  const result = spawnSync(sh, args, { encoding: 'utf8', env, windowsHide: true, timeout: 10000 });
  if (result.error) throw result.error;
  return result;
}
function failed(result) {
  assert.notEqual(result.status, 0, 'Failure was masked by later success');
  assert(!result.stdout.includes(marker), 'Failed action emitted completion marker');
  assert(result.stderr.includes('incomplete'), 'Partial-state diagnostic absent');
}
function absent(f) { for (const name of paths) assert(!existsSync(f.path(name + '/disable'))); }

test('fixed staging script parses without executing any action', () => {
  const args = process.platform === 'win32' ? ['--noprofile', '--norc', '-n'] : ['-n'];
  const result = spawnSync(sh, args, { input: source, encoding: 'utf8', windowsHide: true, timeout: 10000 });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, result.stderr);
});

test('all four module directories receive disable markers without overwriting existing content', () => {
  const f = fixture(); populate(f);
  writeFileSync(f.path(paths[0] + '/disable'), 'existing marker content\n');
  const result = execute(f);
  assert.equal(result.status, 0, result.stderr); assert.equal(result.stdout.trim(), marker);
  for (const name of paths) assert(lstatSync(f.path(name + '/disable')).isFile());
  assert.equal(readFileSync(f.path(paths[0] + '/disable'), 'utf8'), 'existing marker content\n');
});

test('missing module trees succeed without creating trees or touching unrelated data', () => {
  const f = fixture(); mkdirSync(f.path('unrelated')); writeFileSync(f.path('unrelated/keep'), 'keep');
  const result = execute(f);
  assert.equal(result.status, 0, result.stderr); assert.equal(result.stdout.trim(), marker);
  assert(!existsSync(f.path('modules'))); assert(!existsSync(f.path('modules_update')));
  assert.equal(readFileSync(f.path('unrelated/keep'), 'utf8'), 'keep');
});

test('a later module path that is a file fails before any earlier marker is written', () => {
  const f = fixture(); mkdirSync(f.path(paths[0]), { recursive: true });
  mkdirSync(f.path('modules_update')); writeFileSync(f.path(paths[3]), 'not a directory');
  failed(execute(f)); absent(f);
});

test('a later disable marker directory is not treated as disabled or truncated', () => {
  const f = fixture(); populate(f); mkdirSync(f.path(paths[3] + '/disable'));
  failed(execute(f)); assert(!existsSync(f.path(paths[0] + '/disable')));
  assert(lstatSync(f.path(paths[3] + '/disable')).isDirectory());
});

test('malformed remove marker stops preflight before any writes', () => {
  const f = fixture(); populate(f); mkdirSync(f.path(paths[3] + '/remove'));
  failed(execute(f)); absent(f);
});

test('listing failure in the later staged tree is never hidden by absent directories', () => {
  const f = fixture(); populate(f);
  failed(execute(f, 'ls() { case "$*" in *modules_update*) return 23 ;; esac; command ls "$@"; }'));
  absent(f);
});

test('subshell setup failure stops before creating any marker', () => {
  const f = fixture(); populate(f);
  failed(execute(f, 'umask() { return 23; }'));
  absent(f);
});

test('marker creation failure stops immediately instead of falling through later absent paths', () => {
  const f = fixture(); mkdirSync(f.path(paths[0]), { recursive: true });
  const inject = 'umask() { command mkdir "$base/modules/zygisk_lsposed/disable" || return $?; command umask "$@"; }';
  failed(execute(f, inject));
  assert(lstatSync(f.path(paths[0] + '/disable')).isDirectory());
});

test('post-write verification failure reports partial state and never returns success', () => {
  const f = fixture(); populate(f);
  // Preflight lists base + two parents + four modules (seven calls).
  const result = execute(f, 'calls=0; ls() { calls=$((calls+1)); if [ "$calls" -eq 8 ]; then return 23; fi; command ls "$@"; }');
  failed(result);
  for (const name of paths) assert(lstatSync(f.path(name + '/disable')).isFile());
});

test('symlinked marker is rejected without modifying its target', context => {
  const f = fixture(); populate(f); const target = join(f.root, 'outside-marker'); writeFileSync(target, 'preserve');
  try { symlinkSync(target, f.path(paths[3] + '/disable'), 'file'); }
  catch (error) { if (['EPERM', 'EACCES', 'ENOTSUP'].includes(error.code)) return context.skip('Host does not support creating fixture file symlinks'); throw error; }
  failed(execute(f)); assert(!existsSync(f.path(paths[0] + '/disable')));
  assert.equal(readFileSync(target, 'utf8'), 'preserve');
});

test('both product entrypoints delegate to one shared staging implementation', () => {
  const read = name => readFileSync(fileURLToPath(new URL('../src-tauri/src/' + name, import.meta.url)), 'utf8');
  const managed = read('managed.rs'), commands = read('commands.rs'), headless = read('headless.rs');
  assert(managed.includes('include_str!("disable-lsposed.sh")'));
  assert(managed.includes('result.ok && result.code == 0 && result.stdout.trim() == "' + marker + '"'));
  assert(commands.includes('crate::managed::disable_lsposed(&adb).await?'));
  assert(headless.includes('crate::managed::disable_lsposed(adb).await?'));
  assert(!commands.includes('touch "$d/disable"')); assert(!headless.includes('touch "$d/disable"'));
});