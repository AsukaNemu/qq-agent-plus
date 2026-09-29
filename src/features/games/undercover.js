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

const DAY_DISCUSS_SECONDS = 120;   // 描述阶段的固定讨论时长（控制台可改）
// "想直接投票"（没有具体目标时才算：有目标就是投票本身）
const READY_VOTE_RE = /(直接投|开始投票|可以投了?|赶紧投|快点投|投票吧|投吧|开投|别聊了|投得了)/

export function create({ players, rng, now = 0, reveal = true, discussSeconds = 0, roundSeconds = 0 } = {}) {
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
    // 描述阶段的固定讨论时长（秒）：到点直接进投票，不等谁；过半说"投吧"也立刻进
    discussSeconds: Math.min(600, Math.max(30, Number(discussSeconds) || DAY_DISCUSS_SECONDS)),
    roundSeconds: Math.min(600, Math.max(30, Number(roundSeconds) || meta.roundSeconds || 150)),
    readyVote: [],
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
  // 入口已经拦了出局者/退出者，这里是第二道闸：他们的票、以及投给已出局者的票都不算
  // （2026-09-29：有人投票后退出/被淘汰，那票会变成"幽灵票"把人投出去）
  const aliveIds = new Set(state.roles.filter((r) => !state.eliminated.includes(r.userId)).map((r) => r.userId));
  for (const [voter, target] of Object.entries(state.votes)) {
    if (!aliveIds.has(voter) || !aliveIds.has(target)) continue;
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
  // 私聊告诉本人：之后的描述与投票都不再计入（群里拦不住他说话，但别让他以为票还算数）
  effects.push({
    type: 'private',
    userId: out.userId,
    text: '【谁是卧底】你出局了。可以在群里继续围观聊天；之后你的描述与「投 X」都不再计入本局。'
  });
  const rest = state.roles.filter((r) => !eliminated.includes(r.userId));
  const spyAlive = rest.some((r) => r.spy);
  if (!spyAlive) {
    const result = state.reveal === false
      ? '平民获胜。'
      : `平民获胜——卧底是 ${out.name}！词：${state.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...state, eliminated, phase: 'ended' }, effects: [...effects, { type: 'end', result }] };
  }
  if (rest.length <= 2) {
    const spy = rest.find((r) => r.spy);
    const result = state.reveal === false
      ? '卧底获胜。'
      : `卧底获胜——只剩 ${rest.length} 人且卧底（${spy.name}）还在场。词：${state.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...state, eliminated, phase: 'ended' }, effects: [...effects, { type: 'end', result }] };
  }
  return nextRound({ ...state, eliminated }, effects, now);
}

function nextRound(state, effects, now = 0) {
  const round = state.round + 1;
  if (round > state.maxRounds) {
    const spy = state.roles.find((r) => r.spy);
    const result = state.reveal === false ? '轮次用尽，卧底获胜。' : `轮次用尽，卧底（${spy.name}）获胜。`;
    return { state: { ...state, phase: 'ended' }, effects: [...effects, { type: 'end', result }] };
  }
  const order = alive({ ...state, eliminated: state.eliminated }).map((r) => r.userId);
  return {
    // phaseStartedAt 必须在轮次切换时就起算：置 0 的话 onTick 里 `Number(0) || now` 恒等于
    // now，超时永远不生效，整局会卡到全局时长上限（2026-09-29 审查 P1，第 2 轮起必现）
    state: { ...state, phase: 'speak', round, order, cursor: 0, spoken: [], readyVote: [], votes: {}, phaseStartedAt: now || 0 },
    effects: [...effects, { type: 'public', text: `第 ${round} 轮开始：想描述的就说（不用等点名，每人一句），也可以直接发「投 3」带票；`
      + `${Number(state.discussSeconds) || DAY_DISCUSS_SECONDS} 秒后自动进投票，过半人说「投吧」也会立刻进。` }]
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
  // 别人投给他的票也作废（否则"投一个已经走了的人"会把这一轮投出个空结果）
  for (const [voter, victim] of Object.entries(s.votes)) {
    if (victim === me.userId) delete s.votes[voter];
  }
  s.spoken = (s.spoken || []).filter((x) => x !== me.userId);
  s.readyVote = (s.readyVote || []).filter((x) => x !== me.userId);
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
    const result = s.reveal === false ? '平民获胜。' : `平民获胜——卧底（${me.name}）退出了本局。词：${s.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...s, phase: 'ended' }, effects: [...effects, { type: 'end', result }] };
  }
  if (win === 'spy') {
    const spy = s.roles.filter((r) => !s.eliminated.includes(r.userId)).find((r) => r.spy);
    const result = s.reveal === false ? '卧底获胜。' : `卧底获胜——只剩 ${s.roles.length - s.eliminated.length} 人，卧底（${spy.name}）还在场。词：${s.roles.map((r) => `${r.name}=${r.word}`).join('，')}`;
    return { state: { ...s, phase: 'ended' }, effects: [...effects, { type: 'end', result }] };
  }
  // 退出后阶段要重算：剩下的人要是都说过了就直接进投票。
  // （旧判据写的是 cursor >= order.length，而 cursor 从不自增、永远不成立——死分支；2026-09-29 审查 P2）
  const leftAlive = s.roles.filter((r) => !s.eliminated.includes(r.userId));
  if (s.phase === 'speak' && leftAlive.length > 0 && leftAlive.every((r) => (s.spoken || []).includes(r.userId))) {
    s.phase = 'vote';
    s.votes = s.votes && typeof s.votes === 'object' ? s.votes : {};   // 早票要留着
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
  // 与狼人杀同口径：口语常说"我不玩了""不玩了，你们玩"，别做整句精确匹配
  if (/^\s*(?:我)?\s*(不玩了?|不玩啦|不参与|不参加|退出|退赛|弃权|观战|别带我|不凑热闹)/.test(String(msg.text || '').trim())) {
    return quitPlayer(s, me, now);
  }

  if (s.phase === 'speak') {
    // 真人不按点名说话：谁想描述就先说，说过一句就算过（不排顺序、不催"轮到谁"）；
    // 所有人说过、或时间到 → 进投票。多说几句不影响（去重靠 spoken）。
    s.spoken = Array.isArray(s.spoken) ? s.spoken : [];
    s.readyVote = Array.isArray(s.readyVote) ? s.readyVote : [];
    if (!s.spoken.includes(uid)) s.spoken.push(uid);
    // 边说边投也认：但描述阶段**只认「投 X」**（裸 @ 是在聊天，不该被静默记成票，2026-09-29 审查 P2）
    const earlyMatch = /投\s*@?([^\s，。！？!?,.]{1,12})/.exec(String(msg.text || ''));
    // 把"投"字带回去给解析器：它靠这个前缀区分"投票"与"裸 @"（裸 @ 是聊天，不算票）
    const early = earlyMatch ? parseVoteTarget(s, { text: `投 ${earlyMatch[1]}` }) : null;
    if (early && early.userId !== uid) s.votes[uid] = early.userId;
    // "投吧/直接投"（没有目标）＝ 想开投；超过半数立刻进投票
    if ((!early || early.userId === uid) && READY_VOTE_RE.test(String(msg.text || '')) && !s.readyVote.includes(uid)) s.readyVote.push(uid);
    const alive = s.roles.filter((r) => !s.eliminated.includes(r.userId));
    const majorityReady = alive.length > 0 && s.readyVote.length * 2 > alive.length;
    if (s.spoken.length >= alive.length || majorityReady) {
      if (alive.every((r) => s.votes[r.userId])) return tally(s, now);
      s.phase = 'vote';
      // 发言阶段提前投的票要留着（清掉等于把票丢了）
      s.votes = s.votes && typeof s.votes === 'object' ? s.votes : {};
      s.phaseStartedAt = now;
      const why = majorityReady && s.spoken.length < alive.length ? `过半人想投票（${s.readyVote.length}/${alive.length}）` : '都说得差不多了';
      return { state: s, effects: [{ type: 'public', text: `${why}，开始投票：发「投 3」或「投 @他」都行（存活 ${alive.length} 人各一票）。` }] };
    }
    return { state: s, effects: [] };
  }

  if (s.phase === 'vote') {
    // 与发言阶段同口径：只认「投 X」。裸 @ 是在聊天（回复里自动带 @ 很常见），
    // 以前会被 parseVoteTarget 的 @ 兜底静默记成一张票、还能顶掉之前投的票（2026-09-29 审查 P1）
    const voteMatch = /投\s*@?([^\s，。！？!?,.]{1,12})/.exec(String(msg.text || ''));
    const target = voteMatch ? parseVoteTarget(s, { text: `投 ${voteMatch[1]}` }) : null;
    if (!target) return { state: s, effects: [] };
    if (target.userId === uid) return { state: s, effects: [{ type: 'public', text: `${me.name} 想投自己？那不算，换一个。` }] };
    s.votes[uid] = target.userId;
    if (Object.keys(s.votes).length >= alive(s).length) return tally(s, now);
    return { state: s, effects: [] };
  }
  return { state: s, effects: [] };
}

/** 回合超时：跳过一直不说话的当前发言者；投票卡住则直接计票。 */
/** 私聊只处理一件事：退出（"不玩了"）——描述与投票都在群里，白天私聊照常聊天。 */
export function onPrivateMessage(state, msg, { now = 0 } = {}) {
  const s = JSON.parse(JSON.stringify(state));
  const uid = String(msg.userId);
  const me = s.roles.find((r) => r.userId === uid);
  if (!me || s.phase === 'ended' || s.eliminated.includes(uid)) return { state: s, effects: [] };
  const quitRe = /^\s*(?:我)?\s*(不玩了?|不玩啦|不参与|不参加|退出|退赛|弃权|观战|别带我|不凑热闹)/;
  if (!quitRe.test(String(msg.text || '').trim())) return { state: s, effects: [] };
  return quitPlayer(s, me, now);
}

export function onTick(state, { now = 0 } = {}) {
  if (state.phase === 'ended') return { state, effects: [] };
  const started = Number(state.phaseStartedAt) || now;
  // 描述阶段用固定讨论时长（discussSeconds），投票阶段用 roundSeconds
  const windowSec = state.phase === 'speak'
    ? (Number(state.discussSeconds) || DAY_DISCUSS_SECONDS)
    : (Number(state.roundSeconds) || meta.roundSeconds || 150);
  if (now - started < windowSec * 1000) return { state, effects: [] };
  const s = JSON.parse(JSON.stringify(state));
  s.phaseStartedAt = now;
  if (s.phase === 'speak') {
    // 描述阶段固定时长到点：进投票阶段。**早票要留着**（清掉等于把票丢了，2026-09-29 审查 P1）；
    // 但如果全员都已经投过，就直接结算，不再空等投票窗口
    const aliveIds = alive({ ...state, eliminated: state.eliminated }).map((r) => r.userId);
    if (aliveIds.length && aliveIds.every((uid) => state.votes?.[uid])) return tally(state, now);
    const s = JSON.parse(JSON.stringify(state));
    s.phase = 'vote';
    s.votes = s.votes && typeof s.votes === 'object' ? s.votes : {};
    s.phaseStartedAt = now;
    const secs = Number(s.discussSeconds) || DAY_DISCUSS_SECONDS;
    return { state: s, effects: [{ type: 'public', text: `描述 ${secs} 秒到，开始投票：发「投 3」或「投 @他」都行。` }] };
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
    return `谁是卧底第 ${state.round} 轮：存活 ${list}；已描述 ${spoken}/${alive.length} 人，`
      + `想开投 ${(state.readyVote || []).length} 人（过半即开）；谁想说就说，没说的再等等。`;
  }
  const voted = Object.keys(state.votes).length;
  return `谁是卧底第 ${state.round} 轮投票中：已投 ${voted}/${alive(state).length} 票，存活 ${list}。`;
}

export function hostBrief(state) {
  if (state.phase === 'speak') return '谁想描述就让他说，别按顺序点名、别催"轮到你"（真人群不按点名）；别替人描述、不要提到任何人的词。'
    + '已经出局的人在群里说话按"围观"处理：可以正常接话，但别把他的"投 X"当票、别顺着他的说法推谁可疑。';
  return '投票阶段：不引导投给谁、不评价谁可疑，只报票数进度；绝不泄漏词或身份。'
    + '已出局的人与没参加的人投的票都不算，被问到就说明一下。';
}
