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

export function create({ players, rng, now = 0, reveal = true } = {}) {
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
    // 结算是否公开词（控制台「结算时公开词与身份」那个勾；默认开）
    reveal: reveal !== false,
    round: 1,
    maxRounds: 4,
    order: roles.map((r) => r.userId),   // 只用于"第几号人"的展示（不再是发言顺序）
    cursor: 0,
    spoken: [],                            // 本轮描述过的人（谁想说就说，不按点名）
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
    const words = state.reveal === false ? '' : `词：${state.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...state, eliminated, phase: 'ended' }, effects: [...effects, { type: 'end', result: `平民获胜——卧底是 ${out.name}！${words}` }] };
  }
  if (rest.length <= 2) {
    const spy = rest.find((r) => r.spy);
    const words = state.reveal === false ? '' : `词：${state.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...state, eliminated, phase: 'ended' }, effects: [...effects, { type: 'end', result: `卧底获胜——只剩 ${rest.length} 人且卧底（${spy.name}）还在场。${words}` }] };
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
    // now，超时永远不生效，整局会卡到全局时长上限（2026-09-29 审查 P1，第 2 轮起必现）
    state: { ...state, phase: 'speak', round, order, cursor: 0, spoken: [], votes: {}, phaseStartedAt: now || 0 },
    effects: [...effects, { type: 'public', text: `第 ${round} 轮开始：想描述的就说（不用等点名，每人一句），也可以直接发「投 3」；都说过或时间到就进投票。` }]
  };
}

function checkWin(state) {
  const rest = state.roles.filter((r) => !state.eliminated.includes(r.userId));
  const spyAlive = rest.some((r) => r.spy);
  if (!spyAlive) return 'good';
  if (rest.length <= 2) return 'spy';
  return '';
}

/** 玩家退出：移出本局、不公布身份；该他发言就直接跳过，票已投的就作废。 */
function quitPlayer(state, me, now = 0) {
  const s = JSON.parse(JSON.stringify(state));
  s.eliminated = [...s.eliminated, me.userId];
  delete s.votes[me.userId];
  const i = s.order.indexOf(me.userId);
  if (i >= 0) {
    s.order = s.order.filter((x) => x !== me.userId);
    if (i < s.cursor) s.cursor -= 1;
  }
  const effects = [
    { type: 'private', userId: me.userId, text: '【谁是卧底】好，把你移出本局了，接下来正常聊天就行（不再催你发言）。' },
    { type: 'public', text: `👋 ${me.name} 退出了本局（身份不公布），还剩 ${s.roles.length - s.eliminated.length} 人。` }
  ];
  const win = checkWin(s);
  if (win === 'good') {
    const words = s.reveal === false ? '' : `词：${s.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...s, phase: 'ended' }, effects: [...effects, { type: 'end', result: `平民获胜——卧底（${me.name}）退出了本局。${words}` }] };
  }
  if (win === 'spy') {
    const spy = s.roles.filter((r) => !s.eliminated.includes(r.userId)).find((r) => r.spy);
    const words = s.reveal === false ? '' : `词：${s.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...s, phase: 'ended' }, effects: [...effects, { type: 'end', result: `卧底获胜——只剩 ${s.roles.length - s.eliminated.length} 人，卧底（${spy.name}）还在场。${words}` }] };
  }
  // 发言阶段退出的正好是当前发言者：跳过他就继续；所有存活者都发完 → 进投票
  if (s.phase === 'speak' && s.cursor >= s.order.length) {
    s.phase = 'vote';
    s.votes = {};
    s.phaseStartedAt = now;
    effects.push({ type: 'public', text: `第 ${s.round} 轮发言结束，开始投票：发「投 3」或「投 @他」都行。` });
  } else if (s.phase === 'vote' && Object.keys(s.votes).length >= s.roles.filter((r) => !s.eliminated.includes(r.userId)).length) {
    const out = tally(s, now);
    return { state: out.state, effects: [...effects, ...out.effects] };
  }
  return { state: s, effects };
}

export function onMessage(state, msg, { now = 0 } = {}) {
  const s = JSON.parse(JSON.stringify(state));
  const uid = String(msg.userId);
  const me = s.roles.find((r) => r.userId === uid);
  if (!me || s.phase === 'ended' || s.eliminated.includes(uid)) return { state: s, effects: [] };
  // 退出/观战：名单取"最近发过言的人"，得给不想玩的人一条退路
  if (/^\s*(不玩了?|不参与|退出|退赛|弃权|我观战|观战|别带我)\s*[!！。.~～…]?\s*$/.test(String(msg.text || '').trim())) {
    return quitPlayer(s, me, now);
  }

  if (s.phase === 'speak') {
    // 真人不按点名说话：谁想描述就先说，说过一句就算过（不排顺序、不催"轮到谁"）；
    // 所有人说过、或时间到 → 进投票。多说几句不影响（去重靠 spoken）。
    s.spoken = Array.isArray(s.spoken) ? s.spoken : [];
    if (!s.spoken.includes(uid)) s.spoken.push(uid);
    // 边说边投也认：描述阶段出现的"投 3"直接记成他的票
    const early = parseVoteTarget(s, msg);
    if (early && early.userId !== uid) s.votes[uid] = early.userId;
    const alive = s.roles.filter((r) => !s.eliminated.includes(r.userId));
    if (s.spoken.length >= alive.length) {
      if (alive.every((r) => s.votes[r.userId])) return tally(s, now);
      s.phase = 'vote';
      // 发言阶段提前投的票要留着（清掉等于把票丢了）
      s.votes = s.votes && typeof s.votes === 'object' ? s.votes : {};
      s.phaseStartedAt = now;
      return { state: s, effects: [{ type: 'public', text: `都说得差不多了，开始投票：发「投 3」或「投 @他」都行（存活 ${alive.length} 人各一票）。` }] };
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
    // 描述阶段到点：不等谁"接上"（真人群里没人按点名说话），直接进投票
    const s = JSON.parse(JSON.stringify(state));
    s.phase = 'vote';
    s.votes = {};
    s.phaseStartedAt = now;
    return { state: s, effects: [{ type: 'public', text: '时间到，开始投票：发「投 3」或「投 @他」都行。' }] };
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
    const alive = state.roles.filter((r) => !state.eliminated.includes(r.userId));
    const spoken = (state.spoken || []).length;
    return `谁是卧底第 ${state.round} 轮：存活 ${list}；已描述 ${spoken}/${alive.length} 人（谁想说就说，没说的再等等）。`;
  }
  const voted = Object.keys(state.votes).length;
  return `谁是卧底第 ${state.round} 轮投票中：已投 ${voted}/${alive(state).length} 票，存活 ${list}。`;
}

export function hostBrief(state) {
  if (state.phase === 'speak') return '谁想描述就让他说，别按顺序点名、别催"轮到你"（真人群不按点名）；别替人描述、不要提到任何人的词。';
  return '投票阶段：不引导投给谁、不评价谁可疑，只报票数进度；绝不泄漏词或身份。';
}
