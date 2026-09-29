// 狼人杀插件规则测试（模拟环境：纯插件调用，不连任何服务、不发消息）。
// 覆盖：角色表、夜行动收集（即时结算 / 超时结算）、守卫挡刀、查验只达本人、
// 狼队多数、白天发言与投票、胜负判定、夜数上限平局、公开摘要不泄密。
import assert from 'node:assert/strict';
import test from 'node:test';
import * as wolf from '../src/features/games/werewolf.js';

// "狼人杀"这个游戏名里含"狼人"，判"有没有泄身份"时要把它排除掉
const hasRoleWord = (text) => /预言家|守卫|女巫|平民/.test(text) || /狼人(?!杀)/.test(text);

const PLAYERS = Array.from({ length: 6 }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
const by = (s, role) => s.roles.filter((r) => r.role === role);
const idx = (s, uid) => s.roles.findIndex((r) => r.userId === uid) + 1;
const pm = (s, uid, text, now = 1) => wolf.onPrivateMessage(s, { userId: uid, text, ts: now }, { now, rng: () => 0 });

/** 造一个可复现的局（rng 固定 → 洗牌结果固定）。7 人局才有守卫（6 人=狼/狼/预/女巫/民/民）。 */
function newGame(size = 6) {
  const players = Array.from({ length: size }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
  return wolf.create({ players, rng: () => 0, now: 1000 });
}

test('角色表与规模：6 人=2 狼/1 预/1 女巫/2 民；7 人起带守卫；9 人=3 狼', () => {
  const g6 = newGame(6);
  assert.equal(by(g6, 'wolf').length, 2);
  assert.equal(by(g6, 'seer').length, 1);
  assert.equal(by(g6, 'witch').length, 1, '6 人局用女巫替守卫');
  assert.equal(by(g6, 'guard').length, 0);
  assert.equal(by(g6, 'villager').length, 2);
  const g7 = newGame(7);
  assert.equal(by(g7, 'witch').length, 1);
  assert.equal(by(g7, 'guard').length, 1, '7 人起守卫与女巫同时在');
  const g9 = newGame(9);
  assert.equal(by(g9, 'wolf').length, 3);
  assert.equal(by(g9, 'witch').length, 1);
  assert.equal(g9.roles.length, 9);
});

test('开局效果：公开公告只有名单，身份各自私聊（狼队互相可见）', () => {
  const g = newGame();
  const eff = wolf.openingEffects(g);
  const pub = eff.filter((e) => e.type === 'public');
  assert.equal(pub.length, 1);
  assert.match(pub[0].text, /狼人杀开局/);
  assert.equal(hasRoleWord(pub[0].text), false, '公开公告不得出现身份词');
  const priv = eff.filter((e) => e.type === 'private');
  // 6 人：身份 6 条 + 夜行动提示（狼 2 + 预言家 1 + 女巫 1；6 人局没有守卫）
  assert.equal(priv.filter((e) => /你是\*\*/.test(e.text)).length, 6);
  const dealWolf = priv.find((e) => /你是\*\*狼人\*\*/.test(e.text));
  const wolfNames = by(g, 'wolf').filter((w) => w.userId !== dealWolf.userId).map((w) => w.name);
  assert.ok(wolfNames.every((n) => dealWolf.text.includes(n)), '狼的 Deal 里要写明队友');
  const prompts = priv.filter((e) => /第 1 夜·/.test(e.text));   // 别用"行动"筛：身份文案里也有这个词
  assert.equal(prompts.length, 4, '2 狼+预言家+女巫各一条夜行动提示');
});

test('夜晚：收齐即结算；守卫挡刀=平安夜；查验结果只发给预言家本人（7 人局含守卫）', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  const villager = by(s, 'villager')[0];

  // 平民夜里没有行动：给一句明确回执，不推进
  const vOut = pm(s, villager.userId, '我睡觉了');
  assert.match(vOut.effects[0].text, /夜里你没有行动/);
  assert.equal(vOut.state.phase, 'night');

  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  assert.equal(s.phase, 'night', '还没收齐，不能提前结算');
  const wolf2 = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`);     // 狼交齐 → 刀口定下 + 立刻问女巫
  s = wolf2.state;
  assert.ok(wolf2.effects.some((e) => /女巫行动/.test(e.text) && /号/.test(e.text)), '狼刀定了要给女巫带刀口的问话');
  s = pm(s, guard.userId, `守 ${idx(s, seer.userId)}`).state;      // 守卫也守了同一个人
  const witchAsk = pm(s, witch.userId, '不救');                    // 女巫不用药
  assert.ok(witchAsk.effects.length >= 1, '女巫要收到回执');
  s = witchAsk.state;
  const out = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`);       // 最后一个行动 → 立即结算

  const texts = out.effects.map((e) => e.text);
  assert.ok(texts.some((t) => /你是「狼人」|是「狼人」/.test(t)), '预言家要拿到查验结果');
  const seerResult = out.effects.filter((e) => e.type === 'private' && /查验结果/.test(e.text));
  assert.equal(seerResult.length, 1);
  assert.equal(seerResult[0].userId, seer.userId, '查验结果只能发给预言家');
  assert.ok(!out.effects.some((e) => e.type === 'public' && hasRoleWord(e.text)), '公开消息不得出现身份词');
  assert.ok(texts.some((t) => /平安夜/.test(t)), '守卫守中狼刀 → 平安夜');
  assert.equal(out.state.phase, 'day');
  assert.equal(out.state.roles.filter((r) => r.alive).length, 7, '7 人局平安夜 → 一个没少');
});

test('夜晚超时：用已收到的行动结算，缺的当夜空过（无人被刀 → 平安夜）', () => {
  let s = newGame();
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;   // 只有预言家行动
  const out = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 });
  assert.equal(out.state.phase, 'day');
  assert.ok(out.effects.some((e) => /平安夜/.test(e.text)));
});

test('白天：按顺序发言 → 投票淘汰 → 狼全灭判好人胜；摘要与公开消息不泄密', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const guard = by(s, 'guard')[0];
  const seer = by(s, 'seer')[0];
  const witch = by(s, 'witch')[0];
  const villagers = by(s, 'villager');

  // 第 1 夜：狼刀村民 A，守卫守村民 B（无效），预言家查狼 1
  const va = villagers[0];
  const vb = villagers[1];
  s = pm(s, w1.userId, `刀 ${idx(s, va.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, va.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, vb.userId)}`).state;
  s = pm(s, witch.userId, '不救').state;
  const night1 = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`);
  assert.match(night1.effects.find((e) => /查验结果/.test(e.text)).text, /狼人/);
  s = night1.state;
  assert.equal(s.roles.find((r) => r.userId === va.userId).alive, false, '被刀的村民出局');
  assert.match(wolf.summaryForModel(s), /出局/);
  assert.equal(hasRoleWord(wolf.summaryForModel(s)), false, '公开摘要不得出现身份词');

  // 第 1 天：全员发言 → 投票淘汰狼 1
  let out = { state: s, effects: [] };
  for (const uid of [...out.state.order]) {
    out = wolf.onMessage(out.state, { userId: uid, text: '我怀疑 3 号', ts: 2 }, { now: 2 });
  }
  assert.equal(out.state.phase, 'vote');
  for (const uid of out.state.order) {
    // 被投的那个人不能投自己（会被拒），改投别人
    const target = uid === w1.userId ? by(out.state, 'villager').find((v) => v.alive) || by(out.state, 'seer')[0] : w1;
    out = wolf.onMessage(out.state, { userId: uid, text: `投 ${idx(out.state, target.userId)}`, ts: 3 }, { now: 3 });
  }
  assert.equal(out.state.roles.find((r) => r.userId === w1.userId).alive, false, '狼 1 被投出');
  assert.equal(out.state.phase, 'night', '还有狼活着 → 进下一夜');
  // 夜里行动提示只发给活着的人（出局那一刻的"你出局了"说明是另一回事，不算夜提示）
  assert.equal(
    out.effects.filter((e) => e.type === 'private' && e.userId === w1.userId && /第 \d+ 夜·/.test(e.text)).length,
    0,
    '夜行动提示不该发给已出局的人'
  );

  // 第 2 夜：狼刀守卫；守卫守自己、预言家再查一次（**活着的行动角色都要提交，才会收齐结算**）
  const aliveW = by(out.state, 'wolf').filter((r) => r.alive);
  const aliveGuard = out.state.roles.find((r) => r.role === 'guard' && r.alive);
  out.state = pm(out.state, aliveW[0].userId, `刀 ${idx(out.state, aliveGuard.userId)}`).state;
  out.state = pm(out.state, aliveGuard.userId, `守 ${idx(out.state, aliveGuard.userId)}`).state;
  const aliveWitch = out.state.roles.find((r) => r.role === 'witch' && r.alive);
  if (aliveWitch) out.state = pm(out.state, aliveWitch.userId, '不救').state;
  out.state = pm(out.state, seer.userId, `查 ${idx(out.state, aliveW[0].userId)}`).state;
  assert.equal(out.state.phase, 'day', '第 2 夜收齐后应天亮');
  let o2 = { state: out.state, effects: [] };
  for (const uid of o2.state.order) o2 = wolf.onMessage(o2.state, { userId: uid, text: '说话', ts: 4 }, { now: 4 });
  for (const uid of o2.state.order) {
    const target = uid === aliveW[0].userId ? (o2.state.roles.find((r) => r.alive && r.role !== 'wolf')) : aliveW[0];
    o2 = wolf.onMessage(o2.state, { userId: uid, text: `投 ${idx(o2.state, target.userId)}`, ts: 5 }, { now: 5 });
  }
  assert.equal(o2.state.phase, 'ended', '狼全灭 → 结束');
  const end = o2.effects.find((e) => e.type === 'end');
  assert.match(end.result, /好人获胜/);
  assert.match(end.result, /狼人|平民/, '结算要公布全部身份');
});

test('自投不算票；平票不出人；票数过半才出局', () => {
  let s = newGame();
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;
  s = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;   // 进白天
  for (const uid of s.order) s = wolf.onMessage(s, { userId: uid, text: '发言', ts: 1 }, { now: 1 }).state;
  assert.equal(s.phase, 'vote');
  const [a, b] = s.order;
  const selfVote = wolf.onMessage(s, { userId: a, text: `投 ${idx(s, a)}`, ts: 2 }, { now: 2 });
  assert.match(selfVote.effects[0].text, /投自己/);
  assert.equal(Object.keys(selfVote.state.votes).length, 0, '自投不入票');
});

test('夜数上限：到顶那一夜结束时判平局并公布身份（计时起点非 0，超时才会生效）', () => {
  const base = newGame();
  const t0 = 1_700_000_000_000;
  // 最后一夜超时结算 → 白天
  const dawn = wolf.onTick({ ...base, night: base.maxNights, phaseStartedAt: t0 }, { now: t0 + 95 * 1000, rng: () => 0 });
  assert.equal(dawn.state.phase, 'day');
  // 白天走完发言与投票 → 进入"下一夜"那一刻超过上限 → 平局
  let s = dawn.state;
  for (const uid of [...s.order]) s = wolf.onMessage(s, { userId: uid, text: '说话', ts: 1 }, { now: 1 }).state;
  assert.equal(s.phase, 'vote');
  // 投票阶段超时（没人投）→ 直接进下一夜 → 已到上限 → 平局
  const out = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 });
  assert.equal(out.state.phase, 'ended');
  assert.match(out.effects.at(-1).result, /平局/);
  assert.match(out.effects.at(-1).result, /身份：/);
});

test('群里的"各种人"：非参与者投票不计、刷屏不刷私聊、退出有退路、结算可按开关保密', () => {
  // 1) 非参与者：在群里"投 3"、发行动词，都不进状态机
  let s = newGame(7);
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;
  s = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;   // 天亮
  for (const uid of s.order) s = wolf.onMessage(s, { userId: uid, text: '发言', ts: 1 }, { now: 1 }).state;
  assert.equal(s.phase, 'vote');
  const outsider = wolf.onMessage(s, { userId: 'u999', text: `投 ${idx(s, seer.userId)}`, ts: 2 }, { now: 2 });
  assert.equal(Object.keys(outsider.state.votes).length, 0, '没参加的人投票不计');
  assert.equal(outsider.effects.length, 0, '也不该由引擎回话（交给模型正常聊）');

  // 2) 刷屏：同一目标重复提交 → 静默消耗（consume），不再逐条回执
  let n = newGame(7);
  const guard = by(n, 'guard')[0];
  const target = by(n, 'villager')[0];
  n = pm(n, guard.userId, `守 ${idx(n, target.userId)}`).state;
  const again = pm(n, guard.userId, `守 ${idx(n, target.userId)}`);
  assert.equal(again.consume, true, '同一目标重复提交要静默消耗');
  assert.equal((again.effects || []).length, 0, '重复提交不再回执');
  // 反复改目标超过每人每夜上限 → 后续静默，但行动仍然记下（最后一次生效）
  let m = n;
  let silent = 0;
  for (let i = 0; i < 6; i += 1) {
    const t = n.roles.filter((r) => r.alive)[i % 6];
    const out = pm(m, guard.userId, `守 ${idx(m, t.userId)}`);
    m = out.state;
    if (out.consume && !(out.effects || []).length) silent += 1;
  }
  assert.ok(silent >= 1, '超过回执上限后要静默消耗（不回消息也不唤醒模型）');
  assert.ok(m.pending.guard, '刷屏期间行动仍然被记下');

  // 3) 退出：群里说"不玩了"也受理；退出一只狼局继续，狼全退光才结束
  const q = newGame();
  const [wa, wb] = by(q, 'wolf');
  const q1 = wolf.onMessage(q, { userId: wa.userId, text: '不玩了', ts: 1 }, { now: 1 });
  assert.equal(q1.state.phase, 'night', '还剩一只狼 → 局继续');
  assert.equal(q1.state.roles.find((r) => r.userId === wa.userId).alive, false, '退出的人移出本局');
  assert.equal(q1.state.roles.find((r) => r.userId === wa.userId).quit, true);
  assert.ok(q1.effects.some((e) => e.type === 'public' && /退出/.test(e.text) && e.text.includes(wa.name)), '群里要播报退出（不公布身份）');
  assert.ok(q1.effects.some((e) => e.type === 'private' && e.userId === wa.userId), '本人要收到确认');
  const q2 = wolf.onMessage(q1.state, { userId: wb.userId, text: '退赛', ts: 2 }, { now: 2 });
  assert.equal(q2.state.phase, 'ended', '狼全退光 → 结束');
  assert.match(q2.effects.at(-1).result, /好人获胜/);

  // 4) reveal=false：结算不公布身份（同一局把两只狼都劝退）
  const r = { ...newGame(), reveal: false };
  const [ra, rb] = by(r, 'wolf');
  const r1 = wolf.onMessage(r, { userId: ra.userId, text: '退出', ts: 1 }, { now: 1 });
  const r2 = wolf.onMessage(r1.state, { userId: rb.userId, text: '退出', ts: 2 }, { now: 2 });
  assert.equal(r2.state.phase, 'ended');
  const endText = r2.effects.at(-1).result;
  assert.match(endText, /好人获胜/);
  assert.equal(/身份：|狼人（|预言家/.test(endText), false, '关掉公开开关后结算不带身份');
});

test('白天不按点名：乱序发言也算数、边说边投直接记票（真人群必然乱序）', () => {
  let s = newGame();
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;
  s = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;
  assert.equal(s.phase, 'day');
  const alive = s.roles.filter((r) => r.alive);
  // 最后一个号码的人抢先说话：照样算他发过言（旧实现会把这句丢掉）
  const last = alive.at(-1);
  let out = wolf.onMessage(s, { userId: last.userId, text: '我先说！我怀疑 1 号', ts: 1 }, { now: 1 });
  assert.equal(out.state.phase, 'day', '还有人没说 → 继续讨论');
  assert.ok(out.state.spoken.includes(last.userId), '乱序发言要算数');
  assert.equal(out.effects.length, 0, '不为"没轮到"刷提示');
  // 中间几位陆续说完（顺序随意、有人多说一句）
  for (const r of alive.slice(0, -1).slice(1)) out = wolf.onMessage(out.state, { userId: r.userId, text: '我也说两句', ts: 2 }, { now: 2 });
  out = wolf.onMessage(out.state, { userId: alive[0].userId, text: '我说完了', ts: 2 }, { now: 2 });
  out = wolf.onMessage(out.state, { userId: last.userId, text: '再补一句', ts: 2 }, { now: 2 });
  assert.equal(out.state.spoken.length, alive.length, '多说不重复计数');
  // 最后一位边说边投 → 直接进投票，且他的票已经记下、不会被清掉
  const first = alive[0];
  out = wolf.onMessage(out.state, { userId: first.userId, text: `投 ${idx(out.state, last.userId)}`, ts: 3 }, { now: 3 });
  assert.equal(out.state.phase, 'vote');
  assert.equal(out.state.votes[first.userId], last.userId, '边说边投的票要保留');
});

test('白天固定讨论时长：到点进投票（不刷"没接上"），时长可按配置改', () => {
  // 默认 120 秒：95 秒还没到点
  let s = newGame();
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;
  s = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;
  assert.equal(s.phase, 'day');
  assert.equal(wolf.onTick(s, { now: s.phaseStartedAt + 110 * 1000, rng: () => 0 }).state.phase, 'day', '110 秒还没到 120 秒');
  const out = wolf.onTick(s, { now: s.phaseStartedAt + 125 * 1000, rng: () => 0 });
  assert.equal(out.state.phase, 'vote');
  assert.match(out.effects[0].text, /讨论 \d+ 秒到|开始投票/);
  assert.equal(/没接上/.test(JSON.stringify(out.effects)), false, '真人群不点名，不该有"没接上"');

  // 自定义时长（配置走 create 的 discussSeconds）
  const g = wolf.create({ players: PLAYERS, rng: () => 0, now: 1000, discussSeconds: 200 });
  assert.equal(g.discussSeconds, 200);
  let s2 = pm(g, by(g, 'seer')[0].userId, '查 1').state;
  s2 = wolf.onTick(s2, { now: s2.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;
  assert.equal(wolf.onTick(s2, { now: s2.phaseStartedAt + 150 * 1000, rng: () => 0 }).state.phase, 'day', '自定义 200 秒：150 秒还没到');
  assert.equal(wolf.onTick(s2, { now: s2.phaseStartedAt + 205 * 1000, rng: () => 0 }).state.phase, 'vote');
});

test('白天过半人说"投吧"立刻开投；恰好一半不算', () => {
  const toDay = () => {
    let s = newGame();
    s = pm(s, by(s, 'seer')[0].userId, '查 1').state;
    return wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;   // 白天
  };
  // 6 人存活：3 人（正好一半）说"投吧" → 不开
  let s = toDay();
  const alive = s.roles.filter((r) => r.alive);
  let out = { state: s, effects: [] };
  for (const r of alive.slice(0, 3)) out = wolf.onMessage(out.state, { userId: r.userId, text: '投吧', ts: 1 }, { now: 1 });
  assert.equal(out.state.phase, 'day', '恰好一半不算过半');
  assert.equal(out.state.readyVote.length, 3);
  // 第 4 个人也说 → 过半（4/6）立刻开投
  out = wolf.onMessage(out.state, { userId: alive[3].userId, text: '要不直接投吧', ts: 2 }, { now: 2 });
  assert.equal(out.state.phase, 'vote');
  assert.match(out.effects.at(-1).text, /过半人想投票|开始投票/);
  // 带目标的"投 3"不算"想过票"，而是记成票
  let s2 = toDay();
  const alive2 = s2.roles.filter((r) => r.alive);
  const o2 = wolf.onMessage(s2, { userId: alive2[0].userId, text: `投 ${idx(s2, alive2[1].userId)}`, ts: 1 }, { now: 1 });
  assert.equal(o2.state.readyVote.length, 0, '带目标的投票不算"想过票"');
  assert.equal(o2.state.votes[alive2[0].userId], alive2[1].userId);
});

test('审查回归：readyVote 跨天重置、预言家一夜只查一次、退出作废票与夜行动、闲聊不算行动', () => {
  const toDay = () => {
    let g = newGame();
    g = pm(g, by(g, 'seer')[0].userId, '查 1').state;
    return wolf.onTick(g, { now: g.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;   // 天亮
  };
  // ① readyVote 跨天重置：第 1 天 3 人说"投吧"（6 人没过半）→ 超时进投票 → 进下一夜 → 第 2 天不因旧记录立即开投
  let s = toDay();
  const alive = s.roles.filter((r) => r.alive);
  let out = { state: s, effects: [] };
  for (const r of alive.slice(0, 3)) out = wolf.onMessage(out.state, { userId: r.userId, text: '投吧', ts: 1 }, { now: 1 });
  assert.equal(out.state.phase, 'day');
  assert.equal(out.state.readyVote.length, 3);
  out = wolf.onTick(out.state, { now: out.state.phaseStartedAt + 125 * 1000, rng: () => 0 });   // 讨论超时 → 投票
  assert.equal(out.state.phase, 'vote');
  // 全员投票把 1 号投出 → 夜里无人行动 → 超时平安夜 → 第 2 天
  const aliveIds = out.state.roles.filter((r) => r.alive).map((r) => r.userId);
  out = wolf.onMessage(out.state, { userId: aliveIds[0], text: `投 ${idx(out.state, aliveIds[1])}`, ts: 2 }, { now: 2 });
  for (const uid of aliveIds.slice(1)) out = wolf.onMessage(out.state, { userId: uid, text: `投 ${idx(out.state, aliveIds[0])}`, ts: 2 }, { now: 2 });
  out = wolf.onTick(out.state, { now: out.state.phaseStartedAt + 95 * 1000, rng: () => 0 });   // 夜超时 → 天亮
  assert.equal(out.state.phase, 'day', JSON.stringify(out.state.phase));
  assert.equal(out.state.readyVote.length, 0, '第 2 天的"想开投"必须从零开始');
  const day2 = wolf.onMessage(out.state, { userId: out.state.roles.find((r) => r.alive).userId, text: '我说两句', ts: 3 }, { now: 3 });
  assert.equal(day2.state.phase, 'day', '不能因为第 1 天的旧"投吧"立刻开投');

  // ② 预言家一夜只能查一次：换个目标也只回"今晚已经查过"
  let p2 = newGame();
  const seer = by(p2, 'seer')[0];
  const targets = p2.roles.filter((r) => r.alive && r.userId !== seer.userId);
  p2 = pm(p2, seer.userId, `查 ${idx(p2, targets[0].userId)}`).state;
  const again = pm(p2, seer.userId, `查 ${idx(p2, targets[1].userId)}`);
  assert.equal(again.effects.length, 1);
  assert.match(again.effects[0].text, /今晚已经查过/);
  assert.equal(again.state.pending.seer, targets[0].userId, '第二次查验不能改写结果');

  // ③ 退出作废自己的票与夜行动，并从"发过言/想开投"里移除
  let q = toDay();
  const wolfA = q.roles.find((r) => r.role === 'wolf' && r.alive);
  q = wolf.onMessage(q, { userId: wolfA.userId, text: '投吧', ts: 1 }, { now: 1 }).state;
  const other = q.roles.find((r) => r.alive && r.userId !== wolfA.userId);
  q = wolf.onMessage(q, { userId: wolfA.userId, text: `投 ${idx(q, other.userId)}`, ts: 2 }, { now: 2 }).state;
  const quit = wolf.onMessage(q, { userId: wolfA.userId, text: '不玩了', ts: 3 }, { now: 3 });
  assert.equal(quit.state.votes[wolfA.userId], undefined, '退出者的票要作废');
  assert.equal((quit.state.readyVote || []).includes(wolfA.userId), false);
  assert.equal((quit.state.spoken || []).includes(wolfA.userId), false);

  // ④ 夜里闲聊不算行动（解析收紧）
  const night = wolf.create({ players: PLAYERS, rng: () => 0, now: 1000 });
  const w1 = by(night, 'wolf')[0];
  const chat = pm(night, w1.userId, '我 3 点再聊');
  assert.match(chat.effects[0].text, /没看懂/, '闲聊不该被当成"刀 3 号"');
  const real = pm(night, w1.userId, '刀 3');
  assert.match(real.effects[0].text, /已记下你的刀口/);
});

test('口语参与/退出： "我不玩了"/"不玩了，你们玩" 都算退出；"我玩不动了" 不算', () => {
  const base = newGame();
  const [w1, w2] = by(base, 'wolf');
  // ① "我不玩了"（最自然的说法）→ 退出
  const q1 = wolf.onMessage(base, { userId: w1.userId, text: '我不玩了', ts: 1 }, { now: 1 });
  assert.equal(q1.state.roles.find((r) => r.userId === w1.userId).quit, true, '"我不玩了" 要能退出');
  // ② "不玩了，你们玩"（带尾随说明）→ 退出
  const q2 = wolf.onMessage(base, { userId: w2.userId, text: '不玩了，你们玩吧', ts: 1 }, { now: 1 });
  assert.equal(q2.state.roles.find((r) => r.userId === w2.userId).quit, true, '带尾随说明也要认');
  // ③ 只是聊天里提到"玩不动" → 不退出
  const q3 = wolf.onMessage(base, { userId: w1.userId, text: '我玩不动了，你们继续', ts: 1 }, { now: 1 });
  assert.notEqual(q3.state.roles.find((r) => r.userId === w1.userId).quit, true, '聊天里提到"玩不动"不该被当成退出');
  // ④ 私聊同样认（口径一致）
  const q4 = wolf.onPrivateMessage(base, { userId: w1.userId, text: '我不玩了', ts: 1 }, { now: 1 });
  assert.equal(q4.state.roles.find((r) => r.userId === w1.userId).quit, true);
});

test('女巫①：同守同救必死（守卫守 X + 女巫救 X + 狼刀 X → X 仍出局）', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  const ask = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`);        // 狼交齐 → 女巫被问
  assert.ok(ask.effects.some((e) => /女巫行动/.test(e.text)), '要问女巫');
  s = ask.state;
  s = pm(s, guard.userId, `守 ${idx(s, seer.userId)}`).state;
  const heal = pm(s, witch.userId, '救');
  assert.match(heal.effects.at(-1).text, /用解药救/);
  const out = pm(heal.state, seer.userId, `查 ${idx(s, w1.userId)}`);   // 收齐 → 结算
  assert.equal(out.state.roles.find((r) => r.userId === seer.userId).alive, false, '同守同救必死');
  assert.match(out.effects.map((e) => e.text).join('|'), /预言家|小北|玩家3|倒牌/);
  assert.equal(out.state.potions.heal, false, '解药已消耗');
});

test('女巫②：毒药与狼刀能双杀；一晚只用一瓶（后选覆盖先选，先选的药不消耗）', () => {
  let s = newGame(6);
  const [w1, w2] = by(s, 'wolf');
  const witch = by(s, 'witch')[0];
  const villagers = by(s, 'villager');
  const va = villagers[0];
  const vb = villagers[1];
  s = pm(s, w1.userId, `刀 ${idx(s, va.userId)}`).state;
  const ask = pm(s, w2.userId, `刀 ${idx(s, va.userId)}`);
  assert.match(ask.effects.find((e) => /女巫/.test(e.text)).text, /被刀的是.*号/, '要告诉女巫谁被刀');
  s = ask.state;
  assert.match(pm(s, witch.userId, '救').effects[0].text, /用解药救/);
  const poison = pm(pm(s, witch.userId, '救').state, witch.userId, `毒 ${idx(s, vb.userId)}`);
  assert.match(poison.effects[0].text, /今晚毒/);
  const out = wolf.onTick(poison.state, { now: poison.state.phaseStartedAt + 95 * 1000, rng: () => 0 });   // 超时结算
  const aliveIds = out.state.roles.filter((r) => r.alive).map((r) => r.userId);
  assert.equal(aliveIds.includes(va.userId), false, '被刀的人死（女巫改成了毒，没救）');
  assert.equal(aliveIds.includes(vb.userId), false, '被毒的人也死');
  assert.equal(out.state.potions.heal, true, '解药没被用掉（最后选的是毒）');
  assert.equal(out.state.potions.poison, false, '毒药消耗了');
  assert.match(out.effects[0].text, /倒牌/);
});

test('女巫③：不能自救；药水用完会明确说', () => {
  // ① 狼刀女巫自己 → 提示不能自救，回「救」被拒
  let s = newGame(6);
  const [w1, w2] = by(s, 'wolf');
  const witch = by(s, 'witch')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, witch.userId)}`).state;
  const ask = pm(s, w2.userId, `刀 ${idx(s, witch.userId)}`);
  const asked = ask.effects.find((e) => e.userId === witch.userId && /女巫/.test(e.text));
  assert.match(asked.text, /不能自救/, '被刀的是她自己时要说明不能自救');
  const refused = pm(ask.state, witch.userId, '救');
  assert.match(refused.effects[0].text, /不能救自己/);
  assert.equal(refused.state.pending.witch, null, '拒绝后不算已行动');
  // 不救 → 她死
  const out = wolf.onTick(pm(refused.state, witch.userId, '不救').state, { now: 0 + 95 * 1000 + 1000, rng: () => 0 });
  assert.equal(out.state.roles.find((r) => r.userId === witch.userId).alive, false, '没救就死了');

  // ② 药水用完：手动把两瓶标成用完 → 提示与回执都要说清
  let t = wolf.create({ players: PLAYERS, rng: () => 0, now: 1000 });
  t = { ...t, potions: { heal: false, poison: false } };
  const tw = by(t, 'witch')[0];
  const [tw1, tw2] = by(t, 'wolf');
  t = pm(t, tw1.userId, `刀 ${idx(t, by(t, 'seer')[0].userId)}`).state;
  const ask2 = pm(t, tw2.userId, `刀 ${idx(t, by(t, 'seer')[0].userId)}`);
  const prompt = ask2.effects.find((e) => e.userId === tw.userId);
  assert.match(prompt.text, /两瓶药都用完/, '没药了要说清：' + prompt.text);
  assert.match(pm(ask2.state, tw.userId, '救').effects[0].text, /解药已经用过/);
  assert.match(pm(ask2.state, tw.userId, `毒 ${idx(ask2.state, ask2.state.roles[1].userId)}`).effects[0].text, /毒药已经用过/);
});

test('狼刀定下（女巫已被告知）后不许再改；定下之前可以改（女巫只会被告知一次）', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const witch = by(s, 'witch')[0];
  const seer = by(s, 'seer')[0];
  const villager = by(s, 'villager')[0];
  // 定下之前：狼一先刀村民、再改成预言家（此时狼二还没交，不算定下）
  s = pm(s, w1.userId, `刀 ${idx(s, villager.userId)}`).state;
  const changed = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`);
  assert.match(changed.effects[0].text, /已记下你的刀口：.*预言家|已记下你的刀口：3/, '未锁定时允许改');
  s = changed.state;
  assert.equal(s.pending.killTarget, '', '狼队没交齐时不算定下');
  // 狼二交 → 定下 + 只问一次女巫
  const locked = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`);
  const asks = locked.effects.filter((e) => e.userId === witch.userId && /女巫行动/.test(e.text));
  assert.equal(asks.length, 1, '女巫只该被问一次：' + JSON.stringify(locked.effects.map((e) => e.text)));
  s = locked.state;
  // 定下之后再改 → 被拒
  const denied = pm(s, w1.userId, `刀 ${idx(s, villager.userId)}`);
  assert.match(denied.effects[0].text, /刀口已经定下/);
  assert.equal(denied.state.pending.killTarget, locked.state.pending.killTarget, '被拒后刀口不变');
});

test('女巫④：只救不守 → 活（解药单独生效）；天亮播报与公开摘要口径一致', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const other = by(s, 'villager')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;          // 刀口定下 → 女巫被问
  s = pm(s, guard.userId, `守 ${idx(s, other.userId)}`).state;      // 守卫守的是别人（避开同守同救）
  const heal = pm(s, by(s, 'witch')[0].userId, '救');
  assert.match(heal.effects[0].text, /用解药救/);
  const out = pm(heal.state, seer.userId, `查 ${idx(s, w1.userId)}`);   // 收齐 → 结算
  assert.equal(out.state.roles.find((r) => r.userId === seer.userId).alive, true, '只救不守要活');
  assert.equal(out.state.potions.heal, false, '解药消耗');
  assert.match(out.effects.map((e) => e.text).join('|'), /平安夜/);
  assert.match(wolf.summaryForModel(out.state), /昨夜平安/, '摘要不能与播报打架');
});

test('审查回归：公开摘要的死讯与真实死亡一致（毒杀要报、结算前退出的不算死）', () => {
  // ① 守卫挡下狼刀 + 女巫毒死另一个人 → 真正出局的是被毒的（旧实现只看狼刀 → 误报"昨夜平安"）
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  const victim = by(s, 'villager')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, seer.userId)}`).state;       // 狼刀被守卫挡住
  s = pm(s, witch.userId, '不救').state;
  const poison = pm(s, witch.userId, `毒 ${idx(s, victim.userId)}`);
  assert.match(poison.effects[0].text, /今晚毒/);
  const done = pm(poison.state, seer.userId, `查 ${idx(s, w1.userId)}`).state;
  assert.equal(done.roles.find((r) => r.userId === victim.userId).alive, false, '被毒的出局');
  assert.equal(done.roles.find((r) => r.userId === seer.userId).alive, true, '被守的活');
  const sum = wolf.summaryForModel(done);
  assert.equal(/昨夜平安/.test(sum), false, '有人出局就不能报平安：' + sum);
  assert.match(sum, new RegExp(`昨夜 ${idx(s, victim.userId)} 号出局`), '要报真正出局的编号：' + sum);

  // ② 狼刀目标在结算前退出 → 实际没人出局，摘要不能虚报
  let t = newGame(7);
  const [t1, t2] = by(t, 'wolf');
  const tseer = by(t, 'seer')[0];
  const tguard = by(t, 'guard')[0];
  const twitch = by(t, 'witch')[0];
  const quitV = by(t, 'villager')[0];
  t = pm(t, t1.userId, `刀 ${idx(t, quitV.userId)}`).state;
  t = pm(t, t2.userId, `刀 ${idx(t, quitV.userId)}`).state;
  t = pm(t, tguard.userId, `守 ${idx(t, tguard.userId)}`).state;
  t = pm(t, twitch.userId, '不救').state;
  const quit = pm(t, quitV.userId, '不玩了');
  const done2 = pm(quit.state, tseer.userId, `查 ${idx(t, t1.userId)}`);
  assert.match(done2.effects.map((e) => e.text).join('|'), /平安夜/, '被刀的人退了就没人出局');
  assert.match(wolf.summaryForModel(done2.state), /昨夜平安/, '摘要要跟播报一致：' + wolf.summaryForModel(done2.state));
});

test('审查回归：女巫退出 → 本夜已交的药作废、药水不扣', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, guard.userId)}`).state;
  const heal = pm(s, witch.userId, '救');
  assert.match(heal.effects[0].text, /用解药救/);
  const quit = pm(heal.state, witch.userId, '不玩了');
  assert.equal(quit.effects.some((e) => /移出本局/.test(e.text)), true, '要给她退出回执');
  assert.equal(quit.state.pending.witch, null, 'pending 里的女巫行动要清掉');
  assert.equal(quit.state.potions.heal, true, '人走了药不消耗');
  // 退出那一刻预言家还没交最后一手（夜晚不算齐）→ 让他补上，结算时验证药确实作废了
  const out = pm(quit.state, seer.userId, `查 ${idx(s, w1.userId)}`);
  assert.equal(out.state.roles.find((r) => r.userId === seer.userId).alive, false, '作废后没人救，被刀的出局');
  assert.equal(out.state.potions.heal, true, '结算也不能把药扣掉');
});

test('审查回归：药都用完的女巫不再把这一夜拖到超时；被刀时文案不自相矛盾', () => {
  // 6 人局（无守卫）：狼 2 + 预言家 + 女巫（两瓶药都没了）
  let s = wolf.create({ players: PLAYERS, rng: () => 0, now: 1000 });
  s = { ...s, potions: { heal: false, poison: false } };
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const witch = by(s, 'witch')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  const ask = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`);
  const prompt = ask.effects.find((e) => e.userId === witch.userId);
  assert.match(prompt.text, /两瓶药都用完/, '没药了要说清：' + prompt.text);
  // 不必等她回话：预言家交完就该结算（旧实现一直等她的「不救」→ 拖满 90 秒）
  const out = pm(ask.state, seer.userId, `查 ${idx(s, w1.userId)}`);
  assert.equal(out.state.phase === 'night', false, '没药的女巫不该把夜晚卡住');
  assert.match(out.effects.map((e) => e.text).join('|'), /天亮了/);

  // 被刀的是她自己、毒药也没了 → 提示要给出可执行的下一步，而不是让她在提示之间打转
  let t = wolf.create({ players: PLAYERS, rng: () => 0, now: 1000 });
  t = { ...t, potions: { heal: true, poison: false } };
  const [tw1, tw2] = by(t, 'wolf');
  const twitch = by(t, 'witch')[0];
  t = pm(t, tw1.userId, `刀 ${idx(t, twitch.userId)}`).state;
  const selfAsk = pm(t, tw2.userId, `刀 ${idx(t, twitch.userId)}`);
  const selfPrompt = selfAsk.effects.find((e) => e.userId === twitch.userId);
  assert.match(selfPrompt.text, /不能自救/, selfPrompt.text);
  assert.equal(/用毒药毒一个人/.test(selfPrompt.text), false, '毒药已用完就别再劝她下毒：' + selfPrompt.text);
  assert.match(selfPrompt.text, /回「不救」继续/, '要给下一步：' + selfPrompt.text);
  assert.match(pm(selfAsk.state, twitch.userId, `毒 ${idx(selfAsk.state, by(selfAsk.state, 'seer')[0].userId)}`).effects[0].text, /毒药已经用过了/);
});

test('出局者还能说话？引擎一律不认：不计发言、不计票、不算"投吧"、不推进阶段', () => {
  let s = newGame(7);
  // 先把一个村民投出去（白天：全员发言 + 全员投他）
  const victim = by(s, 'villager')[0];
  s = { ...s, phase: 'day', spoken: [], votes: {}, readyVote: [] };
  for (const r of s.roles) s = wolf.onMessage(s, { userId: r.userId, text: '我先说说' }, { now: 1000 }).state;
  let out = null;
  for (const r of s.roles) {
    const target = r.userId === victim.userId ? s.roles.find((x) => x.userId !== victim.userId && x.alive) : victim;
    out = wolf.onMessage(s, { userId: r.userId, text: `投 ${idx(s, target.userId)}` }, { now: 1001 });
    s = out.state;
  }
  assert.equal(s.roles.find((r) => r.userId === victim.userId).alive, false, '村民被投出局');
  const notice = out.effects.filter((e) => e.type === 'private' && e.userId === victim.userId);
  assert.equal(notice.length, 1, '出局要私聊通知本人');
  assert.match(notice[0].text, /你出局了/);
  assert.match(notice[0].text, /不再计入本局/, '要说清之后的发言与投票不算：' + notice[0].text);

  // 出局者在群里继续说话/投票/"投吧" → 状态一个字节都不该变
  const snap = (x) => JSON.stringify({ spoken: x.spoken, votes: x.votes, ready: x.readyVote, phase: x.phase });
  const before = snap(s);
  const spokenBefore = (s.spoken || []).length;
  for (const text of ['我死了也要说，我怀疑 1 号', `投 ${idx(s, by(s, 'wolf')[0].userId)}`, '投吧', '直接投']) {
    const r = wolf.onMessage(s, { userId: victim.userId, text }, { now: 1002 });
    assert.equal(r.effects.length, 0, `出局者的话不该产生任何效果：${text}`);
    s = r.state;
  }
  assert.equal(snap(s), before, '出局者说话不得改动局面');
  assert.equal((s.spoken || []).length, spokenBefore, '出局者不算新发言');
  assert.equal(s.votes[victim.userId], undefined, '出局者的票不得记下');
  assert.equal((s.readyVote || []).includes(victim.userId), false, '出局者的"投吧"不得计数');
});

test('夜里出局也会私聊通知本人（退出的人不重复发）', () => {
  let s = newGame(7);
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const witch = by(s, 'witch')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, guard.userId)}`).state;
  s = pm(s, witch.userId, '不救').state;
  const out = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`);   // 收齐 → 结算（预言家被刀）
  const notices = out.effects.filter((e) => e.type === 'private' && /你出局了/.test(e.text));
  assert.equal(notices.length, 1, '夜里出局的人要收到私聊说明');
  assert.equal(notices[0].userId, seer.userId);

  // 退出导致的"出局"不重复发（他已经收过告别）
  let t = newGame(7);
  const tv = by(t, 'villager')[0];
  t = { ...t, phase: 'day' };
  const q = wolf.onMessage(t, { userId: tv.userId, text: '我不玩了' }, { now: 2000 });
  assert.equal(q.effects.filter((e) => /你出局了/.test(e.text)).length, 0, '退出不该再发"你出局了"');
  assert.equal(q.effects.some((e) => /移出本局/.test(e.text)), true);
});

test('审查回归：提醒类回执也吃"每人每夜 4 条"配额（不能靠反复发「救/查」刷私聊）', () => {
  // 女巫被刀：回「救」会被拒（不能自救）。连发 10 次，最多只该有 4 条回执
  let s = newGame(6);
  const [w1, w2] = by(s, 'wolf');
  const witch = by(s, 'witch')[0];
  s = pm(s, w1.userId, `刀 ${idx(s, witch.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, witch.userId)}`).state;          // 锁刀 → 问女巫
  let replies = 0;
  for (let i = 0; i < 10; i += 1) {
    const r = pm(s, witch.userId, '救');                             // 每次都被"不能自救"拒
    replies += r.effects.filter((e) => e.type === 'private').length;
    s = r.state;
  }
  assert.equal(replies, 4, '错误分支也要被配额夹住：' + replies);

  // 预言家：一夜只能查一次，连发同样只吃 4 条配额
  // （用 7 人局：6 人局行动少，第一次查完就收齐结算了，后面的私聊会落到白天、根本不回话）
  let t = newGame(7);
  const [t1, t2] = by(t, 'wolf');
  const seer = by(t, 'seer')[0];
  t = pm(t, t1.userId, `刀 ${idx(t, t2.userId)}`).state;
  t = pm(t, t2.userId, `刀 ${idx(t, t2.userId)}`).state;
  t = pm(t, by(t, 'witch')[0].userId, '不救').state;
  t = pm(t, seer.userId, `查 ${idx(t, t1.userId)}`).state;            // 第一次：给结果
  let seerReplies = 0;
  for (let i = 0; i < 10; i += 1) {
    const r = pm(t, seer.userId, `查 ${idx(t, t2.userId)}`);          // 之后：一夜只能查一次
    seerReplies += r.effects.filter((e) => e.type === 'private').length;
    t = r.state;
  }
  assert.equal(seerReplies, 3, '查过之后的重试也要吃配额（已用掉 1 条）：' + seerReplies);
});

test('审查回归：roundSeconds 对狼人杀生效（以前是只写着不生效的死配置）', () => {
  const mk = (secs) => wolf.create({ players: PLAYERS, rng: () => 0, now: 1000, roundSeconds: secs });
  const fast = mk(30);
  assert.equal(fast.roundSeconds, 30, '配置要进 state');
  const slow = mk(0);
  assert.equal(slow.roundSeconds, 0, '0 = 用插件默认（90）');
  // 30 秒窗口：31 秒到点就结算，29 秒不结算
  const early = wolf.onTick(fast, { now: fast.phaseStartedAt + 29 * 1000, rng: () => 0 });
  assert.equal(early.state.phase, 'night', '没到点不该结算');
  const late = wolf.onTick(fast, { now: fast.phaseStartedAt + 31 * 1000, rng: () => 0 });
  assert.equal(late.state.phase, 'day', '到点要结算（进入白天）');
  // 默认 90 秒：31 秒时不该结算
  const def = wolf.onTick(slow, { now: slow.phaseStartedAt + 31 * 1000, rng: () => 0 });
  assert.equal(def.state.phase, 'night', '默认窗口是 90 秒');
});
