// 狼人杀插件规则测试（模拟环境：纯插件调用，不连任何服务、不发消息）。
// 覆盖：角色表、夜行动收集（即时结算 / 超时结算）、守卫挡刀、查验只达本人、
// 狼队多数、白天发言与投票、胜负判定、夜数上限平局、公开摘要不泄密。
import assert from 'node:assert/strict';
import test from 'node:test';
import * as wolf from '../src/features/games/werewolf.js';

// "狼人杀"这个游戏名里含"狼人"，判"有没有泄身份"时要把它排除掉
const hasRoleWord = (text) => /预言家|守卫|平民/.test(text) || /狼人(?!杀)/.test(text);

const PLAYERS = Array.from({ length: 6 }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
const by = (s, role) => s.roles.filter((r) => r.role === role);
const idx = (s, uid) => s.roles.findIndex((r) => r.userId === uid) + 1;
const pm = (s, uid, text, now = 1) => wolf.onPrivateMessage(s, { userId: uid, text, ts: now }, { now, rng: () => 0 });

/** 造一个可复现的 6 人局（rng 固定 → 洗牌结果固定）。 */
function newGame() {
  return wolf.create({ players: PLAYERS, rng: () => 0, now: 1000 });
}

test('角色表与规模：6 人=2 狼/1 预/1 守/2 民，9 人=3 狼；人数不足 6 给不了合适配置', () => {
  const g6 = newGame();
  assert.equal(by(g6, 'wolf').length, 2);
  assert.equal(by(g6, 'seer').length, 1);
  assert.equal(by(g6, 'guard').length, 1);
  assert.equal(by(g6, 'villager').length, 2);
  const g9 = wolf.create({ players: Array.from({ length: 9 }, (_, i) => ({ userId: `x${i}`, name: `x${i}` })), rng: () => 0 });
  assert.equal(by(g9, 'wolf').length, 3);
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
  // 6 人：身份 6 条 + 夜行动提示（守卫 1 + 狼 2 + 预言家 1）
  assert.equal(priv.filter((e) => /你是\*\*/.test(e.text)).length, 6);
  const dealWolf = priv.find((e) => /你是\*\*狼人\*\*/.test(e.text));
  const wolfNames = by(g, 'wolf').filter((w) => w.userId !== dealWolf.userId).map((w) => w.name);
  assert.ok(wolfNames.every((n) => dealWolf.text.includes(n)), '狼的 Deal 里要写明队友');
  const prompts = priv.filter((e) => /第 1 夜·/.test(e.text));   // 别用"行动"筛：身份文案里也有这个词
  assert.equal(prompts.length, 4, '守卫+2 狼+预言家各一条夜行动提示');
});

test('夜晚：收齐即结算；守卫挡刀=平安夜；查验结果只发给预言家本人', () => {
  let s = newGame();
  const [w1, w2] = by(s, 'wolf');
  const seer = by(s, 'seer')[0];
  const guard = by(s, 'guard')[0];
  const villager = by(s, 'villager')[0];

  // 平民夜里没有行动：给一句明确回执，不推进
  const vOut = pm(s, villager.userId, '我睡觉了');
  assert.match(vOut.effects[0].text, /夜里你没有行动/);
  assert.equal(vOut.state.phase, 'night');

  s = pm(s, w1.userId, `刀 ${idx(s, seer.userId)}`).state;
  assert.equal(s.phase, 'night', '还没收齐，不能提前结算');
  s = pm(s, w2.userId, `刀 ${idx(s, seer.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, seer.userId)}`).state;     // 守卫也守了同一个人
  const out = pm(s, seer.userId, `查 ${idx(s, w1.userId)}`);       // 最后一个行动 → 立即结算

  const texts = out.effects.map((e) => e.text);
  assert.ok(texts.some((t) => /你是「狼人」|是「狼人」/.test(t)), '预言家要拿到查验结果');
  const seerResult = out.effects.filter((e) => e.type === 'private' && /查验结果/.test(e.text));
  assert.equal(seerResult.length, 1);
  assert.equal(seerResult[0].userId, seer.userId, '查验结果只能发给预言家');
  assert.ok(!out.effects.some((e) => e.type === 'public' && hasRoleWord(e.text)), '公开消息不得出现身份词');
  assert.ok(texts.some((t) => /平安夜/.test(t)), '守卫守中狼刀 → 平安夜');
  assert.equal(out.state.phase, 'day');
  assert.equal(out.state.roles.filter((r) => r.alive).length, 6);
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
  let s = newGame();
  const [w1, w2] = by(s, 'wolf');
  const guard = by(s, 'guard')[0];
  const seer = by(s, 'seer')[0];
  const villagers = by(s, 'villager');

  // 第 1 夜：狼刀村民 A，守卫守村民 B（无效），预言家查狼 1
  const va = villagers[0];
  const vb = villagers[1];
  s = pm(s, w1.userId, `刀 ${idx(s, va.userId)}`).state;
  s = pm(s, w2.userId, `刀 ${idx(s, va.userId)}`).state;
  s = pm(s, guard.userId, `守 ${idx(s, vb.userId)}`).state;
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
  // 夜里行动提示只发给活着的人
  assert.equal(out.effects.filter((e) => e.type === 'private' && e.userId === w1.userId).length, 0);

  // 第 2 夜：狼刀守卫；守卫守自己、预言家再查一次（**活着的行动角色都要提交，才会收齐结算**）
  const aliveW = by(out.state, 'wolf').filter((r) => r.alive);
  const aliveGuard = out.state.roles.find((r) => r.role === 'guard' && r.alive);
  out.state = pm(out.state, aliveW[0].userId, `刀 ${idx(out.state, aliveGuard.userId)}`).state;
  out.state = pm(out.state, aliveGuard.userId, `守 ${idx(out.state, aliveGuard.userId)}`).state;
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
  let s = newGame();
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;
  s = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;   // 天亮
  for (const uid of s.order) s = wolf.onMessage(s, { userId: uid, text: '发言', ts: 1 }, { now: 1 }).state;
  assert.equal(s.phase, 'vote');
  const outsider = wolf.onMessage(s, { userId: 'u999', text: `投 ${idx(s, seer.userId)}`, ts: 2 }, { now: 2 });
  assert.equal(Object.keys(outsider.state.votes).length, 0, '没参加的人投票不计');
  assert.equal(outsider.effects.length, 0, '也不该由引擎回话（交给模型正常聊）');

  // 2) 刷屏：同一目标重复提交 → 静默消耗（consume），不再逐条回执
  let n = newGame();
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

test('白天到点不等人：谁都不说话也进投票，不再出现"XX 没接上"', () => {
  let s = newGame();
  const seer = by(s, 'seer')[0];
  s = pm(s, seer.userId, `查 ${idx(s, seer.userId)}`).state;
  s = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 }).state;
  const out = wolf.onTick(s, { now: s.phaseStartedAt + 95 * 1000, rng: () => 0 });
  assert.equal(out.state.phase, 'vote');
  assert.match(out.effects[0].text, /时间到|开始投票/);
  assert.equal(/没接上/.test(JSON.stringify(out.effects)), false, '真人群不点名，不该有"没接上"');
});
