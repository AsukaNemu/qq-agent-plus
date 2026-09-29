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
  groupGame: {
    enabled: true, chats: ['group:1'], allowPrivateInvite: true, maxDurationMin: 60, dailyLimitPerChat: 6,
    recruitSeconds: 0, games: ['number-bomb', 'undercover', 'werewolf']
  }
}));

const { GroupGameManager } = await import('../src/features/group-game.js');
const { ChatStore } = await import('../src/core/store.js');
const { updateConfig } = await import('../src/core/config.js');

function makeWorld({ rng = () => 0.42, limit = 6, players = 4, privateDm = false } = {}) {
  // 每个"世界"从零开始：games.json 是共享文件，不清会跨用例污染（上一局的局与每日计数都会带过来）
  fs.rmSync(path.join(dataDir, 'games.json'), { force: true });
  updateConfig({
    groupGame: {
      enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: privateDm,
      dailyLimitPerChat: limit, maxDurationMin: 60,
      recruitSeconds: 0, games: ['number-bomb', 'undercover', 'werewolf'], maxPlayers: 10, discussSeconds: 0
    }
  });
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

test('超时推进：没人描述 → 到点直接进投票（不点名、不刷"跳过"）；时长上限到点自动结束', async () => {
  const { store, sent, mgr, setClock, getClock } = makeWorld({ rng: () => 0 });
  await mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  setClock(getClock() + 200 * 1000);          // 超过 roundSeconds=150
  await mgr.tick();
  assert.equal(mgr.games.get('group:1')?.state.phase, 'vote', '描述阶段到点直接进投票');
  assert.match(sent.at(-1).msgs[0], /时间到|开始投票/);

  // 拨到超过 maxDurationMin（60 分钟）→ 自动收尾
  const g = mgr.games.get('group:1');
  if (g) setClock(g.deadlineAt + 1000);
  await mgr.tick();
  if (sent.at(-1)) assert.match(sent.at(-1).msgs[0], /时间到了|结束/);
  assert.equal(mgr.games.has('group:1'), false);
});

test('谁是卧底：第 2 轮没人描述也会到点进投票（轮次切换即计时，不卡在发言阶段）', async () => {
  // 回归 2026-09-29 审查 P1：nextRound 把 phaseStartedAt 置 0，onTick 里 `0 || now` 恒等 now，
  // 150 秒计时永远不开始 → 整局卡到 45 分钟上限
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
  // 没人描述 → 150 秒后直接进投票（真人群不按点名说话，不该出现"XX 没接上"）
  setClock(st2.phaseStartedAt + 200 * 1000);
  await mgr.tick();
  assert.equal(mgr.games.get('group:1').state.phase, 'vote');
  assert.match(sent.at(-1).msgs[0], /时间到|开始投票/);
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
  // 引擎私聊必须在发送时就标明 game-secret（发送端才是首次写库者，见 test/game-secret-prompt.test.mjs）
  assert.ok(onPrivs.every((x) => x.options?.eventKind === 'game-secret'), '引擎私聊要带 game-secret 标记');
  assert.ok(on.sent.filter((x) => x.chatKey === 'group:1').every((x) => x.options?.eventKind !== 'game-secret'), '群里公开消息不是 secret');
});

test('群里的"各种人"（老游戏）：非参与者投票不计；退出有退路；数字炸弹谁都能猜', async () => {
  // 谁是卧底：没参加的人投"投 3"不计票
  const w1 = makeWorld({ rng: () => 0.6 });
  await w1.mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  const order1 = w1.mgr.games.get('group:1').state.order;
  for (const uid of order1) say(w1.store, uid, `群友${uid.slice(1)}`, '日常用品');
  await w1.mgr.handleNewMessages('group:1');
  w1.sent.length = 0;
  say(w1.store, 'u99', '围观群众', '投 1');       // 局外人投票
  await w1.mgr.handleNewMessages('group:1');
  assert.equal(Object.keys(w1.mgr.games.get('group:1').state.votes || {}).length, 0, '局外人的票不计');

  // 谁是卧底：局内人说"不玩了"→ 移出本局并播报（身份不公布）
  const quitter = order1[0];
  say(w1.store, quitter, `群友${quitter.slice(1)}`, '不玩了');
  await w1.mgr.handleNewMessages('group:1');
  const st = w1.mgr.games.get('group:1')?.state;
  assert.ok(!st || st.eliminated.includes(quitter), '退出的人要从本局移出');
  assert.ok(w1.sent.some((x) => /退出/.test(x.msgs[0])), '群里要播报退出');

  // 数字炸弹：围观者也能猜（公共游戏，刻意的）——踩中照样结算
  const w2 = makeWorld({ rng: () => 0.42 });   // 炸弹固定 43
  await w2.mgr.start({ chatKey: 'group:1', gameId: 'number-bomb' });
  w2.sent.length = 0;
  say(w2.store, 'u99', '围观群众', '猜 43！');
  await w2.mgr.handleNewMessages('group:1');
  assert.equal(w2.mgr.games.has('group:1'), false, '围观者猜中也要结算');
  assert.match(w2.sent.at(-1).msgs[0], /踩中炸弹 43/);
});

test('私聊静默消耗：插件认领但不回执的消息，同样标记已读、不唤醒模型', async () => {
  // 7 人局才有守卫（6 人局由女巫替掉守卫，2026-09-29）
  const { store, sent, mgr } = makeWorld({ players: 7 });
  await mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  const state = mgr.games.get('group:1').state;
  const guard = state.roles.find((r) => r.role === 'guard');
  const villager = state.roles.find((r) => r.role === 'villager');
  sent.length = 0;
  // 先正常提交一次（有回执）
  const first = store.appendIncoming(`private:${guard.userId}`, { mid: 's1', ts: Date.now(), senderId: guard.userId, senderName: guard.name, text: `守 ${state.roles.findIndex((r) => r.userId === villager.userId) + 1}`, reply: null, media: [] });
  assert.equal(await mgr.consumePrivateAction(`private:${guard.userId}`, first), true);
  sent.length = 0;
  // 同目标重复提交 → 插件静默消耗：接管（true）、标记已读、但一条消息都不发
  const again = store.appendIncoming(`private:${guard.userId}`, { mid: 's2', ts: Date.now(), senderId: guard.userId, senderName: guard.name, text: first.text, reply: null, media: [] });
  assert.equal(await mgr.consumePrivateAction(`private:${guard.userId}`, again), true, '静默也要算"接管"，否则会唤起模型');
  assert.equal(sent.length, 0, '静默消耗不发任何消息');
  assert.equal(store.findByMid(`private:${guard.userId}`, 's2').state, 'acked', '同样要标记已读');
});

test('白天讨论时长可配：discussSeconds 传进插件（0=插件默认），到点由插件推进', async () => {
  const { mgr } = makeWorld({ players: 6 });
  // 默认 0 → 插件用自己的默认（120 秒）
  await mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(mgr.games.get('group:1').state.discussSeconds, 120);
  mgr.games.delete('group:1');
  // 配置 300 → 插件照做
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: false, dailyLimitPerChat: 6, discussSeconds: 300, recruitSeconds: 0, games: ['number-bomb', 'undercover', 'werewolf'] } });
  await mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(mgr.games.get('group:1').state.discussSeconds, 300);
});

test('报名制：够人才发牌（报名阶段不发任何私聊）、到点人不够就取消、显式名单跳过报名', async () => {
  // 1) 开报名：只发公告，不发身份私聊
  const w = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: false, dailyLimitPerChat: 6, recruitSeconds: 30, games: ['number-bomb', 'undercover', 'werewolf'] } });
  const r = await w.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.match(r.text, /报名/);
  const st = w.mgr.games.get('group:1').state;
  assert.equal(st.phase, 'recruiting');
  assert.equal(w.sent.filter((x) => x.chatKey.startsWith('private:')).length, 0, '报名阶段一条私聊都不能发');
  assert.ok(w.sent.some((x) => /报名中/.test(x.msgs[0])), '要发报名公告');

  // 2) 报名者（发"我玩/报名"）：够 6 人才发牌
  const ids = ['u1', 'u2', 'u3', 'u4', 'u5', 'u6'];
  for (const [i, uid] of ids.entries()) {
    say(w.store, uid, `群友${i + 1}`, i === 0 ? '我玩' : '报名');
    await w.mgr.tick();
    const cur = w.mgr.games.get('group:1')?.state;
    if (i < ids.length - 1) {
      assert.equal(cur.phase, 'recruiting', `还差 ${ids.length - 1 - i} 人，不能提前发牌`);
    }
  }
  const after = w.mgr.games.get('group:1').state;
  assert.notEqual(after.phase, 'recruiting', '够人就要发牌进局');
  assert.equal(after.roles.length, 6);
  assert.ok(w.sent.filter((x) => x.chatKey.startsWith('private:')).length >= 6, '发牌后才开始私聊发身份');

  // 3) 到点人不够 → 取消
  const w2 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: false, dailyLimitPerChat: 6, recruitSeconds: 20, games: ['number-bomb', 'undercover', 'werewolf'] } });
  await w2.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  say(w2.store, 'u1', '群友1', '我玩');
  await w2.mgr.tick();
  w2.setClock(w2.getClock() + 25 * 1000);
  await w2.mgr.tick();
  assert.equal(w2.mgr.games.has('group:1'), false, '人不够要取消并清状态');
  assert.ok(w2.sent.some((x) => /报名人数不够|这局先算了/.test(x.msgs[0])));

  // 4) 模型显式给名单 → 跳过报名，直接发牌（"就我们四个玩"）
  const w3 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: false, dailyLimitPerChat: 6, recruitSeconds: 30, games: ['number-bomb', 'undercover', 'werewolf'] } });
  const r3 = await w3.mgr.start({ chatKey: 'group:1', gameId: 'werewolf', players: ['群友1', '群友2', '群友3', '群友4', '群友5', '群友6'] });
  assert.equal(r3.ok, true, JSON.stringify(r3));
  assert.equal(w3.mgr.games.get('group:1').state.phase, 'night', '显式名单直接开局');
});

test('审查回归：games 白名单、maxPlayers 生效、报名默认 45、否定式不入选、报名中移出白名单即取消', async () => {
  // ① games 白名单：没勾狼人杀 → 拒绝 start（以前 UI 勾选是死控件）
  const w1 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, dailyLimitPerChat: 6, recruitSeconds: 0, games: ['number-bomb', 'undercover'] } });
  const denied = await w1.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(denied.ok, false);
  assert.match(denied.error, /控制台没被允许|没被允许/);

  // ② maxPlayers：控制台设 5 → 名单最多 5 人（用卧底：它的 state.roles 里能直接数名单；
  //     数字炸弹的 state 不保存名单，断言不了这件事，2026-09-29 审查 P2）
  const w2 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, dailyLimitPerChat: 6, recruitSeconds: 0, games: ['undercover'], maxPlayers: 5 } });
  const started2 = await w2.mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  assert.equal(started2.ok, true, JSON.stringify(started2));
  assert.equal(w2.mgr.games.get('group:1').state.roles.length, 5, 'maxPlayers 要真的截断名单（6 个活跃成员只发 5 张牌）');

  // ③ 报名默认值（键缺失 → 45）在 test/game-recruit-default.test.mjs 里单测（那边是干净配置）
  const w3 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, dailyLimitPerChat: 6, games: ['number-bomb', 'undercover', 'werewolf'], recruitSeconds: 45 } });
  await w3.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(w3.mgr.games.get('group:1').state.phase, 'recruiting');

  // ④ 报名：常见说法都认（我玩/我也玩/我参加/我来/算我一个），否定式不认
  say(w3.store, 'u1', '群友1', '别带我，你们玩');
  say(w3.store, 'u2', '群友2', '我不参与');
  say(w3.store, 'u3', '群友3', '我玩');
  say(w3.store, 'u4', '群友4', '我也玩');
  say(w3.store, 'u5', '群友5', '我参加');
  say(w3.store, 'u6', '群友6', '我来');
  await w3.mgr.tick();
  const st = w3.mgr.games.get('group:1')?.state;
  // 够 6 人（u3,u4,u5,u6 + u1? 不）——u1/u2 被排除，只有 4 人 → 还在报名
  assert.ok(st && st.phase === 'recruiting', '人数不够应继续报名（u1/u2 不算）');
  const joiners = st.joiners.map((j) => j.userId).sort();
  assert.deepEqual(joiners, ['u3', 'u4', 'u5', 'u6'], '明确要玩的 4 人算报名：' + JSON.stringify(joiners));

  // ⑤ 报名中把群移出白名单 → 报名取消（不发身份私聊）
  updateConfig({ groupGame: { enabled: true, chats: [], allowPrivateInvite: true, dailyLimitPerChat: 6, games: ['number-bomb', 'undercover', 'werewolf'], recruitSeconds: 45 } });
  await w3.mgr.tick();
  assert.equal(w3.mgr.games.has('group:1'), false, '白名单外的报名要取消');
  assert.ok(w3.sent.some((x) => /报名取消/.test(x.msgs[0])));
  assert.equal(w3.sent.filter((x) => x.chatKey.startsWith('private:')).length, 0, '取消前也没发过私聊');
});

test('审查回归：新局不重放上一局的历史私聊（私聊水位在发牌时初始化）', async () => {
  const w = makeWorld({ players: 6 });
  // 上一局留下的历史私聊（含像行动的内容）
  w.store.appendIncoming('private:u1', { mid: 'old-1', ts: Date.now() - 60000, senderId: 'u1', senderName: '群友1', text: '刀 2', reply: null, media: [] }, { recordOnly: true });
  w.store.appendIncoming('private:u1', { mid: 'old-2', ts: Date.now() - 59000, senderId: 'u1', senderName: '群友1', text: '不玩了', reply: null, media: [] }, { recordOnly: true });
  await w.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  w.sent.length = 0;
  await w.mgr.tick();
  await w.mgr.tick();
  const acted = w.sent.filter((x) => x.chatKey === 'private:u1' && /已记下|退出|查验/.test(x.msgs[0]));
  assert.equal(acted.length, 0, '新局不能把上一局的历史私聊当成本局行动：' + JSON.stringify(acted.map((x) => x.msgs[0])));
  assert.equal(w.mgr.games.get('group:1').state.roles.length, 6, '也不该被历史"不玩了"踢出人');
});

test('人满/开局后还有人报名：给一句"来晚了"的提示（同一人只提一次）', async () => {
  // ① 报名阶段满员：maxPlayers=4（卧底 minPlayers=4）→ 第 5 个报名者收到"来晚了一步"
  const w1 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, dailyLimitPerChat: 6, recruitSeconds: 30, games: ['undercover'], maxPlayers: 4 } });
  await w1.mgr.start({ chatKey: 'group:1', gameId: 'undercover' });
  for (const uid of ['u1', 'u2', 'u3', 'u4', 'u5']) say(w1.store, uid, `群友${uid.slice(1)}`, '我玩');
  await w1.mgr.tick();
  assert.ok(w1.sent.some((x) => /来晚了一步|报满/.test(x.msgs[0])), '满员后要有人被回绝：' + JSON.stringify(w1.sent.map((x) => x.msgs[0])));
  assert.ok(w1.sent.some((x) => /来晚/.test(x.msgs[0]) && /群友5/.test(x.msgs[0])), '要指名道姓说清是谁晚了');

  // ② 开局之后才来报：也提示一次，且不重复
  const w2 = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, dailyLimitPerChat: 6, recruitSeconds: 0, games: ['werewolf'] } });
  await w2.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  w2.sent.length = 0;
  say(w2.store, 'u99', '路人甲', '我玩');   // 局外人（不在名单里）
  await w2.mgr.tick();
  assert.equal(w2.sent.filter((x) => /来晚/.test(x.msgs[0])).length, 1, '来晚了要提示一次');
  say(w2.store, 'u99', '路人甲', '我也来');
  await w2.mgr.tick();
  assert.equal(w2.sent.filter((x) => /来晚/.test(x.msgs[0])).length, 1, '同一人不重复提示');
  // 在册玩家说"我玩"不该被提示
  const inside = w2.mgr.games.get('group:1').state.roles[0].userId;
  say(w2.store, inside, '局内人', '我玩');
  await w2.mgr.tick();
  assert.equal(w2.sent.filter((x) => /来晚/.test(x.msgs[0])).length, 1, '在册玩家不该被当成迟到的');
});

test('审查回归：报名消息还是 pending（模型那边在飞）也必须能报上名', async () => {
  // 以前报名扫描带 readOnly（== 只看 acked），同一群里正好有一次模型运行在飞时，
  // 这批「我玩」对引擎不可见 → 45 秒到点直接"人数不够"（2026-09-29 审查 P2）
  const w = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: false, dailyLimitPerChat: 6, recruitSeconds: 30, games: ['werewolf'] } });
  await w.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  // 关键：不用 recordOnly → 落库是 pending（模拟"编排器还没跑完这一批"）
  for (let i = 1; i <= 6; i += 1) {
    w.store.appendIncoming('group:1', { mid: 700 + i, ts: Date.now(), senderId: `u${i}`, senderName: `群友${i}`, text: '我玩', reply: null, media: [] });
  }
  await w.mgr.tick();
  const st = w.mgr.games.get('group:1')?.state;
  assert.equal(st?.phase !== 'recruiting', true, '待处理的报名也要算数：' + JSON.stringify(st?.phase));
  assert.equal(st.roles.length, 6, '6 个人都要进名单');
});

test('审查回归：白天讨论/投票消息是 pending 时也要计票', async () => {
  const w = makeWorld({ players: 6 });
  updateConfig({ groupGame: { enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: false, dailyLimitPerChat: 6, recruitSeconds: 0, discussSeconds: 0, games: ['werewolf'] } });
  const r = await w.mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(r.ok, true, JSON.stringify(r));
  const st0 = w.mgr.games.get('group:1').state;
  // 发牌即夜间，而夜里群里说什么都不参与判定 → 先把第 1 夜（全员 AFK）推过去
  w.setClock(w.getClock() + 95 * 1000);
  await w.mgr.tick();
  const st = w.mgr.games.get('group:1').state;
  assert.equal(st.phase, 'day', '超时结算后应进入白天：' + st.phase);
  const target = st.roles[0];
  for (const m of st.roles) {
    w.store.appendIncoming('group:1', { mid: `v-${m.userId}`, ts: Date.now(), senderId: m.userId, senderName: m.name, text: `投 ${st.roles.indexOf(target) + 1}`, reply: null, media: [] });
  }
  await w.mgr.tick();
  const votes = w.mgr.games.get('group:1')?.state?.votes || {};
  assert.ok(Object.keys(votes).length >= 5, '待处理状态的票也要记下来：' + JSON.stringify(votes));
});
