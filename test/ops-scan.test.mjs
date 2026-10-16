import assert from 'node:assert/strict';
import { test } from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

function fixture(t, source) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-ops-scan-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  if (source) fs.writeFileSync(path.join(dir, 'example.js'), source);
  return dir;
}

function scan(dir) {
  return spawnSync(process.execPath, ['src/ops.js', 'scan', dir, '--strict', '--no-ignore'], {
    encoding: 'utf8', env: { ...process.env, QQ_AGENT_DATA_DIR: dir }
  });
}

test('strict scan accepts callbacks in function and private method parameters', (t) => {
  const dir = fixture(t, `
    export function select(api, availability = () => ({ pending: 0, waitMs: 0 })) {
      return availability(api);
    }
    class Feed {
      async #request(url, options = {}, parser = JSON.parse) {
        return parser(url);
      }
    }
  `);
  const result = scan(dir);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.match(result.stdout, /可疑未定义调用点: 0/);
});

test('callback parameters do not hide unrelated undefined calls', (t) => {
  const dir = fixture(t, `
    function check(value, callback = (a, b) => a) {
      callback(value);
      missingMaintenanceFunction(value);
    }
  `);
  const result = scan(dir);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout, /missingMaintenanceFunction/);
  assert.doesNotMatch(result.stdout, /callback\(第/);
});

test('strict scan recognizes destructured imports and callback options', (t) => {
  const dir = fixture(t, `
    const { setGlobalDispatcher: dispatch, nested: { worker }, plain } = await import('example');
    function run({ log = console.log, options = {} } = {}) {
      log(options);
      dispatch(worker);
      return plain();
    }
  `);
  const result = scan(dir);
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test('strict scan rejects a missing directory and an empty directory', (t) => {
  const dir = fixture(t);
  assert.equal(scan(path.join(dir, 'missing')).status, 1);
  assert.equal(scan(dir).status, 1);
});
