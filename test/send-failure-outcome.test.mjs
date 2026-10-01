// 发送失败的定性：能证明"没送达"的算 failed（可被「重试失败批次」捞回来），
// 可能已经投递的才算 unknown（持有待人工核对）。
//
// 背景（2026-09-23 全项目审查）：以前 outbox 的 outcome 只看 error.outcome，而那只在
// OneBotActionError 上才有；裸 undici 失败（ECONNREFUSED 等）永远落进 unknown ——
// 一次"连接被拒"会被记成 critical 未知写入挂在"待处理"，回复静默丢失且不可重试。
import assert from 'node:assert/strict';
import { it } from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-send-outcome-'));
process.env.QQ_AGENT_DATA_DIR = dir;
process.on('exit', () => fs.rmSync(dir, { recursive: true, force: true }));

const { ChatStore } = await import('../src/core/store.js');
const { SendQueue, classifyTransportFailure } = await import('../src/onebot/sender.js');
const { setRuntimeConfig, DEFAULT_CONFIG } = await import('../src/core/config.js');

const cfg = structuredClone(DEFAULT_CONFIG);
cfg.runtime.mode = 'active';
cfg.allow.groups = ['1'];
setRuntimeConfig(cfg);

const fetchFailure = (code) => Object.assign(new TypeError('fetch failed'), { cause: { code } });

it('classifyTransportFailure：连不上=确定没送达，超时/重置/5xx=结果未知', () => {
  const refused = classifyTransportFailure(fetchFailure('ECONNREFUSED'));
  assert.equal(refused.definite, true);
  assert.equal(refused.uncertain, false);

  for (const code of ['ECONNRESET', 'ETIMEDOUT', 'EPIPE']) {
    const r = classifyTransportFailure(fetchFailure(code));
    assert.equal(r.definite, false, code);
    assert.equal(r.uncertain, true, code);
  }
  assert.equal(classifyTransportFailure(new Error('HTTP 502 Bad Gateway')).uncertain, true, '协议端 5xx 可能已转发');
  assert.equal(classifyTransportFailure(new Error('Request timeout after 15000ms')).uncertain, true);
});

// outbox 的落库状态（记账口径就是它）：failed = 确认没送达、可重试；unknown = 可能已投递、持有待核对。
const outboxState = (store, chatKey) => store.db
  .prepare("SELECT state FROM outbox WHERE chat_key=? ORDER BY rowid DESC LIMIT 1")
  .get(chatKey)?.state;

it('确认未送达的裸 fetch 失败记成 failed（可重试），不占"待核对"', async () => {
  const store = new ChatStore(0, { dataDir: dir });
  const sender = new SendQueue({ store, onebot: { sendText: async () => { throw fetchFailure('ECONNREFUSED'); } } });
  await assert.rejects(() => sender.sendTextBatch('group:1', ['hi']));
  assert.equal(store.getChatMeta('group:1').held, 0, '确定没送达的不该记成未知写入');
  assert.equal(outboxState(store, 'group:1'), 'failed', '要记成 failed，人工「重试失败批次」才捞得回来');
  store.close();
});

it('结果未知的失败仍记成 unknown（持有待人工核对）', async () => {
  const store = new ChatStore(0, { dataDir: dir });
  const sender = new SendQueue({ store, onebot: { sendText: async () => { throw fetchFailure('ECONNRESET'); } } });
  await assert.rejects(() => sender.sendTextBatch('group:1', ['hi']));
  assert.equal(store.getChatMeta('group:1').held, 1, '可能已投递的要持有，等人工核对');
  assert.equal(outboxState(store, 'group:1'), 'unknown');
  assert.equal(store.listUnknownOperations('group:1').length, 1, '未知写入要出现在待核对清单里');
  store.close();
});

it('PacketBackend 不支持时停用拍一拍，不记发送异常且不重复撞接口', async () => {
  const store = new ChatStore(0, { dataDir: dir });
  let calls = 0;
  let incidents = 0;
  const sender = new SendQueue({
    store,
    onIncident: () => { incidents += 1; },
    onebot: {
      sendPoke: async () => {
        calls += 1;
        throw Object.assign(new Error('OneBot group_poke 失败: retcode=400 packetBackend 发包能力不可用；PacketBackend 不支持当前QQ版本架构：7.0.2-53644-arm64'), {
          action: 'group_poke', outcome: 'failed', retcode: 400
        });
      }
    }
  });
  await assert.rejects(() => sender.poke('group:1', 42), (error) => error.code === 'POKE_UNAVAILABLE');
  await assert.rejects(() => sender.poke('group:1', 42), (error) => error.code === 'POKE_UNAVAILABLE');
  assert.equal(calls, 1, '能力确认失败后不应再次调用 OneBot');
  assert.equal(incidents, 0, '能力缺失不应刷异常面板');
  assert.equal(outboxState(store, 'group:1'), 'failed');
  store.close();
});
