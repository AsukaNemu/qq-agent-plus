// test/local 的跑法统一入口：给每个用例准备一个临时数据目录，逐个执行。
//
// 为什么要这个：这些用例都要求 QQ_AGENT_DATA_DIR 指向临时目录（绝不指向生产数据），
// 直接写进 npm script 在各个 shell 里难以跨平台；CI 与本地都用这一份逻辑。
//
// 用法：node test/local/run.mjs
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const CASES = [
  'test-inline-fallback.mjs',
  'test-sender-retry.mjs',
  'test-qzone-backoff.mjs',
  'test-qzone-intervals.mjs',
  'test-qzone-reply-fallback.mjs',
  'test-sticker-lookup.mjs',
  'test-send-tools.mjs',
  'test-thinking-toolchoice.mjs',
  'test-card-segments.mjs',
  'game-drive.mjs',
  'game-sim.mjs'
];

const here = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
let failed = 0;

// 每个用例一个全新的临时数据目录：用例之间互不污染，也保证单个用例重跑时的行为可预期。
for (const file of CASES) {
  const target = path.join(here, file);
  if (!fs.existsSync(target)) {
    console.error(`FAIL ${file}（文件不存在）`);
    failed += 1;
    continue;
  }
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), `qq-agent-local-${path.basename(file, '.mjs')}-`));
  try {
    const run = spawnSync(process.execPath, [target], {
      stdio: 'inherit',
      env: { ...process.env, QQ_AGENT_DATA_DIR: dataDir }
    });
    const ok = run.status === 0;
    if (!ok) failed += 1;
    console.log(`${ok ? 'PASS' : 'FAIL'} ${file}`);
  } finally {
    fs.rmSync(dataDir, { recursive: true, force: true });
  }
}

console.log(`\n本地回归结果: ${CASES.length - failed}/${CASES.length} 通过`);
process.exit(failed === 0 ? 0 : 1);
