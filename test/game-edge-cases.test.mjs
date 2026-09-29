// 群游戏的"复杂情况"组合测试（狼人杀为主）：把单条规则组合起来跑，覆盖单测没连起来看的路径。
// 覆盖：保护矩阵四组合、跨三夜的用药链条、刀口锁定后的改主意、女巫不回复的超时、
// 白天两级 AFK 超时、守卫连守被拒、9 人局 3 狼平票、出局者的私聊侧、
// deny 优先于游戏私聊豁免、局结束后私聊不再被引擎接管、双死 + 不公开身份。
// 纯插件/框架调用（真 ChatStore + 桩 sender），不连任何服务。
import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-game-edge-'));
process.env.QQ_AGENT_DATA_DIR = dataDir;
fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { groups: ['1'], private: [] },
  deny: { groups: [], private: [] },
  api: { baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'mock' },
  groupGame: {
    enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: true,
    dailyLimitPerChat: 6, recruitSeconds: 0, games: ['number-bomb', 'undercover', 'werewolf'], discussSeconds: 0
  }
}));

const wolf = await import('../src/features/games/werewolf.js');
const { assertCanSend } = await import('../src/core/access.js');
const { updateConfig } = await import('../src/core/config.js');
const { GroupGameManager } = await import('../src/features/group-game.js');
const { ChatStore } = await import('../src/core/store.js');

// ── 插件级 helper ────────────────────────────────────────────────────────
const P = (n) => Array.from({ length: n }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
const by = (s, role) => s.roles.filter((r) => r.role === role);
const idx = (s, uid) => s.roles.findIndex((r) => r.userId === uid) + 1;
const pm = (s, uid, text, now = 1000) => wolf.onPrivateMessage(s, { userId: uid, text, ts: now }, { now, rng: () => 0 });
const tickAt = (s, now) => wolf.onTick(s, { now, rng: () => 0 });
const game = (n, extra = {}) => wolf.create({ players: P(n), rng: () => 0, now: 1000, ...extra });
// 注意：普通效果是 text，结算/终局效果是 result —— 两种都要认，否则结算文本会被当成空串
const texts = (effects) => effects.map((e) => e.text ?? e.result ?? '').join(' | ');
const aliveIds = (s) => s.roles.filter((r) => r.alive).map((r) => r.userId);
const hasRoleWord = (t) => /预言家|守卫|女巫|平民/.test(t) || /狼人(?!杀)/.test(t);

// ── 1. 保护矩阵 ─────────────────────────────────────────────────────────
test('保护矩阵：守+救同一人=死；只守=活；只救=活；都不管=死（四种组合一次跑完）', () => {
  const cases = [
    { guard: true, heal: true, alive: false, why: '同守同救必死' },
    { guard: true, heal: false, alive: true, why: '只守不救要活' },
    { guard: false, heal: true, alive: true, why: '只救不守要活' },
    { guard: false, heal: false, alive: false, why: '没人管就死' }
  ];
  for (const c of cases) {
    let s = game(7);
    const [w1, w2] = by(s, 'wolf');
    const seer = by(s, 'seer')[0];
    const guard = by(s, 'guard')[0];
    const witch = by(s, 'witch')[0];
    s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
    s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;         // 刀口定下 → 女巫被问
    const guardTo = c.guard ? seer : guard;
    s = pm(s, guard.userId, `守 ${idx(s, guardTo.userId)}`).state;
    s = pm(s, witch.userId, c.heal ? '救' : '不救').state;
    const out = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`);        // 收齐 → 结算
    const tag = `守=${c.guard} 救=${c.heal}（${c.why}）`;
    assert.equal(out.state.roles.find((r) => r.userId === seer.userId).alive, c.alive, tag);
    assert.equal(out.state.potions.heal, !c.heal, `解药只在真的用了时才扣：${tag}`);
    assert.equal(out.state.nightLog.at(-1).dead.length, c.alive ? 0 : 1, `夜晚记录的真实出局名单：${tag}`);
  }
});

// ── 2. 跨三夜的链条 ─────────────────────────────────────────────────────
test('跨三夜：第 1 夜救人 → 第 2 夜改毒（双死）→ 第 3 夜无药的女巫被刀也不卡夜，之后不再被问', () => {
  let s = game(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  const [va, vb] = by(s, 'villager');
  // 一夜的行动：狼两只 + 守卫 + （女巫）+ 预言家（全部交齐才会立刻结算）
  const night = (st, { kill, guardTo, witchCmd, check }) => {
    let x = pm(st, w1.userId, `刀 ${idx(st, kill.userId)}`).state;
    x = pm(x, w2.userId, `刀 ${idx(x, kill.userId)}`).state;
    x = pm(x, guard.userId, `守 ${idx(x, guardTo.userId)}`).state;
    if (witchCmd) x = pm(x, witch.userId, witchCmd).state;
    return pm(x, seer.userId, `查 ${idx(x, check.userId)}`);
  };
  const skipDay = (st) => {
    const toVote = tickAt(st, st.phaseStartedAt + (Number(st.discussSeconds) + 1) * 1000).state;
    assert.equal(toVote.phase, 'vote', '讨论到点进投票');
    return tickAt(toVote, toVote.phaseStartedAt + 91 * 1000);   // 投票窗口到点、没人投 → 下一夜
  };

  // 第 1 夜：狼刀预言家、守卫守村民（避开同守同救）、女巫救预言家 → 平安夜
  let out = night(s, { kill: seer, guardTo: va, witchCmd: '救', check: w1 });
  s = out.state;
  assert.equal(s.roles.find((r) => r.userId === seer.userId).alive, true, '救活了');
  assert.match(texts(out.effects), /平安夜/);
  assert.equal(s.potions.heal, false, '解药已用');

  out = skipDay(s);
  assert.match(texts(out.effects), /没人投票|直接进入下一夜/);
  s = out.state;
  assert.equal(s.night, 2);
  assert.equal(s.potions.heal, false, '解药不会因为跨天回血');

  // 第 2 夜：狼刀守卫、守卫守自己（挡刀）、女巫毒村民 A → 只死村民 A（一死一夜）
  out = night(s, { kill: guard, guardTo: guard, witchCmd: `毒 ${idx(s, va.userId)}`, check: w2 });
  s = out.state;
  assert.deepEqual(aliveIds(s).includes(va.userId), false, '被毒的村民出局');
  assert.equal(s.roles.find((r) => r.userId === guard.userId).alive, true, '被守的守卫活着');
  assert.equal(s.potions.poison, false, '毒药已用');
  assert.match(texts(out.effects), /玩家\d 昨晚倒牌/, '天亮要报出局者');
  assert.match(wolf.summaryForModel(s), /昨夜 \d+ 号出局/, '摘要口径与播报一致');

  out = skipDay(s);
  s = out.state;
  assert.equal(s.night, 3);

  // 第 3 夜：狼刀女巫（她两瓶药都没了、不能自救）→ 不必等她回话、其余交齐就结算
  out = night(s, { kill: witch, guardTo: seer, witchCmd: null, check: w1 });
  s = out.state;
  assert.equal(s.roles.find((r) => r.userId === witch.userId).alive, false, '没药的女巫被刀要出局');
  assert.match(texts(out.effects), /倒牌/);
  assert.equal(s.nightLog.length, 3, '三夜都记了账');

  // 进第 4 夜：行动提示只发给活着的人（死掉的女巫不再被问）
  const night4 = skipDay(s);
  assert.equal(night4.state.night, 4, '推进到第 4 夜');
  const n4 = night4.effects.filter((e) => e.type === 'private' && /第 4 夜/.test(e.text));
  assert.equal(n4.some((e) => e.userId === witch.userId), false, '死人不该再收到夜行动提示');
  assert.equal(n4.length, aliveIds(night4.state).filter((uid) => {
    const r = night4.state.roles.find((x) => x.userId === uid);
    return ['wolf', 'seer', 'guard'].includes(r.role);
  }).length, '活着需要行动的人各一条');
});

// ── 3. 刀口锁定后的改主意 ───────────────────────────────────────────────
test('刀口锁定链：狼改刀 → 交齐锁定 → 女巫救 → 狼再改被拒 → 女巫改毒 → 结算用锁定刀口', () => {
  let s = game(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  const va = by(s, 'villager')[0];
  // 只有一只狼交的时候还能改
  s = pm(s, w1.userId, `刀 ${idx(s, va.userId)}`).state;
  const changed = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`);
  assert.match(texts(changed.effects), /已记下你的刀口/, '未锁定时允许改');
  s = changed.state;
  const asked = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`);     // 两只狼都交 → 锁定 + 问女巫
  assert.match(texts(asked.effects), /女巫行动：今晚被刀的是/, '刀口定下要告诉女巫');
  s = asked.state;
  const rejected = pm(s, w1.userId, `刀 ${idx(s, va.userId)}`);
  assert.match(texts(rejected.effects), /刀口已经定下/, '锁定后不能再改');
  s = pm(rejected.state, guard.userId, `守 ${idx(s, guard.userId)}`).state;
  s = pm(s, witch.userId, '救').state;                             // 先救
  const toPoison = pm(s, witch.userId, `毒 ${idx(s, va.userId)}`); // 再改成毒（一晚一瓶）
  assert.match(texts(toPoison.effects), /毒/);
  const out = pm(toPoison.state, seer.userId, `查 ${idx(s, w1.userId)}`);
  // 一晚只用一瓶：最后选的是毒 → 解药没生效，锁定的刀口照样出局；毒药另外带走一个
  assert.equal(out.state.roles.find((r) => r.userId === seer.userId).alive, false, '改成毒之后没有人救被刀的人');
  assert.equal(out.state.roles.find((r) => r.userId === va.userId).alive, false, '毒药带走村民');
  assert.match(texts(out.effects), /玩家\d+、玩家\d+ 昨晚倒牌|玩家\d+ 昨晚倒牌/, '双死之夜要一次报全');
  assert.equal(out.state.potions.heal, true, '改主意后解药不扣');
  assert.equal(out.state.potions.poison, false, '最终用的是毒药');
  assert.equal(out.state.nightLog.at(-1).wolf, idx(s, seer.userId), '夜晚记录用的是锁定刀口');
});

// ── 4. 女巫不回复 → 到点按"没用药"结算 ─────────────────────────────────
test('女巫一直不回复：夜行动窗口到点就按已收到的结算（不救），药水不扣', () => {
  let s = game(6);
  const [w1, w2] = by(s, 'wolf');
  const va = by(s, 'villager')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, va.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, va.userId)}`).state;   // 锁刀 → 问女巫（她不理）
  s = pm(s, by(s, 'seer')[0].userId, `查 ${idx(s, w1.userId)}`).state;
  const out = tickAt(s, s.phaseStartedAt + 91 * 1000);
  assert.equal(out.state.roles.find((r) => r.userId === va.userId).alive, false, '没人救 → 被刀者出局');
  assert.equal(out.state.potions.heal, true, '没交行动不算用药');
  assert.equal(out.state.potions.poison, true);
});

// ── 5. 白天两级 AFK 超时 ────────────────────────────────────────────────
test('白天没人说话 + 没人投票：讨论到点进投票、投票到点直接进下一夜（不卡、不点名）', () => {
  let s = game(6);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, by(s, 'witch')[0].userId, '救').state;
  s = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`).state;    // 平安夜 → 白天
  assert.equal(s.phase, 'day');
  const toVote = tickAt(s, s.phaseStartedAt + (Number(s.discussSeconds) + 1) * 1000);
  assert.equal(toVote.state.phase, 'vote');
  assert.match(texts(toVote.effects), /到，开始投票/);
  assert.equal(/没接上|轮到/.test(texts(toVote.effects)), false, '不催人、不点名');
  const next = tickAt(toVote.state, toVote.state.phaseStartedAt + 91 * 1000);
  assert.equal(next.state.night, 2, '没人投票也要推进到下一夜');
  assert.match(texts(next.effects), /没人投票|直接进入下一夜/);
});

// ── 6. 守卫连守被拒 ─────────────────────────────────────────────────────
test('守卫不能连着两晚守同一个人：第 2 夜会被拒，换人成功', () => {
  let s = game(7);
  const [w1, w2] = by(s, 'wolf');
  const guard = by(s, 'guard')[0];
  const seer = by(s, 'seer')[0];
  const va = by(s, 'villager')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, guard.userId)}`).state;
  s = pm(s, by(s, 'witch')[0].userId, '不救').state;
  s = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`).state;
  assert.equal(s.phase, 'day');
  const toVote = tickAt(s, s.phaseStartedAt + (Number(s.discussSeconds) + 1) * 1000).state;
  s = tickAt(toVote, toVote.phaseStartedAt + 91 * 1000).state;
  assert.equal(s.night, 2);
  const refused = pm(s, guard.userId, `守 ${idx(s, guard.userId)}`);
  assert.match(texts(refused.effects), /不能连着两晚守同一个人/, '连守要被拒');
  const ok = pm(refused.state, guard.userId, `守 ${idx(s, va.userId)}`);
  assert.match(texts(ok.effects), /已记下：今晚守/, '换人要成功');
});

// ── 7. 9 人局 3 狼：平票按 rng 落定，且与"告诉女巫的刀口"一致 ──────────
test('9 人局 3 狼：三狼各刀一个（平票）→ 按 rng 落定，女巫被告知与结算用同一目标', () => {
  let s = wolf.create({ players: P(9), rng: () => 0, now: 1000 });
  assert.equal(by(s, 'wolf').length, 3);
  const [w1, w2, w3] = by(s, 'wolf');
  const targets = by(s, 'villager');
  // 三狼各投不同的人 → 三票平票；rng=0.99 → 取并列里的最后一个
  s = pm(s, w1.userId, `刀 ${idx(s, targets[0].userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, targets[1].userId)}`).state;
  const locked = wolf.onPrivateMessage(
    s, { userId: w3.userId, text: `刀 ${idx(s, targets[2].userId)}`, ts: 1000 }, { now: 1000, rng: () => 0.99 }
  );
  const told = locked.effects.find((e) => e.userId === by(s, 'witch')[0].userId && /女巫行动/.test(e.text));
  assert.ok(told, '刀口定下要问女巫');
  const expectIdx = Math.max(...targets.map((t) => idx(s, t.userId)));
  assert.match(told.text, new RegExp(`被刀的是 ${expectIdx} 号`), '告诉女巫的刀口要与 rng 落定的一致：' + told.text);
  let x = locked.state;
  x = pm(x, by(x, 'guard')[0].userId, `守 ${idx(x, by(x, 'guard')[0].userId)}`).state;
  x = pm(x, by(x, 'witch')[0].userId, '不救').state;
  const out = pm(x, by(x, 'seer')[0].userId, `查 ${idx(x, w1.userId)}`);
  assert.equal(out.state.nightLog.at(-1).wolf, expectIdx, '结算用的也是同一个刀口');
  assert.equal(out.state.roles.find((r) => r.userId === targets[2].userId).alive, false);
});

// ── 8. 出局者的私聊侧 ───────────────────────────────────────────────────
test('出局者私聊：行动被明确拒绝、也不能再"退出"，且不产生任何状态变化', () => {
  let s = game(6);
  const [w1, w2] = by(s, 'wolf');
  const victim = by(s, 'villager')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, victim.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, victim.userId)}`).state;
  s = pm(s, by(s, 'witch')[0].userId, '不救').state;
  s = pm(s, by(s, 'seer')[0].userId, `查 ${idx(s, w1.userId)}`).state;
  assert.equal(s.roles.find((r) => r.userId === victim.userId).alive, false, '被刀的村民出局');
  // 白天私聊是正常聊天（引擎不碰），所以要在夜里测"出局者提交行动"
  const toVote = tickAt(s, s.phaseStartedAt + (Number(s.discussSeconds) + 1) * 1000).state;
  s = tickAt(toVote, toVote.phaseStartedAt + 91 * 1000).state;
  assert.equal(s.phase, 'night', '推进到下一夜');
  const strip = (x) => JSON.stringify({ ...x, ackCount: null });   // 回执计数是防刷屏用的，按设计要涨
  const before = strip(s);
  const act = pm(s, victim.userId, `刀 ${idx(s, w1.userId)}`);
  assert.match(texts(act.effects), /已经出局/, '出局者夜里提交行动要被明确拒绝');
  assert.equal(strip(act.state), before, '出局者的话不得改动局面（角色/行动/药水/票/阶段一个都不变）');
  assert.equal(act.state.ackCount[victim.userId], 1, '被拒的消息照样计数，防刷屏');
  const quit = pm(s, victim.userId, '不玩了');
  assert.match(texts(quit.effects), /本来就不在局里|已经出局/, '出局者再退出要有回应');
  assert.equal(texts(quit.effects).includes('退出了本局'), false, '出局者不该再触发一次"退出了本局"播报');
});

// ── 9. deny 优先于游戏私聊豁免 ──────────────────────────────────────────
test('deny 优先于游戏私聊豁免：被管理员屏蔽的人在册也发不进去；豁免只管私聊白名单', () => {
  const setCfg = (patch) => updateConfig({ allow: { groups: ['1'], private: [] }, deny: { groups: [], private: [] }, ...patch });
  const uid = '100000001';
  // ① 不在白名单、没豁免 → 拦
  setCfg({});
  assert.throws(() => assertCanSend(`private:${uid}`, null, {}), /not allowed/, '白名单外要拦');
  // ② 不在白名单、带游戏豁免 → 放行
  assert.doesNotThrow(() => assertCanSend(`private:${uid}`, null, { gameScoped: true }), '游戏豁免要放行');
  // ③ 在 deny 名单、带豁免 → 仍然拦（deny 优先）
  setCfg({ deny: { groups: [], private: [uid] } });
  assert.throws(() => assertCanSend(`private:${uid}`, null, { gameScoped: true }), /屏蔽/, 'deny 必须优先');
  // ④ 豁免不改变群白名单
  setCfg({});
  assert.throws(() => assertCanSend('group:999', null, { gameScoped: true }), /not allowed/, '群白名单不受豁免影响');
  setCfg({});
});

// ── 10. 局结束后私聊不再被接管 ──────────────────────────────────────────
test('管理员 stop 之后：原在册玩家的私聊不再被引擎接管，"投 X"也不再生效', async () => {
  fs.rmSync(path.join(dataDir, 'games.json'), { force: true });
  updateConfig({
    groupGame: {
      enabled: true, chats: ['group:1'], allowPrivateInvite: true, allowGamePrivateDm: true,
      dailyLimitPerChat: 6, recruitSeconds: 0, games: ['werewolf'], maxPlayers: 10, discussSeconds: 0
    }
  });
  const store = new ChatStore(0, { dataDir, filename: `edge-${Math.random().toString(36).slice(2)}.sqlite` });
  for (let i = 1; i <= 6; i += 1) {
    store.appendIncoming('group:1', { mid: 900 + i, ts: Date.now() - i * 1000, senderId: `u${i}`, senderName: `群友${i}`, text: '在' }, { recordOnly: true });
  }
  const sent = [];
  const sender = { sendTextBatch: async (chatKey, msgs, options = {}) => { sent.push({ chatKey, msgs: [...msgs], options }); return { message_id: sent.length }; } };
  const mgr = new GroupGameManager({ store, sender, log: () => {}, now: () => Date.now(), rng: () => 0 });
  const started = await mgr.start({ chatKey: 'group:1', gameId: 'werewolf' });
  assert.equal(started.ok, true, JSON.stringify(started));
  const stopped = await mgr.stop('group:1', '模拟管理员中止');
  assert.equal(stopped.ok, true);
  assert.equal(mgr.games.has('group:1'), false);
  // 私聊：不再接管
  const row = store.appendIncoming('private:u1', { mid: 'p-after-stop', ts: Date.now(), senderId: 'u1', senderName: '群友1', text: '刀 2', reply: null, media: [] });
  assert.equal(await mgr.consumePrivateAction('private:u1', row), false, '局结束后私聊要回到普通聊天');
  // 群里：没人认领
  store.appendIncoming('group:1', { mid: 'g-after-stop', ts: Date.now(), senderId: 'u1', senderName: '群友1', text: '投 2' }, { recordOnly: true });
  const before = sent.length;
  await mgr.handleNewMessages('group:1');
  assert.equal(sent.length, before, '局结束后群里发「投 2」不该有任何反应');
  store.close();
});

// ── 11. 双死 + 不公开身份 ───────────────────────────────────────────────
test('双死之夜 + reveal=false：公开文本不出现身份词，结算只报胜方（但人数口径仍然对）', () => {
  let s = game(7, { reveal: false });
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  const [va, vb] = by(s, 'villager');
  s = pm(s, w1.userId, `刀 ${idx(s, guard.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, guard.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, va.userId)}`).state;       // 守错人 → 守卫被刀
  s = pm(s, witch.userId, '不救').state;
  const poison = pm(s, witch.userId, `毒 ${idx(s, vb.userId)}`);
  const dawn = pm(poison.state, seer.userId, `查 ${idx(s, w1.userId)}`);
  const line = texts(dawn.effects.filter((e) => e.type === 'public'));   // 私聊（查验结果/出局通知）本来就不公开
  assert.match(line, /倒牌/, '有人出局');
  assert.equal(hasRoleWord(line), false, '公开播报不得出现身份词：' + line);
  assert.equal(hasRoleWord(wolf.summaryForModel(dawn.state)), false, '摘要不得出现身份词');
  const deadNow = aliveIds(dawn.state);
  assert.equal(deadNow.includes(guard.userId), false, '被刀的守卫出局');
  assert.equal(deadNow.includes(vb.userId), false, '被毒的村民出局');
  // 结算：把两只狼都"退赛"→ 好人胜；reveal=false 的结算文本只报胜方、不列身份与夜晚记录
  let x = dawn.state;
  const endEffects = [];
  for (const w of [w1, w2]) {
    if (!x.roles.find((r) => r.userId === w.userId).alive) continue;
    const q = pm(x, w.userId, '我不玩了');
    endEffects.push(...q.effects);
    x = q.state;
  }
  assert.equal(x.phase, 'ended', '狼退光要立刻结束');
  const endLine = texts(endEffects);
  assert.match(endLine, /好人获胜/, '要报胜方：' + endLine);
  assert.equal(/身份：/.test(endLine), false, 'reveal=false 不得列身份：' + endLine);
  assert.equal(/夜晚记录/.test(endLine), false, 'reveal=false 不得列夜晚记录：' + endLine);
  assert.equal(hasRoleWord(endLine), false, '结算也不得出现身份词：' + endLine);
});
