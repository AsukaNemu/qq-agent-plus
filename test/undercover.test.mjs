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
  let s = advanceToVote(newGame());
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
  // 关键：要投出一个**平民**（卧底 roles[0] 还在），局才会继续到下一轮 ——
  // 以前投的是卧底、局当场结束，后面的断言全落在 phase==='ended' 上，恒真（2026-09-29 审查）
  const deadVillager = s.roles[1];
  let out = null;
  for (const r of s.roles) {
    const vote = r.userId === deadVillager.userId ? `投 ${numOf(s, s.roles[0].userId)}` : `投 ${numOf(s, deadVillager.userId)}`;
    out = uc.onMessage(s, { userId: r.userId, text: vote }, { now: 1002 });
    s = out.state;
  }
  const victim = out.effects.find((e) => e.type === 'private' && /你出局了/.test(e.text));
  assert.ok(victim, '出局要私聊通知本人');
  assert.equal(s.phase, 'speak', '投出平民后局要继续到下一轮：' + s.phase);
  const dead = deadVillager.userId;
  assert.equal(s.eliminated.includes(dead), true);
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

test('坏人赢的结算分支：剩 2 人且卧底在场 → 卧底获胜；轮次用尽 → 卧底获胜', () => {
  // ① 5 人局：投出两个平民 → 剩 2 人且卧底在 → 卧底获胜
  const players = Array.from({ length: 5 }, (_, i) => ({ userId: `u${i + 1}`, name: `玩家${i + 1}` }));
  let s = uc.create({ players, rng: () => 0, now: 1000 });
  const spy = s.roles.find((r) => r.spy);
  const good = s.roles.filter((r) => !r.spy);
  let line = '';
  for (const victim of good.slice(0, 3)) {   // 5 人里投出 3 个好人 → 剩 2 人且卧底在（<=2 才判卧底胜）
    s = uc.onTick(s, { now: (s.phaseStartedAt || 0) + 999 * 1000 });   // 到点进投票
    s = s.state;
    let out = null;
    const aliveNow = s.roles.filter((x) => !s.eliminated.includes(x.userId));
    for (const r of aliveNow) {
      // 被投的人不能投自己（会被拒、票收不齐）→ 他改投第一个好人
      const target = r.userId === victim.userId
        ? aliveNow.find((x) => x.userId !== victim.userId)
        : victim;
      out = uc.onMessage(s, { userId: r.userId, text: `投 ${numOf(s, target.userId)}` }, { now: 1001 });
      s = out.state;
    }
    line = (out?.effects || []).map((e) => e.text ?? e.result ?? '').join(' | ');
    if (s.phase === 'ended') break;
  }
  assert.equal(s.phase, 'ended', '剩 2 人且卧底在，要结束：' + s.phase);
  assert.match(line, /卧底获胜/, '要判卧底获胜：' + line);
  assert.equal(line.includes(spy.name), true, '默认要公布卧底是谁');

  // ② 轮次用尽：4 人局每轮 2:2 平票 → 到 maxRounds 之后判卧底获胜
  const p4 = Array.from({ length: 4 }, (_, i) => ({ userId: `v${i + 1}`, name: `玩家${i + 1}` }));
  let t = uc.create({ players: p4, rng: () => 0, now: 1000 });
  let endLine = '';
  for (let round = 0; round < 6 && t.phase !== 'ended'; round += 1) {
    t = uc.onTick(t, { now: (t.phaseStartedAt || 0) + 999 * 1000 }).state;   // 描述到点 → 投票
    const aliveIds = t.roles.filter((r) => !t.eliminated.includes(r.userId)).map((r) => r.userId);
    // 2:2 平票（u1+u2 投 u3，u3+u4 投 u1）
    for (const [voter, target] of [[aliveIds[0], aliveIds[2]], [aliveIds[1], aliveIds[2]], [aliveIds[2], aliveIds[0]], [aliveIds[3], aliveIds[0]]]) {
      if (!aliveIds.includes(voter) || !aliveIds.includes(target)) continue;
      const r = uc.onMessage(t, { userId: voter, text: `投 ${numOf(t, target)}` }, { now: 1002 });
      t = r.state;
      if (t.phase === 'ended') { endLine = r.effects.map((e) => e.text ?? e.result ?? '').join(' | '); break; }
    }
  }
  assert.equal(t.phase, 'ended', '平票拖到轮次上限也要结束：' + t.phase);
  assert.match(endLine, /轮次用尽，卧底.*获胜/, '要报轮次用尽：' + endLine);
});

test('对抗性回归 F2：投票阶段反复「投 自己」只有一次公开提醒', () => {
  let s = advanceToVote(newGame());
  const me = s.order[0];
  let pubs = 0;
  for (let i = 0; i < 12; i += 1) {
    const out = uc.onMessage(s, { userId: me, text: `投 ${numOf(s, me)}` }, { now: 100 });
    pubs += out.effects.filter((e) => e.type === 'public').length;
    s = out.state;
  }
  assert.equal(pubs, 1, '同一轮只提醒一次：' + pubs);
  assert.equal(s.votes[me], undefined, '投自己始终不入票');
});

test('描述阶段的早票：全员说完切到投票时不能把票丢掉（speak 分支，不是 onTick 那条）', () => {
  let s = newGame();
  const me = s.order[0];
  const target = s.order[2];
  s = uc.onMessage(s, { userId: me, text: `我描述一下，投 ${numOf(s, target)}` }, { now: 1001 }).state;   // 边说边投
  assert.equal(s.votes[me], target, '描述阶段的票要记下');
  // 其余人依次描述，最后一位说完 → 进投票（spoken 满了）
  for (const uid of s.order.slice(1)) {
    s = uc.onMessage(s, { userId: uid, text: '我这东西是白的' }, { now: 1002 }).state;
  }
  assert.equal(s.phase, 'vote', '全员说完要进投票');
  assert.equal(s.votes[me], target, '切阶段时早票必须保留（以前会被清空）');
});

test('给模型的摘要与主持口径：不含身份/词，且叮嘱了出局者的话怎么处理', () => {
  const s = newGame();
  const sum = uc.summaryForModel(s);
  assert.match(sum, /谁是卧底第 1 轮/, sum);
  assert.match(sum, /存活/, sum);
  // 注意：游戏名本身就叫「谁是卧底」，不能拿"卧底"当泄密判据（与狼人杀里的 /狼人(?!杀)/ 同款）
  // 游戏名里就有"卧底"，先把它摘掉再查；并且**不能用真实词值**（这才是核心不变量）
  const cleaned = sum.replace(/谁是卧底/g, '');
  assert.equal(/卧底|身份|词是|词：|平民/.test(cleaned), false, '摘要不得提到身份与词：' + sum);
  for (const r of s.roles) assert.equal(sum.includes(r.word), false, '摘要里不能出现任何人的词：' + sum);
  const brief = uc.hostBrief(s);
  assert.match(brief, /别替人描述|不要提到任何人的词/, brief);
  assert.match(brief, /出局的人/, '主持口径要说出局者的话按围观处理：' + brief);
  const voteBrief = uc.hostBrief({ ...s, phase: 'vote' });
  assert.match(voteBrief, /绝不泄漏词或身份/, voteBrief);
  assert.match(voteBrief, /已出局的人|没参加的人/, voteBrief);
});
