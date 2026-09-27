// 人设切换的两处控制台行为：
//  1) 真的改了人设 → 打 changedAt 时间戳（提示词据此在 24 小时内提醒"旧口癖不作数"）
//  2) 「换人设后清空交接」接口 → 清掉各群的会话交接 + 关闭进行中的线程（不动聊天记录/记忆）
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-persona-switch-'));
process.env.QQ_AGENT_DATA_DIR = root;
const { DEFAULT_CONFIG, getConfig, updateConfig } = await import('../src/core/config.js');
const { createApp } = await import('../src/console/app.js');
const { ChatStore } = await import('../src/core/store.js');
const { MemoryStore } = await import('../src/memory/memory.js');

async function freePort() {
  const server = http.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

test('换人设打时间戳；清空交接接口清掉交接并关线程', async (t) => {
  const port = await freePort();
  const cfg = structuredClone(DEFAULT_CONFIG);
  cfg.server = { ...cfg.server, host: '127.0.0.1', port, token: '' };
  cfg.runtime.mode = 'observe';
  cfg.onebot.wsUrl = 'ws://127.0.0.1:1';
  cfg.onebot.httpUrl = 'http://127.0.0.1:1';
  updateConfig(cfg);
  const app = createApp({ log: () => {} });
  t.after(async () => {
    await app.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });
  await app.start();
  const request = async (route, { method = 'GET', body } = {}) => {
    const response = await fetch(`http://127.0.0.1:${port}${route}`, {
      method,
      headers: { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body)
    });
    return { status: response.status, body: await response.json() };
  };

  // 1) 改正文 → 打时间戳
  const before = getConfig().persona.changedAt || 0;
  await request('/api/config', {
    method: 'POST',
    body: { persona: { ...getConfig().persona, roleText: '# 另一张卡\n这次换一张。' } }
  });
  const stamped = getConfig().persona.changedAt || 0;
  assert.ok(stamped > before, '改了人设要打时间戳（提示词据此提醒旧口癖不作数）');

  // 2) 保存一模一样的正文 → 时间戳不该被刷新（否则每次保存都触发 24 小时提醒）
  await request('/api/config', {
    method: 'POST',
    body: { persona: { ...getConfig().persona } }
  });
  assert.equal(getConfig().persona.changedAt, stamped, '没改就不刷新');

  // 3) 造一个"有交接 + 有线程"的会话，然后调清空接口
  const chatKey = 'group:1';
  const store = new ChatStore(0, { dataDir: root });
  store.appendIncoming(chatKey, { text: '在吗', ts: Date.now(), senderId: '42', senderName: '群友', mid: 'm1' });
  store.upsertConversationThread(chatKey, { mode: 'lifecycle', state: 'active' });
  const memory = new MemoryStore();
  memory.setHandoff(chatKey, { topic: '上一张卡的话题', summary: '说了"行了 别汪了喵"' });
  assert.ok(memory.getHandoff(chatKey), '交接要先写进去');
  store.close();

  // 破坏性操作要 confirm 门槛（与删表情/删记忆那批接口同款）
  const noConfirm = await request('/api/persona/reset-handoffs', { method: 'POST' });
  assert.equal(noConfirm.status, 409, '不带 confirm 应被拒');
  const reset = await request('/api/persona/reset-handoffs', { method: 'POST', body: { confirm: true } });
  assert.equal(reset.status, 200);
  assert.ok(reset.body.chats >= 1, '要报告清了几个会话');
  assert.equal(typeof reset.body.activeRuns, 'number', '要报告是否有在途运行（会写回旧交接）');
  const memoryAfter = new MemoryStore();
  assert.equal(memoryAfter.getHandoff(chatKey), null, '交接要被清掉');
  const storeAfter = new ChatStore(0, { dataDir: root });
  const thread = storeAfter.getConversationThread(chatKey);
  assert.ok(!thread || thread.state === 'closed', '线程要关掉（这样 checkpoint 就不会再注入）');
  storeAfter.close();
});
