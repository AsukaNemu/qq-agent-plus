// 谁是卧底：一对相近词，1 个卧底拿另一个词；轮流一句描述 → 投票淘汰 → 判胜负。
// 关键设计：**词只走私聊**，公开摘要里绝不出现词与身份（结构上不可能说漏嘴）。
import { sanitizeUserText } from '../../core/util.js';

export const meta = {
  id: 'undercover',
  name: '谁是卧底',
  minPlayers: 4,
  maxPlayers: 10,
  needsPrivate: true,      // 发词要私聊（受 groupGame.allowPrivateInvite 门控）
  roundSeconds: 150,       // 单轮发言超时：到点跳过没说话的人
  maxDurationMin: 45
};

const PAIRS = [
  ['豆浆', '牛奶'], ['西瓜', '冬瓜'], ['沙发', '床'], ['雪碧', '七喜'],
  ['薯片', '锅巴'], ['口红', '唇膏'], ['汉堡', '三明治'], ['咖啡', '奶茶'],
  ['篮球', '排球'], ['微博', '朋友圈'], ['冰箱', '冰柜'], ['筷子', '勺子'],
  ['空调', '电风扇'], ['拖鞋', '凉鞋'], ['眼镜', '墨镜'], ['蛋糕', '面包']
];

const alive = (s) => s.roles.filter((r) => !s.eliminated.includes(r.userId));

export function create({ players, rng, now = 0 } = {}) {
  // 局部常量而不是直接用解构参数：ops scan 认不出解构出来的名字（会当成未定义调用点）
  const rand = typeof rng === 'function' ? rng : Math.random;
  const pair = PAIRS[Math.floor(rand() * PAIRS.length)];
  const spyIndex = Math.floor(rand() * players.length);
  const roles = players.map((p, i) => ({
    userId: String(p.userId),
    // 群名片是用户可控文本，进提示词/播报前统一清洗（伪造段头穿透，2026-09-28 审查 P2）
    name: sanitizeUserText(String(p.name || p.userId)),
    word: i === spyIndex ? pair[1] : pair[0],
    spy: i === spyIndex
  }));
  return {
    phase: 'speak',
    round: 1,
    maxRounds: 4,
    order: roles.map((r) => r.userId),
    cursor: 0,
    roles,
    votes: {},
    eliminated: [],
    phaseStartedAt: now
  };
}

function parseVoteTarget(state, msg) {
  const text = String(msg.text || '');
  const m = /投\s*@?([^\s，。！？!?,.]{1,12})/.exec(text) || /@([^\s，。！？!?,.]{1,12})/.exec(text);
  if (!m) return null;
  const token = m[1].trim();
  // 已出局的人不是合法目标：否则会播报"某某出局"却什么都没发生（2026-09-28 审查 P3）
  const alive = (r) => (r && !state.eliminated.includes(r.userId) ? r : null);
  if (/^\d+$/.test(token)) {
    const idx = Number(token);
    if (idx >= 1 && idx <= state.roles.length) return alive(state.roles[idx - 1]);
    return null;
  }
  return alive(state.roles.find((r) => r.name === token || r.userId === token));
}

/** now：轮次推进时作为新一轮计时起点（不传则沿用旧行为，计时等第一个发言者开口）。 */
function tally(state, now = 0) {
  const counts = new Map();
  for (const target of Object.values(state.votes)) {
    counts.set(target, (counts.get(target) || 0) + 1);
  }
  const ranked = [...counts.entries()].sort((a, b) => b[1] - a[1]);
  const effects = [];
  if (!ranked.length) {
    effects.push({ type: 'public', text: '这轮没人投票，重新投一次。' });
    return { state: { ...state, votes: {} }, effects };
  }
  const [topId, topCount] = ranked[0];
  const tie = ranked.filter(([, c]) => c === topCount).length > 1;
  if (tie) {
    effects.push({ type: 'public', text: `平票，本轮不出人，直接进下一轮（${topCount} 票并列）。` });
    return nextRound(state, effects, now);
  }
  const out = state.roles.find((r) => r.userId === topId);
  const eliminated = [...state.eliminated, topId];
  effects.push({ type: 'public', text: `🗳 投票结果：${out.name} 出局（${topCount} 票）。` });
  const rest = state.roles.filter((r) => !eliminated.includes(r.userId));
  const spyAlive = rest.some((r) => r.spy);
  if (!spyAlive) {
    const words = state.roles.map((r) => `${r.name}=${r.word}`).join('，');
    return { state: { ...state, eliminated, phase: 'ended' }, effects: [...effects, { type: 'end', result: `平民获胜——卧底是 ${out.name}！词：${words}` }] };
  }
  if (rest.length <= 2) {
    const spy = rest.find((r) => r.spy);
    const words = state.roles.map((r) => `${r.name}=${r.word}`).join('，');
    return { state: { ...state, eliminated, phase: 'ended' }, effects: [...effects, { type: 'end', result: `卧底获胜——只剩 ${rest.length} 人且卧底（${spy.name}）还在场。词：${words}` }] };
  }
  return nextRound({ ...state, eliminated }, effects, now);
}

function nextRound(state, effects, now = 0) {
  const round = state.round + 1;
  if (round > state.maxRounds) {
    const spy = state.roles.find((r) => r.spy);
    return { state: { ...state, phase: 'ended' }, effects: [...effects, { type: 'end', result: `轮次用尽，卧底（${spy.name}）获胜。` }] };
  }
  const order = alive({ ...state, eliminated: state.eliminated }).map((r) => r.userId);
  return {
    // phaseStartedAt 必须在轮次切换时就起算：置 0 的话 onTick 里 `Number(0) || now` 恒等于
    // now，150 秒的"跳过没说话的人"对每轮排头永远不生效，整局会卡到全局时长上限
    // （2026-09-29 审查 P1，第 2 轮起必现）
    state: { ...state, phase: 'speak', round, order, cursor: 0, votes: {}, phaseStartedAt: now || 0 },
    effects: [...effects, { type: 'public', text: `第 ${round} 轮开始，从 ${state.roles.find((r) => r.userId === order[0])?.name} 开始，每人一句描述。` }]
  };
}

export function onMessage(state, msg, { now = 0 } = {}) {
  const s = JSON.parse(JSON.stringify(state));
  const uid = String(msg.userId);
  const me = s.roles.find((r) => r.userId === uid);
  if (!me || s.phase === 'ended' || s.eliminated.includes(uid)) return { state: s, effects: [] };

  if (s.phase === 'speak') {
    if (s.order[s.cursor] !== uid) return { state: s, effects: [] };
    s.cursor += 1;
    if (!s.phaseStartedAt) s.phaseStartedAt = now;
    if (s.cursor >= s.order.length) {
      s.phase = 'vote';
      s.votes = {};
      s.phaseStartedAt = now;
      return { state: s, effects: [{ type: 'public', text: `第 ${s.round} 轮发言结束，开始投票：发「投 3」或「投 @他」都行。` }] };
    }
    return { state: s, effects: [] };
  }

  if (s.phase === 'vote') {
    const target = parseVoteTarget(s, msg);
    if (!target) return { state: s, effects: [] };
    if (target.userId === uid) return { state: s, effects: [{ type: 'public', text: `${me.name} 想投自己？那不算，换一个。` }] };
    s.votes[uid] = target.userId;
    if (Object.keys(s.votes).length >= alive(s).length) return tally(s, now);
    return { state: s, effects: [] };
  }
  return { state: s, effects: [] };
}

/** 回合超时：跳过一直不说话的当前发言者；投票卡住则直接计票。 */
export function onTick(state, { now = 0 } = {}) {
  if (state.phase === 'ended') return { state, effects: [] };
  const started = Number(state.phaseStartedAt) || now;
  if (now - started < meta.roundSeconds * 1000) return { state, effects: [] };
  const s = JSON.parse(JSON.stringify(state));
  s.phaseStartedAt = now;
  if (s.phase === 'speak') {
    const uid = s.order[s.cursor];
    const who = s.roles.find((r) => r.userId === uid);
    s.cursor += 1;
    const effects = [{ type: 'public', text: `${who?.name || '有人'} 没接上，先跳过。` }];
    if (s.cursor >= s.order.length) {
      s.phase = 'vote';
      s.votes = {};
      effects.push({ type: 'public', text: `第 ${s.round} 轮发言结束，开始投票：发「投 3」或「投 @他」。` });
    }
    return { state: s, effects };
  }
  if (s.phase === 'vote') {
    // 一票都没有时 tally 会"重置再等"，超时-重置会无限空转到时长上限；
    // 直接进下一轮（有人投了才按票数结算）（2026-09-28 审查 P3）
    if (Object.keys(s.votes).length > 0) return tally(s, now);
    return nextRound(s, [{ type: 'public', text: '这轮没人投票，直接进下一轮。' }], now);
  }
  return { state: s, effects: [] };
}

export function summaryForModel(state) {
  if (state.phase === 'ended') return '谁是卧底已结束。';
  const list = state.roles.filter((r) => !state.eliminated.includes(r.userId)).map((r) => r.name).join('、');
  if (state.phase === 'speak') {
    const who = state.roles.find((r) => r.userId === state.order[state.cursor]);
    return `谁是卧底第 ${state.round} 轮：存活 ${list}；轮到 ${who?.name || '（全部说完）'} 描述自己的词（一句）。`;
  }
  const voted = Object.keys(state.votes).length;
  return `谁是卧底第 ${state.round} 轮投票中：已投 ${voted}/${alive(state).length} 票，存活 ${list}。`;
}

export function hostBrief(state) {
  if (state.phase === 'speak') return '轮到谁就等他说，别替人描述、别催超过一次；不要提到任何人的词。';
  return '投票阶段：不引导投给谁、不评价谁可疑，只报票数进度；绝不泄漏词或身份。';
}
