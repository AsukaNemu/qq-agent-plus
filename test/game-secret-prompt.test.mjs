// 引擎发的游戏私聊（身份/查验结果/行动回执）不能进模型上下文：模型在私聊里不该是上帝视角。
// 链路：群游戏管理器发送时登记 message id → ingest 落库打 eventKind='game-secret' →
// buildPastState 过滤掉（2026-09-29 审查 P2）。
import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-game-secret-'));
process.env.QQ_AGENT_DATA_DIR = dataDir;
fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { private: ['1'] },
  api: { baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'm' }
}));

const { ChatStore } = await import('../src/core/store.js');
const { buildPastState } = await import('../src/llm/prompt.js');
const { SendQueue } = await import('../src/onebot/sender.js');
const { buildToolDefs, executeTool } = await import('../src/tools/tools-core.js');

test('game-secret 标记的私聊不进提示词历史；普通消息照常进', () => {
  const store = new ChatStore(0, { dataDir, filename: 'secret.sqlite' });
  store.appendIncoming('private:1', { mid: 'u1', ts: Date.now() - 3000, senderId: '1', senderName: '阿猫', text: '在吗', reply: null, media: [] }, { recordOnly: true });
  // 引擎发的身份私聊（模拟 ingest 打标后的落库行）
  store.appendSelf('private:1', { mid: 's1', ts: Date.now() - 2000, text: '【狼人杀】你是**狼人**。队友：2 号', eventKind: 'game-secret' });
  // 引擎发的查验结果
  store.appendSelf('private:1', { mid: 's2', ts: Date.now() - 1000, text: '🔮 查验结果：3 号 是「狼人」。', eventKind: 'game-secret' });
  // 机器人正常聊天（不带标记）
  store.appendSelf('private:1', { mid: 's3', ts: Date.now(), text: '我也在，刚忙完', eventKind: 'message' });

  const state = buildPastState(store, 'private:1');
  assert.equal(/你是\*\*狼人\*\*|查验结果/.test(state.text), false, '身份与查验结果不能进提示词：' + state.text);
  assert.equal(state.text.includes('我也在，刚忙完'), true, '机器人自己的普通发言要保留');
  assert.equal(state.text.includes('在吗'), true, '用户的普通消息要保留');
  store.close();
});

test('端到端：真实发送次序下，引擎私聊落库就是 game-secret（不靠 ingest 回显补标）', async () => {
  // 这条用例是给 P0 的：sender 自己才是首次写库者（sendTextBatch 内部 appendSelf），
  // 而 store 按 (chat_key, mid) 幂等、命中重复不会回填 event_kind —— 所以标记必须在发送侧就给到。
  const store = new ChatStore(0, { dataDir, filename: 'secret-e2e.sqlite' });
  let n = 0;
  const onebot = { async sendText() { n += 1; return { message_id: `mid-${n}` }; } };
  const q = new SendQueue({ onebot, store });

  await q.sendTextBatch('private:1', ['【狼人杀】你是**狼人**。队友：2 号'], { eventKind: 'game-secret' });
  await q.sendTextBatch('private:1', ['🔮 查验结果：3 号 是「狼人」。'], { eventKind: 'game-secret' });
  await q.sendTextBatch('private:1', ['我也在，刚忙完'], {});   // 机器人普通发言：不标

  const secret = store.findByMid('private:1', 'mid-1');
  assert.equal(secret?.eventKind, 'game-secret', '引擎私聊落库就要是 game-secret：' + JSON.stringify(secret));
  assert.equal(store.findByMid('private:1', 'mid-3')?.eventKind, 'message', '普通发言不能被误标');

  store.appendIncoming('private:1', { mid: 'u1', ts: Date.now(), senderId: '1', senderName: '阿猫', text: '在吗', reply: null, media: [] }, { recordOnly: true });
  const state = buildPastState(store, 'private:1');
  assert.equal(/你是\*\*狼人\*\*|查验结果/.test(state.text), false, '身份与查验结果不能进提示词：' + state.text);
  assert.equal(state.text.includes('我也在，刚忙完'), true, '机器人自己的普通发言要保留');
  // 翻页工具的口径同源：这里只验 store 能给出标记，工具侧的过滤在 tools 用例里
  const rows = store.recent('private:1', { limit: 10, readOnly: true });
  assert.equal(rows.filter((m) => m.eventKind === 'game-secret').length, 2, '两条引擎私聊都要带上标记');
  store.close();
});

test('工具出口也要过闸：按 id 单查/列可见 id 都看不到 game-secret', async () => {
  const store = new ChatStore(0, { dataDir, filename: 'secret-tools.sqlite' });
  store.appendIncoming('private:1', { mid: 'u9', ts: Date.now() - 5000, senderId: '1', senderName: '阿猫', text: '在吗', reply: null, media: [] }, { recordOnly: true });
  store.appendSelf('private:1', { mid: 's9', ts: Date.now() - 4000, text: '【狼人杀】你是**狼人**。队友：2 号', eventKind: 'game-secret' });
  store.appendSelf('private:1', { mid: 's10', ts: Date.now(), text: '我也在，刚忙完' });

  const defs = buildToolDefs();
  const ctx = { store, chatKey: 'private:1', session: {} };
  const recent = await executeTool(defs, ctx, 'get_recent_messages', JSON.stringify({ limit: 10 }));
  assert.equal(/你是\*\*狼人\*\*/.test(recent.content), false, '翻页工具不得看到引擎私聊：' + recent.content);

  // 报错提示里列的"最近可见 id"也不能把 secret 的 id 吐出来
  const miss = await executeTool(defs, ctx, 'get_message_detail', JSON.stringify({ messageId: '不存在的id' }));
  assert.equal(/s9/.test(miss.content), false, 'midHint 不得包含 game-secret 的 id：' + miss.content);

  // 直接用 id 查：必须当它不存在
  const detail = await executeTool(defs, ctx, 'get_message_detail', JSON.stringify({ messageId: 's9' }));
  assert.equal(detail.isError, true, '按 id 单查也要挡住：' + detail.content);
  assert.match(detail.content, /找不到消息/, detail.content);
  assert.equal(/你是\*\*狼人\*\*/.test(detail.content), false, '更不能回正文');

  // 普通消息照常能查到（别把闸门做成一律拒绝）
  const normal = await executeTool(defs, ctx, 'get_message_detail', JSON.stringify({ messageId: 's10' }));
  assert.equal(normal.isError, undefined);
  assert.match(normal.content, /我也在，刚忙完/);
  store.close();
});

test('重复落库补标：先按普通消息写库、后才知道是引擎私聊时，标记要补上', () => {
  const store = new ChatStore(0, { dataDir, filename: 'secret-backfill.sqlite' });
  store.appendSelf('private:2', { mid: 'b1', ts: Date.now(), text: '【狼人杀】你的查验结果：3 号是狼人' });
  assert.equal(store.findByMid('private:2', 'b1').eventKind, 'message', '第一次是普通消息');
  const again = store.appendSelf('private:2', { mid: 'b1', ts: Date.now(), text: '【狼人杀】你的查验结果：3 号是狼人', eventKind: 'game-secret' });
  assert.equal(again.duplicate, true, '应命中重复');
  assert.equal(store.findByMid('private:2', 'b1').eventKind, 'game-secret', '命中重复也要把标记补上');
  const state = buildPastState(store, 'private:2');
  assert.equal(/查验结果/.test(state.text), false, '补标之后不得再进提示词');
  store.close();
});
