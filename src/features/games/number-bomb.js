// 数字炸弹：1~100 里藏一个数，群友轮流猜，谁踩中谁输。纯公开信息，不需要私聊。
// 插件只产出 effects（要发的话/要结束），真正的发送与唤醒由 GroupGameManager 统一做。
export const meta = {
  id: 'number-bomb',
  name: '数字炸弹',
  minPlayers: 2,
  maxPlayers: 30,
  needsPrivate: false,
  roundSeconds: 0,        // 无回合制（谁都能猜），不需要超时推进
  maxDurationMin: 20
};

export function create({ rng } = {}) {
  const rand = typeof rng === 'function' ? rng : Math.random;
  const secret = 1 + Math.floor(rand() * 100);
  // low/high 是"已排除的边界"：对外文案是 low+1 ~ high-1。初始设 0/101，
  // 与 secret ∈ [1,100] 一致（原来 1/100 会让"炸弹就是 1 或 100"时文案说错，2026-09-28 审查 P3）
  // nudged：越界提示发过给谁（每人每局只回一次，防"猜 0"刷屏；2026-09-29 对抗性验证 P1）
  return { phase: 'guess', low: 0, high: 101, secret, guesses: 0, phaseStartedAt: 0, nudged: [] };
}

export function onMessage(state, msg) {
  // 只认"像猜测"的消息：整条基本就是个数字（可带"我猜/猜"），或句中出现"猜 N"。
  // 否则群里的"12 点开会""买了 3 个"会被当成猜测：误收窄区间、误判踩中，还会刷屏（2026-09-28 审查 P2）
  const text = String(msg.text || '').trim();
  const strict = /^(?:我)?(?:猜)?\s*(\d{1,3})\s*[!！。.~～…]?$/.exec(text);
  const m = strict || /猜\s*(\d{1,3})(?![\d])/.exec(text);
  if (!m) return { state, effects: [] };
  const n = Number(m[1]);
  const s = { ...state, guesses: state.guesses + 1 };
  if (n === s.secret) {
    return {
      state: { ...s, phase: 'ended', loser: msg.name, guess: n },
      effects: [{ type: 'end', result: `💣 ${msg.name} 踩中炸弹 ${n}！` }]
    };
  }
  if (n <= s.low || n >= s.high) {
    // 同一个人反复发越界数字只提醒一次：群发送配额被这类提示吃光后，
    // 引擎自己的播报（命中/收窄）反而发不出去（2026-09-29 对抗性验证 P1）
    const nudged = Array.isArray(s.nudged) ? s.nudged : [];
    const uid = String(msg.userId || '');
    if (uid && nudged.includes(uid)) return { state: s, effects: [] };
    if (uid) s.nudged = [...nudged, uid];
    return { state: s, effects: [{ type: 'public', text: `炸弹在 ${s.low + 1}~${s.high - 1} 之间，${n} 不在这段里` }] };
  }
  if (n < s.secret) s.low = n;
  else s.high = n;
  return { state: s, effects: [] };
}

export function onTick(state) {
  return { state, effects: [] };
}

export function summaryForModel(state) {
  if (state.phase === 'ended') return `数字炸弹已结束（${state.loser} 踩中 ${state.guess}）。`;
  return `数字炸弹进行中：炸弹在 ${state.low + 1}~${state.high - 1} 之间，已猜 ${state.guesses} 次还没人中。`;
}

export function hostBrief() {
  return '有人猜就顺着接一句（一两句，别像个播报机）；范围收窄时可以提醒一下区间。';
}
