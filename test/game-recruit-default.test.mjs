// 报名时长的"键缺失 → 产品默认 45"：干净配置（没有 recruitSeconds 这个键）里必须挂报名，
// 不能因为代码缺省 0 而变成"直接按最近发言者发牌"（2026-09-29 审查：文档/UI 说默认开、代码却是关）。
import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-recruit-default-'));
process.env.QQ_AGENT_DATA_DIR = dataDir;
// 注意：故意不写 recruitSeconds
fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { private: ['1'] },
  api: { baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'm' },
  groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, games: ['undercover', 'werewolf'] }
}));

const { GroupGameManager } = await import('../src/features/group-game.js');
const { ChatStore } = await import('../src/core/store.js');

test('配置里没写 recruitSeconds 时按默认 45 挂报名（不是直接发牌）', async () => {
  const store = new ChatStore(0, { dataDir, filename: 'recruit-default.sqlite' });
  for (let i = 1; i <= 6; i += 1) {
    store.appendIncoming('group:1', { mid: `m${i}`, ts: Date.now() - i * 1000, senderId: `u${i}`, senderName: `群友${i}`, text: '在' }, { recordOnly: true });
  }
  const sent = [];
  const mgr = new GroupGameManager({
    store,
    sender: { async sendTextBatch(chatKey, msgs) { sent.push({ chatKey, msgs: [...msgs] }); return { sent: [{ messageId: 'x' }] }; } },
    log: () => {}, now: () => Date.now(), rng: () => 0.42
  });
  const r = await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.equal(mgr.games.get('group:1').state.phase, 'recruiting', '缺省应挂报名');
  // "缺省 = 45 秒"这个数量本身也要钉住：只有 phase 是 recruiting 的话，默认值改成 5 秒也照样绿
  const until = Number(mgr.games.get('group:1').state.recruitUntil || 0);
  const left = until - Date.now();
  assert.ok(left > 40 * 1000 && left <= 46 * 1000, `缺省报名窗口应约 45 秒，实际剩 ${Math.round(left / 1000)} 秒`);
  assert.equal(sent.filter((x) => x.chatKey.startsWith('private:')).length, 0, '报名阶段不发私聊');
  store.close();
});
