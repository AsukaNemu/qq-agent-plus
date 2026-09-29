// 谁是卧底的插件级回归（2026-09-29 审查发现）：早票保留、全员早投立刻结算、
// reveal=false 不点名、私聊退出、裸 @ 不算票。
import assert from 'node:assert/strict';
import test from 'node:test';
import * as uc from '../src/features/games/undercover.js';

const PLAYERS = Array.from({ length: 4 }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
const newGame = (extra = {}) => uc.create({ players: PLAYERS, rng: () => 0, now: 1000, ...extra });
const by = (s, role) => role === 'spy' ? s.roles.find((r) => r.spy) : null;   // 只区分卧底/平民
const numOf = (s, uid) => s.roles.findIndex((r) => r.userId === uid) + 1;
const msg = (s, uid, text, now = 1) => uc.onMessage(s, { userId: uid, text, ts: now }, { now });
const advanceToVote = (s) => {
  let st = s;
  for (const uid of st.order) st = msg(st, uid, '这是个日用品').state;
  return st;
};

test('描述阶段提前投的票，在超时进投票时要保留（审查 P1：以前会被清空）', () => {
  let s = newGame();
  s = advanceToVote(s);
  // 还没进投票阶段？order 里的人说完就进 vote 了——这里要测的是"发言阶段就投了票"的路径，
  // 所以重新造一局：先说一句带票的话，再等超时
  let s2 = newGame();
  const first = s2.order[0];
  const other = s2.order.find((x) => x !== first);
  s2 = msg(s2, first, `我投 ${numOf(s2, other)} 吧`).state;   // 边说边投（描述阶段）
  assert.equal(s2.votes[first], other, '描述阶段带目标的投票要记下');
  const out = uc.onTick(s2, { now: s2.phaseStartedAt + 200 * 1000 });
  assert.equal(out.state.phase, 'vote');
  assert.equal(out.state.votes[first], other, '超时进投票后早票不能被清空');
});

test('全员在描述阶段就投完 → 最后一个人说完那一刻立刻结算（不等投票窗口）', () => {
  // 旧用例是假绿：全员各说一句已经把阶段推到 vote，后面那段 speak 分支永远不执行、
  // onTick 一次都没调（2026-09-29 审查 P2）。改成真的走"边说边投"：
  let s = newGame();
  const order = s.order;
  const target = order[3];                       // 让最后发言的人就是被投的目标（他不能投自己）
  for (const uid of order.slice(0, 3)) {         // 前三位：描述一句 + 顺手投 4 号
    s = msg(s, uid, `我描述一下，投 ${numOf(s, target)}`).state;
  }
  assert.equal(s.phase, 'speak', '还有人没说完，就该还在描述阶段');
  assert.equal(Object.keys(s.votes).length, 3, '早投要记下');
  // 最后一位说完 + 投别人 → 这时"全员都发过言 + 全员都投过票" → 立刻 tally（不等窗口、不用 tick）
  const out = msg(s, target, `我说完了，投 ${numOf(s, order[0])}`);
  assert.ok(/投票结果/.test(out.effects.map((e) => e.text).join('|')), '全员投完要立刻结算：' + JSON.stringify(out.effects));
  assert.equal(out.state.eliminated.includes(target), true, '票高的出局：' + JSON.stringify(out.state.eliminated));   // order 里存的是 userId 字符串
});

test('投票阶段也只认「投 X」：聊天里的裸 @ 不算票，「投 @他」才算（审查 P1）', () => {
  let s = advanceToVote(newGame());
  assert.equal(s.phase, 'vote');
  const me = s.order[0];
  const other = s.order[1];
  // ① 回复/点名时自动带的 @ → 不是在投票
  const chat = msg(s, me, `@玩家${numOf(s, other)} 你投谁？`);
  assert.equal(chat.state.votes[me], undefined, '裸 @ 不能被静默记成票：' + JSON.stringify(chat.state.votes));
  assert.equal(chat.effects.length, 0);
  // ② 「投 @他」是投票
  const vote = msg(s, me, `投 @玩家${numOf(s, other)}`);
  assert.equal(vote.state.votes[me], other, '「投 @他」要算票：' + JSON.stringify(vote.state.votes));
  // ③ 覆盖式：改票后以最新为准
  const changed = msg(vote.state, me, `投 ${numOf(s, s.order[2])}`);
  assert.equal(changed.state.votes[me], s.order[2], '改票要生效');
  // ④ 投不存在/投自己 → 不变
  const bad = msg(changed.state, me, '投 99');
  assert.equal(bad.state.votes[me], s.order[2], '投不存在的人不该改动已有的票');
  const self = msg(changed.state, me, `投 ${numOf(s, me)}`);
  assert.match(self.effects.map((e) => e.text).join('|'), /不能投自己|想投自己/, '投自己要被拒');
});

test('reveal=false：卧底被投出时结算也不点名（审查 P1：以前仍会说"卧底是 X"）', () => {
  let s = newGame({ reveal: false });
  s = advanceToVote(s);
  const spy = by(s, 'spy');
  let out = { state: s, effects: [] };
  for (const uid of s.order) {
    const target = uid === spy.userId ? s.order.find((x) => x !== spy.userId) : spy.userId;
    out = uc.onMessage(out.state, { userId: uid, text: `投 ${numOf(out.state, target)}`, ts: 2 }, { now: 2 });
  }
  assert.equal(out.state.phase, 'ended', '卧底出局即结束');
  const end = out.effects.find((e) => e.type === 'end');
  assert.match(end.result, /平民获胜/);
  assert.equal(/卧底是|词：/.test(end.result), false, '关掉公开开关后不许点名或报词：' + end.result);
});

test('reveal=false 的结算文本：只报胜方（含退出路径）', () => {
  const s0 = newGame({ reveal: false });
  const spy = by(s0, 'spy');
  const out = uc.onMessage(s0, { userId: spy.userId, text: '不玩了', ts: 1 }, { now: 1 });
  assert.equal(out.state.phase, 'ended');
  const end = out.effects.find((e) => e.type === 'end');
  assert.match(end.result, /平民获胜/);
  assert.equal(/卧底是|词：/.test(end.result), false, '关掉公开开关后不许点名或报词：' + end.result);
});

test('私聊也能退出（审查 P2：以前只有群里认）；裸 @ 不算票（审查 P2）', () => {
  const s0 = newGame();
  const u = s0.roles[0];
  const quit = uc.onPrivateMessage(s0, { userId: u.userId, text: '不玩了', ts: 1 }, { now: 1 });
  assert.equal(quit.state.eliminated.includes(u.userId), true, '私聊退出要生效');
  assert.ok(quit.effects.some((e) => /移出本局/.test(e.text)));

  // 裸 @ 不算票（描述阶段）
  let s = newGame();
  const first = s.order[0];
  const second = s.order[1];
  const after = msg(s, first, `@玩家${second.slice(1)} 你觉得呢`);
  assert.equal(Object.keys(after.state.votes || {}).length, 0, '裸 @ 是聊天，不该被记成票');
});

test('出局者还能说话？卧底同样一律不认；出局会私聊通知本人', () => {
  const players = Array.from({ length: 5 }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
  let s = uc.create({ players, rng: () => 0, now: 1000 });
  // 全员描述 + 全员投 1 号
  for (const r of s.roles) s = uc.onMessage(s, { userId: r.userId, text: '我这东西是白的' }, { now: 1001 }).state;
  let out = null;
  for (const r of s.roles) {
    // 1 号自己不能投自己（会被拒），他改投 2 号 —— 否则票数永远收不齐、进不了结算
    const vote = r.userId === s.roles[0].userId ? '投 2' : '投 1';
    out = uc.onMessage(s, { userId: r.userId, text: vote }, { now: 1002 });
    s = out.state;
  }
  const victim = out.effects.find((e) => e.type === 'private' && /你出局了/.test(e.text));
  assert.ok(victim, '出局要私聊通知本人');
  const dead = s.eliminated[0];
  const before = JSON.stringify({ spoken: s.spoken, votes: s.votes, ready: s.readyVote, phase: s.phase, eliminated: s.eliminated });
  for (const text of ['我出局了也要描述：我这杯是甜的', '投 2', '投吧']) {
    const r = uc.onMessage(s, { userId: dead, text }, { now: 1003 });
    assert.equal(r.effects.length, 0, `出局者的话不该有任何效果：${text}`);
    s = r.state;
  }
  assert.equal(JSON.stringify({ spoken: s.spoken, votes: s.votes, ready: s.readyVote, phase: s.phase, eliminated: s.eliminated }), before, '出局者说话不得改动局面');
});

test('幽灵票：投给"已退出者"的票不算数，也不会把这一轮投成他出局', () => {
  const players = Array.from({ length: 5 }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
  let s = uc.create({ players, rng: () => 0, now: 1000 });
  for (const r of s.roles) s = uc.onMessage(s, { userId: r.userId, text: '白的' }, { now: 1001 }).state;
  const quitter = s.roles[0];
  s = uc.onMessage(s, { userId: s.roles[1].userId, text: `投 1` }, { now: 1002 }).state;   // 有人投了 1 号
  const q = uc.onMessage(s, { userId: quitter.userId, text: '我不玩了' }, { now: 1003 });  // 1 号退出了
  s = q.state;
  assert.equal(q.effects.some((e) => /移出本局/.test(e.text)), true);
  assert.equal(Object.keys(s.votes).length, 0, '投给退出者的票要作废');
});
