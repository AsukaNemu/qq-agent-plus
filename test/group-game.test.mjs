// 群游戏状态机测试（框架 + 数字炸弹 + 谁是卧底）：真 ChatStore、假发送器、可控时钟与随机数。
import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-games-'));
process.env.QQ_AGENT_DATA_DIR = dataDir;

fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { private: ['100000001'] },
  api: { baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'mock', thinking: 'on' },
  groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, maxDurationMin: 60, dailyLimitPerChat: 6 }
}));

const { GroupGameManager } = await import('../src/features/group-game.js');
const { ChatStore } = await import('../src/core/store.js');
const { updateConfig } = await import('../src/core/config.js');

function makeWorld({ rng = () => 0.42, limit = 6, players = 4, privateDm = false } = {}) {
  // 每个"世界"从零开始：games.json 是共享文件，不清会跨用例污染（上一局的局与每日计数都会带过来）
  fs.rmSync(path.join(dataDir, 'games.json'), { force: true });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: privateDm, dailyLimitPerChat: limit, maxDurationMin: 60 } });
  const store = new ChatStore(0, { dataDir, filename: `games-${Math.random().toString(36).slice(2)}.sqlite` });
  // 活跃成员（activeMembers 取的是 self=0 的最近发言者）
  for (let i = 1; i <= players; i += 1) {
    store.appendIncoming('group:1', {
      mid: 100 + i, ts: Date.now() - i * 1000, senderId: `u${i}`, senderName: `群友${i}`, text: '在'
    }, { recordOnly: true });
  }
  const sent = [];
  // 记录 options：私聊豁免（gameScoped）这类标记只能从这里断言
  const sender = { sendTextBatch: async (chatKey, msgs, options = {}) => { sent.push({ chatKey, msgs: [...msgs], options }); return { message_id: sent.length }; } };
  let clock = Date.now();
  const mgr = new GroupGameManager({ store, sender, log: () => {}, now: () => clock, rng });
  return { store, sent, mgr, setClock: (v) => { clock = v; }, getClock: () => clock };
}

const say = (store, uid, name, text) => store.appendIncoming('group:1', {
  mid: 5000 + Math.floor(Math.random() * 100000), ts: Date.now(), senderId: uid, senderName: name, text
}, { recordOnly: true });

test('数字炸弹：区间收窄、越界提示、踩中即结束（rng 固定 → 炸弹 43）', async () => {
  const { store, sent, mgr } = makeWorld({ rng: () => 0.42 });
  const r = await mgr.start({ chatKey: 'group:1', gameId: 'number-bomb' });
  assert.equal(r.ok, true);
  assert.match(sent[0].msgs[0], /数字炸弹开局/);

  say(store, 'u1', '群友1', '我 12 点要开会，先撤了');
  await mgr.handleNewMessages('group:1');
  assert.equal(sent.length, 1, '聊天里带数字不算猜测（不误收窄、不刷屏）');

  say(store, 'u1', '群友1', '我猜 10');
  await mgr.handleNewMessages('group:1');
  assert.equal(sent.length, 1, '区间内的猜测不刷屏');

  say(store, 'u2', '群友2', '猜 200');
  await mgr.handleNewMessages('group:1');
  assert.match(sent.at(-1).msgs[0], /不在这段里/);

  say(store, 'u3', '群友3', '猜 43！');
  await mgr.handleNewMessages('group:1');
  assert.match(sent.at(-1).msgs[0], /踩中炸弹 43/);
  assert.equal(mgr.games.has('group:1'), false, '结束后清空');
});

test('谁是卧底：开局私聊发词 → 依次发言 → 投票淘汰 → 平民获胜', async () => {
  // rng 第 1 次选词组、第 2 次定卧底位置
  const seq = [0, 0.6];
  const { store, sent, mgr } = makeWorld({ rng: () => seq.shift() ?? 0 });
  const r = await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  assert.equal(r.ok, true, JSON.stringify(r));
  const privates = sent.filter((x) => x.chatKey.startsWith('private:'));
  assert.equal(privates.length, 4, '四人各发一条私聊词');
  assert.ok(privates.every((x) => /你的词是/.test(x.msgs[0])));
  const words = privates.map((x) => /「(.+?)」/.exec(x.msgs[0])[1]);
  assert.equal(new Set(words).size, 2, '只有两种词（平民词 + 卧底词）');
  // 卧底是第 3 个（floor(0.6*4)=2 → u3）
  const spyWord = words[2];
  assert.equal(words.filter((w) => w === spyWord).length, 1, '卧底词只有一个人拿到');

  // 依次发言（顺序 = activeMembers 的 lastTs 倒序 → u1,u2,u3,u4？以 state.order 为准）
  const order = mgr.games.get('group:1').state.order;
  for (const uid of order) say(store, uid, `群友${uid.slice(1)}`, '这是一种日常用品');
  await mgr.handleNewMessages('group:1');
  assert.equal(mgr.games.get('group:1').state.phase, 'vote');
  assert.match(sent.at(-1).msgs[0], /开始投票/);

  // 全员投 u3（卧底）→ 平民获胜；u3 自己不能投自己（插件会挡），改投 1
  for (const uid of order) say(store, uid, `群友${uid.slice(1)}`, uid === 'u3' ? '投 1' : '投 3');
  await mgr.handleNewMessages('group:1');
  const endMsg = sent.at(-1).msgs[0];
  assert.match(endMsg, /平民获胜/);
  assert.match(endMsg, /卧底是/);
  assert.equal(mgr.games.has('group:1'), false);
});

test('超时推进：当前发言者一直不接话 → 跳过并继续；时长上限到点自动结束', async () => {
  const { store, sent, mgr, setClock, getClock } = makeWorld({ rng: () => 0 });
  await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  setClock(getClock() + 200 * 1000);          // 超过 roundSeconds=150
  await mgr.tick();
  assert.match(sent.at(-1).msgs[0], /没接上，先跳过/);

  // 拨到超过 maxDurationMin（60 分钟）→ 自动收尾
  const g = mgr.games.get('group:1');
  if (g) setClock(g.deadlineAt + 1000);
  await mgr.tick();
  if (sent.at(-1)) assert.match(sent.at(-1).msgs[0], /时间到了|结束/);
  assert.equal(mgr.games.has('group:1'), false);
});

test('谁是卧底：第 2 轮起排头不说话也会被超时跳过（轮次切换即计时）', async () => {
  // 回归 2026-09-29 审查 P1：nextRound 把 phaseStartedAt 置 0，onTick 里 `0 || now` 恒等 now，
  // 150 秒计时永远不开始 → 第 2 轮排头 AFK 时整局卡到 45 分钟上限
  const { store, sent, mgr, setClock, getClock } = makeWorld({ rng: () => 0.6 });
  await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  let order = mgr.games.get('group:1').state.order;
  for (const uid of order) say(store, uid, `群友${uid.slice(1)}`, '日常用品');
  await mgr.handleNewMessages('group:1');
  // 全员投票淘汰 u1（卧底是 u3）→ 进第 2 轮（u1 不能投自己，改投 u2）
  for (const uid of order) say(store, uid, `群友${uid.slice(1)}`, uid === 'u1' ? '投 2' : '投 1');
  await mgr.handleNewMessages('group:1');
  const st2 = mgr.games.get('group:1').state;
  assert.equal(st2.round, 2);
  assert.equal(st2.phase, 'speak');
  assert.ok(st2.phaseStartedAt > 0, '第 2 轮的计时起点必须在轮次切换时就设置');
  // 排头一直不说话 → 150 秒后 tick 跳过他
  setClock(st2.phaseStartedAt + 200 * 1000);
  await mgr.tick();
  assert.match(sent.at(-1).msgs[0], /没接上，先跳过/);
});

test('谁是卧底：投票阶段一票都没有 → 超时直接进下一轮，不空转', async () => {
  const { store, sent, mgr, setClock, getClock } = makeWorld({ rng: () => 0.6 });
  await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  const order = mgr.games.get('group:1').state.order;
  for (const uid of order) say(store, uid, `群友${uid.slice(1)}`, '这是一种日常用品');
  await mgr.handleNewMessages('group:1');
  assert.equal(mgr.games.get('group:1').state.phase, 'vote');
  setClock(getClock() + 200 * 1000);   // 投票超时（roundSeconds=150）且 0 票
  await mgr.tick();
  const st = mgr.games.get('group:1').state;
  assert.equal(st.phase, 'speak', '直接进下一轮发言');
  assert.equal(st.round, 2);
  assert.match(sent.at(-1).msgs[0], /没人投票|第 2 轮/);
});

test('门控与限额：私聊未开 → 拒绝卧底；白名单外不认；每日上限封顶', async () => {
  const { mgr } = makeWorld({ limit: 1 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: false, dailyLimitPerChat: 1, maxDurationMin: 60 } });
  const denied = await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  assert.equal(denied.ok, false);
  assert.match(denied.error, /允许私聊发身份/);

  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, dailyLimitPerChat: 1, maxDurationMin: 60 } });
  const first = await mgr.start({ chatKey: 'group:1', gameId: 'number-bomb' });
  assert.equal(first.ok, true);
  await mgr.stop('group:1', '测试');
  const second = await mgr.start({ chatKey: 'group:1', gameId: 'number-bomb' });
  assert.equal(second.ok, false);
  assert.match(second.error, /已经开过/);

  const outside = await mgr.start({ chatKey: 'group:999', gameId: 'number-bomb' });
  assert.equal(outside.ok, false);
  assert.match(outside.error, /白名单/);
});

test('重启恢复：进行中的局写盘后能在新实例里继续', async () => {
  const { store, mgr } = makeWorld({ rng: () => 0.1 });
  await mgr.start({ chatKey: 'group:1', gameId: 'number-bomb' });
  const saved = JSON.parse(fs.readFileSync(path.join(dataDir, 'games.json'), 'utf8'));
  assert.ok(saved.games['group:1'], 'games.json 里有进行中的局');

  const sender = { sendTextBatch: async () => ({}) };
  const revived = new GroupGameManager({ store, sender, log: () => {}, now: () => Date.now(), rng: () => 0.1 });
  assert.equal(revived.games.has('group:1'), true, '新实例恢复该局');
  assert.equal(revived.summaryFor('group:1').includes('数字炸弹'), true);
});

test('狼人杀的私聊行动：入口接管（标记已读、发回执），非参与者不接管', async () => {
  const { store, sent, mgr } = makeWorld({ players: 6 });
  const r = await mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(r.ok, true, JSON.stringify(r));
  const state = mgr.games.get('group:1').state;
  const seer = state.roles.find((x) => x.role === 'seer');
  const villager = state.roles.find((x) => x.role === 'villager');
  const outsider = 'u99';
  sent.length = 0;

  // 预言家私聊查人 → 引擎接管：回执 + 查验结果，且这条私聊被标记已读（不再唤醒模型）
  const stored = store.appendIncoming(`private:${seer.userId}`, {
    mid: 'pm-1', ts: Date.now(), senderId: seer.userId, senderName: seer.name, text: '查 1', reply: null, media: []
  });
  const took = await mgr.consumePrivateAction(`private:${seer.userId}`, stored);
  assert.equal(took, true, '属于进行中的局 → 引擎接管');
  assert.equal(store.findByMid(`private:${seer.userId}`, 'pm-1').state, 'acked', '接管后要标记已读');
  assert.match(sent.at(-1).msgs[0], /查验结果/);
  assert.equal(sent.at(-1).chatKey, `private:${seer.userId}`);

  // 解析不了的私聊不接管（交回普通链路，玩家发了不至于没人理）
  const junk = store.appendIncoming(`private:${villager.userId}`, {
    mid: 'pm-2', ts: Date.now(), senderId: villager.userId, senderName: villager.name, text: '在吗晚上好', reply: null, media: []
  });
  const took2 = await mgr.consumePrivateAction(`private:${villager.userId}`, junk);
  assert.equal(took2, true, '平民夜里也会拿到一句"你没行动"的回执（属于游戏私聊）');

  // 不在局里的人：不接管
  const other = store.appendIncoming(`private:${outsider}`, {
    mid: 'pm-3', ts: Date.now(), senderId: outsider, senderName: '路人', text: '查 1', reply: null, media: []
  });
  assert.equal(await mgr.consumePrivateAction(`private:${outsider}`, other), false);

  // 水位：同一条不会被 tick 再喂一遍
  const before = store.recent(`private:${seer.userId}`, { limit: 5 }).length;
  await mgr.tick();
  assert.equal(sent.filter((x) => x.chatKey === `private:${seer.userId}` && /查验结果/.test(x.msgs[0])).length, 1, '查验结果只发一次');
  assert.ok(before >= 1);
  assert.equal(store.findByMid(`private:${seer.userId}`, 'pm-1').state, 'acked');
});

test('私聊豁免开关：关着不带标记、开着对在册玩家带 gameScoped（deny 语义在 access 层）', async () => {
  const off = makeWorld({ players: 6, privateDm: false });
  await off.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  const offPriv = off.sent.find((x) => x.chatKey.startsWith('private:'));
  assert.ok(offPriv, '开局要发身份私聊');
  assert.notEqual(offPriv.options?.gameScoped, true, '开关关着时不得带豁免标记');

  const on = makeWorld({ players: 6, privateDm: true });
  await on.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  const onPrivs = on.sent.filter((x) => x.chatKey.startsWith('private:'));
  assert.ok(onPrivs.length >= 6, '6 人各一条身份私聊');
  assert.ok(onPrivs.every((x) => x.options?.gameScoped === true), '开关开着且收件人在册 → 每条私聊都带豁免标记');
});
