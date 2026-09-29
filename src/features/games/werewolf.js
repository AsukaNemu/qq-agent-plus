// 狼人杀（简化 6~9 人）：夜里私聊提交行动（守卫/狼队/预言家），白天在群里讨论与投票。
// 与谁是卧底的两个本质差异：
//   ① 私聊是**贯穿全程的行动通道**——玩家私聊机器人提交行动（onPrivateMessage），
//      Manager 在 ingest 入口接管这些消息并就地标记已读，不唤醒模型（省调用 + 零泄密面）；
//   ② 公开信息与隐藏信息严格分层——身份、夜晚行动、查验结果全部只走私聊，
//      公开摘要（summaryForModel）里只有存活名单与公开死讯。
// 行动格式：回**编号**（开局公告里 1=谁 …）或群名片，一字不差。重复提交 = 覆盖。
import { sanitizeUserText } from '../../core/util.js';

export const meta = {
  id: 'werewolf',
  name: '狼人杀',
  minPlayers: 6,
  maxPlayers: 9,
  needsPrivate: true,      // 发身份 + 夜里行动都要私聊（受 allowPrivateInvite / 游戏豁免开关门控）
  roundSeconds: 90,        // 夜行动窗口 / 每人发言 / 投票 各自的超时
  maxDurationMin: 60
};

// 角色表（按人数）：先狼、再预言家/守卫、其余平民
const ROLE_TABLE = {
  6: ['wolf', 'wolf', 'seer', 'guard', 'villager', 'villager'],
  7: ['wolf', 'wolf', 'seer', 'guard', 'villager', 'villager', 'villager'],
  8: ['wolf', 'wolf', 'seer', 'guard', 'villager', 'villager', 'villager', 'villager'],
  9: ['wolf', 'wolf', 'wolf', 'seer', 'guard', 'villager', 'villager', 'villager', 'villager']
};
const ROLE_NAME = { wolf: '狼人', seer: '预言家', guard: '守卫', villager: '平民' };
const MAX_NIGHTS = 6;

const aliveList = (state) => state.roles.filter((r) => r.alive);

export function create({ players, rng, now = 0 } = {}) {
  const rand = typeof rng === 'function' ? rng : Math.random;
  const list = (players || []).map((p) => ({
    userId: String(p.userId),
    // 群名片是用户可控文本，进提示词/播报前统一清洗（伪造段头穿透，与卧底同口径）
    name: sanitizeUserText(String(p.name || p.userId))
  }));
  const table = ROLE_TABLE[list.length] || ROLE_TABLE[6];
  const shuffled = [...list];
  for (let i = shuffled.length - 1; i > 0; i -= 1) {
    const j = Math.floor(rand() * (i + 1));
    [shuffled[i], shuffled[j]] = [shuffled[j], shuffled[i]];
  }
  const roles = shuffled.map((p, i) => ({ ...p, role: table[i] || 'villager', alive: true }));
  return {
    phase: 'night',
    night: 1,
    roles,
    order: roles.map((r) => r.userId),
    cursor: 0,
    pending: { guard: '', wolves: {}, seer: '' },   // 本夜已收到的行动
    nightLog: [],                                    // 每晚结算，结束时公布
    phaseStartedAt: now,
    votes: {},
    maxNights: MAX_NIGHTS
  };
}

const idxOf = (state, r) => state.roles.indexOf(r) + 1;
const aliveOf = (state, role) => state.roles.filter((r) => r.role === role && r.alive);

/** 解析"守/刀/查 3"这类行动：编号（1..N）或群名片，歧义/不在场返回 null。 */
function parseTarget(state, text) {
  const t = String(text || '').trim();
  if (!t) return null;
  const m = /(\d{1,2})\s*号?/.exec(t);
  if (m) {
    const idx = Number(m[1]);
    if (idx >= 1 && idx <= state.roles.length) {
      const hit = state.roles[idx - 1];
      return hit && hit.alive ? hit : null;
    }
    return null;
  }
  const bare = t.replace(/^(守|刀|杀|查|我选|选择)\s*/g, '').trim();
  return state.roles.find((r) => r.alive && (r.name === t || r.name === bare)) || null;
}

/** 夜晚行动提示（开局与每晚结算后各发一次）。 */
function nightPrompts(state) {
  const eff = [];
  const guard = aliveOf(state, 'guard')[0];
  const seer = aliveOf(state, 'seer')[0];
  const wolves = aliveOf(state, 'wolf');
  if (guard) {
    const ban = state.lastGuard ? `（昨晚守了 ${idxOf(state, state.roles.find((r) => r.userId === state.lastGuard))} 号，今晚不能连守）` : '';
    eff.push({ type: 'private', userId: guard.userId, text: `【狼人杀】第 ${state.night} 夜·守卫行动：你要守谁？回编号或群名片${ban}。不回就当你今晚不守。` });
  }
  if (wolves.length) {
    for (const w of wolves) {
      const mates = wolves.filter((x) => x.userId !== w.userId).map((x) => x.name).join('、');
      eff.push({ type: 'private', userId: w.userId, text: `【狼人杀】第 ${state.night} 夜·狼队行动：你们刀谁？回编号或群名片（队友：${mates || '只有你'}）。不回就当你弃权。` });
    }
  }
  if (seer) {
    eff.push({ type: 'private', userId: seer.userId, text: `【狼人杀】第 ${state.night} 夜·预言家行动：你要查谁？回编号或群名片，我立刻把结果发给你。` });
  }
  return eff;
}

/** 开局效果：公开公告（含编号名单）+ 逐个私聊发身份 + 第 1 夜行动提示。 */
export function openingEffects(state) {
  const list = state.roles.map((r, i) => `${i + 1}=${r.name}`).join('、');
  const eff = [{
    type: 'public',
    text: `🐺 狼人杀开局：${state.roles.length} 人 —— ${list}。身份已私聊给各位（狼队互相可见），`
      + '夜里按私聊提示回行动，白天在群里讨论、发「投 3」投票。没收到身份的私下告诉我。'
  }];
  for (const r of state.roles) {
    const mates = aliveOf(state, 'wolf').filter((w) => w.userId !== r.userId).map((w) => `${idxOf(state, w)} 号 ${w.name}`);
    const text = r.role === 'wolf'
      ? `【狼人杀】你是**狼人**。${mates.length ? `队友：${mates.join('、')}。` : ''}每晚我会私聊问你刀谁（狼队各自回，多数一致生效）。`
      : (r.role === 'seer' ? '【狼人杀】你是**预言家**。每晚可查一个人是"狼人/好人"，结果只发给你。'
        : (r.role === 'guard' ? '【狼人杀】你是**守卫**。每晚可守一个人免于狼刀（可以守自己，但不能连着两晚守同一人）。'
          : '【狼人杀】你是**平民**。夜里没有行动，白天靠讨论与投票找出狼人来。'));
    eff.push({ type: 'private', userId: r.userId, text });
  }
  eff.push(...nightPrompts(state));
  return eff;
}

/** 夜里行动是否都收齐了（收齐就立刻结算，不干等超时）。 */
function nightReady(state) {
  const need = [];
  const guard = aliveOf(state, 'guard')[0];
  const seer = aliveOf(state, 'seer')[0];
  const wolves = aliveOf(state, 'wolf');
  if (guard) need.push(Boolean(state.pending.guard));
  if (seer) need.push(Boolean(state.pending.seer));
  for (const w of wolves) need.push(Boolean(state.pending.wolves[w.userId]));
  return need.every(Boolean);
}

/** 结算夜晚：守卫挡刀 → 平安夜；否则狼刀目标出局；公布死讯（不公布身份）。 */
function resolveNight(state, rng = Math.random, now = 0) {
  const s = JSON.parse(JSON.stringify(state));
  const rand = typeof rng === 'function' ? rng : Math.random;
  // 狼队：多数一致；平票在并列目标里随机
  const votes = Object.values(s.pending.wolves).filter(Boolean);
  const counts = new Map();
  for (const uid of votes) counts.set(uid, (counts.get(uid) || 0) + 1);
  let target = '';
  if (counts.size) {
    const top = [...counts.entries()].sort((a, b) => b[1] - a[1]);
    const best = top.filter(([, n]) => n === top[0][1]);
    target = best[Math.floor(rand() * best.length)][0];
  }
  const guarded = String(s.pending.guard || '');
  const died = target && target !== guarded ? target : '';
  const byUid = (uid) => s.roles.find((r) => r.userId === uid);
  const guardRole = aliveOf(s, 'guard')[0];
  const seerRole = aliveOf(s, 'seer')[0];
  const seerTarget = s.roles.find((r) => r.userId === String(s.pending.seer || ''));
  const detail = {
    night: s.night,
    guard: guardRole && s.pending.guard ? idxOf(s, byUid(String(s.pending.guard))) : 0,
    wolf: target ? idxOf(s, byUid(target)) : 0,
    seer: seerRole && seerTarget ? idxOf(s, seerTarget) : 0,
    seerSawWolf: seerRole && seerTarget ? seerTarget.role === 'wolf' : null,
    died: died ? idxOf(s, byUid(died)) : 0
  };
  s.nightLog = [...(s.nightLog || []), detail];
  s.lastGuard = s.pending.guard || '';
  const effects = [];
  if (died) {
    const victim = byUid(died);
    victim.alive = false;
    effects.push({ type: 'public', text: `🌅 天亮了（第 ${s.night} 夜）：${victim.name} 昨晚倒牌，身份不公布。可以说遗言，然后开始讨论。` });
  } else {
    effects.push({ type: 'public', text: `🌅 天亮了（第 ${s.night} 夜）：平安夜，昨晚没有人出局。` });
  }
  s.pending = { guard: '', wolves: {}, seer: '' };
  const win = checkWin(s);
  if (win) return { state: { ...s, phase: 'ended' }, effects: [...effects, winEffect(s, win)] };
  s.phase = 'day';
  s.cursor = 0;
  s.order = aliveList(s).map((r) => r.userId);
  s.votes = {};
  // 计时起点必须在天亮这一刻就设：置 0 会让 onTick 里 `Number(0) || now` 恒等于 now，
  // 白天第一个发言者 AFK 时 90 秒超时永不生效（与卧底第 2 轮同款坑，2026-09-29）
  s.phaseStartedAt = now || 0;
  const first = s.roles.find((r) => r.userId === s.order[0]);
  effects.push({ type: 'public', text: `第 ${s.night} 天讨论：存活 ${s.order.length} 人，从 ${first?.name || '（没人）'} 开始，每人一句发言；说完发「投 3」投票。` });
  return { state: s, effects };
}

/** 胜负：狼全灭=好人胜；狼数 ≥ 好人数=狼胜。 */
function checkWin(state) {
  const wolves = state.roles.filter((r) => r.role === 'wolf' && r.alive).length;
  const good = state.roles.filter((r) => r.role !== 'wolf' && r.alive).length;
  if (!wolves) return 'good';
  if (wolves >= good) return 'wolf';
  return '';
}

function winEffect(state, win) {
  const reveal = state.roles.map((r, i) => `${i + 1}=${r.name}（${ROLE_NAME[r.role]}${r.alive ? '' : '·已出局'}）`).join('，');
  const nights = (state.nightLog || []).map((n) => `第 ${n.night} 夜：守${n.guard || '-'}／刀${n.wolf || '-'}${n.seer ? `／查${n.seer}${n.seerSawWolf ? '（狼）' : '（好人）'}` : ''}`).join('；');
  return {
    type: 'end',
    result: `${win === 'wolf' ? '狼人获胜' : '好人获胜'}。身份：${reveal}。${nights ? `夜晚记录：${nights}` : ''}`
  };
}

/** 白天投票结算：票高者出局；平票本轮不出人。 */
function tally(state, now = 0) {
  const s = JSON.parse(JSON.stringify(state));
  const counts = new Map();
  for (const target of Object.values(s.votes)) counts.set(target, (counts.get(target) || 0) + 1);
  const effects = [];
  if (!counts.size) {
    effects.push({ type: 'public', text: '这轮没人投票，直接进入下一夜。' });
    return toNight(s, effects, now);
  }
  const ranked = [...counts.entries()].sort((a, b) => b[1] - a[1]);
  if (ranked.filter(([, n]) => n === ranked[0][1]).length > 1) {
    effects.push({ type: 'public', text: `平票（各 ${ranked[0][1]} 票），本轮不出人，直接进入下一夜。` });
    return toNight(s, effects, now);
  }
  const out = s.roles.find((r) => r.userId === ranked[0][0]);
  out.alive = false;
  effects.push({ type: 'public', text: `🗳 投票结果：${out.name} 出局（${ranked[0][1]} 票），身份不公布。要留遗言就现在。` });
  const win = checkWin(s);
  if (win) return { state: { ...s, phase: 'ended' }, effects: [...effects, winEffect(s, win)] };
  return toNight(s, effects, now);
}

function toNight(state, effects, now = 0) {
  const s = { ...state, phase: 'night', night: (state.night || 1) + 1, cursor: 0, votes: {}, phaseStartedAt: now || 0 };
  if (s.night > (state.maxNights || MAX_NIGHTS)) {
    const reveal = s.roles.map((r, i) => `${i + 1}=${r.name}（${ROLE_NAME[r.role]}）`).join('，');
    return { state: { ...s, phase: 'ended' }, effects: [...effects, { type: 'end', result: `夜晚数用尽，本局平局。身份：${reveal}` }] };
  }
  const aliveNames = aliveList(s).map((r) => r.name).join('、');
  effects.push({ type: 'public', text: `🌙 天黑请闭眼（第 ${s.night} 夜，存活：${aliveNames}）。` });
  effects.push(...nightPrompts(s));
  return { state: s, effects };
}

/** 群消息：白天的发言轮与投票（夜里群里闲聊不参与判定）。 */
export function onMessage(state, msg, { now = 0 } = {}) {
  const s = JSON.parse(JSON.stringify(state));
  const uid = String(msg.userId);
  const me = s.roles.find((r) => r.userId === uid);
  if (!me || !me.alive || s.phase === 'ended') return { state: s, effects: [] };

  if (s.phase === 'day') {
    if (s.order[s.cursor] !== uid) return { state: s, effects: [] };
    s.cursor += 1;
    if (!s.phaseStartedAt) s.phaseStartedAt = now;
    if (s.cursor >= s.order.length) {
      s.phase = 'vote';
      s.votes = {};
      s.phaseStartedAt = now;
      return { state: s, effects: [{ type: 'public', text: `发言结束，开始投票：发「投 3」或「投 @他」都行（存活的 ${s.order.length} 人各一票）。` }] };
    }
    return { state: s, effects: [] };
  }

  if (s.phase === 'vote') {
    const voteMatch = /投\s*@?([^\s，。！？!?,.]{1,12})/.exec(String(msg.text || ''));
    if (!voteMatch) return { state: s, effects: [] };
    const picked = parseTarget(s, voteMatch[1]);
    if (!picked) return { state: s, effects: [] };
    if (picked.userId === uid) return { state: s, effects: [{ type: 'public', text: `${me.name} 想投自己？那不算，换一个。` }] };
    s.votes[uid] = picked.userId;
    if (Object.keys(s.votes).length >= aliveList(s).length) return tally(s, now);
    return { state: s, effects: [] };
  }

  return { state: s, effects: [] };   // 夜里不看群消息
}

/** 私聊行动：夜里按角色收行动，收到就回执；解析不了返回空 effects（交回普通链路兜底）。 */
export function onPrivateMessage(state, msg, { now = 0, rng = Math.random } = {}) {
  const s = JSON.parse(JSON.stringify(state));
  const uid = String(msg.userId);
  const me = s.roles.find((r) => r.userId === uid);
  if (!me) return { state: s, effects: [] };
  if (s.phase !== 'night') return { state: s, effects: [] };   // 白天私聊照常聊天
  if (!me.alive) {
    // 出局的玩家夜里私聊：明确告诉他没行动，别去猜活人的事
    return { state: s, effects: [{ type: 'private', userId: uid, text: '【狼人杀】你已经出局了，夜里没有行动，安心等到局末看身份吧。' }] };
  }
  const num = () => idxOf(s, parseTarget(s, msg.text) || { userId: '?' }) || '?';
  if (me.role === 'villager') {
    return { state: s, effects: [{ type: 'private', userId: uid, text: '【狼人杀】夜里你没有行动，安心等到天亮（有话白天在群里说）。' }] };
  }
  const target = parseTarget(s, msg.text);
  if (!target) {
    return { state: s, effects: [{ type: 'private', userId: uid, text: `【狼人杀】没看懂你的行动：回 1~${s.roles.length} 的编号或群名片都行（例如「${me.role === 'wolf' ? '刀' : me.role === 'seer' ? '查' : '守'} 3」）。` }] };
  }
  const effects = [];
  if (me.role === 'guard') {
    if (s.lastGuard && String(s.lastGuard) === target.userId) {
      return { state: s, effects: [{ type: 'private', userId: uid, text: '【狼人杀】不能连着两晚守同一个人，今晚换一个。' }] };
    }
    s.pending.guard = target.userId;
    effects.push({ type: 'private', userId: uid, text: `✔ 已记下：今晚守 ${idxOf(s, target)} 号 ${target.name}。` });
  } else if (me.role === 'wolf') {
    s.pending.wolves[uid] = target.userId;
    effects.push({ type: 'private', userId: uid, text: `✔ 已记下你的刀口：${idxOf(s, target)} 号 ${target.name}（狼队各自提交，多数一致生效；改主意就再发一条）。` });
  } else if (me.role === 'seer') {
    s.pending.seer = target.userId;
    effects.push({ type: 'private', userId: uid, text: `🔮 查验结果：${idxOf(s, target)} 号 ${target.name} 是「${target.role === 'wolf' ? '狼人' : '好人'}」。` });
  }
  // 收齐就立刻结算夜晚（不干等 90 秒）
  if (nightReady(s)) {
    const out = resolveNight(s, rng, now);
    return { state: out.state, effects: [...effects, ...out.effects] };
  }
  return { state: s, effects };
}

export function onTick(state, { now = 0, deadline = 0, rng = Math.random } = {}) {
  if (state.phase === 'ended') return { state, effects: [] };
  const started = Number(state.phaseStartedAt) || now;
  const timeout = (meta.roundSeconds || 90) * 1000;
  if (now - started < timeout) return { state, effects: [] };

  if (state.phase === 'night') {
    // 到点用已收到的行动结算（没交的当夜空过）
    return resolveNight({ ...state, phaseStartedAt: now }, rng, now);
  }
  if (state.phase === 'day') {
    const s = JSON.parse(JSON.stringify(state));
    const uid = s.order[s.cursor];
    const who = s.roles.find((r) => r.userId === uid);
    s.cursor += 1;
    s.phaseStartedAt = now;
    const effects = [{ type: 'public', text: `${who?.name || '有人'} 没接上，先跳过。` }];
    if (s.cursor >= s.order.length) {
      s.phase = 'vote';
      s.votes = {};
      effects.push({ type: 'public', text: `发言结束，开始投票：发「投 3」或「投 @他」。` });
    }
    return { state: s, effects };
  }
  if (state.phase === 'vote') return tally(state, now);
  return { state, effects: [] };
}

export function summaryForModel(state) {
  const list = aliveList(state).map((r) => `${idxOf(state, r)}号${r.name}`).join('、');
  const head = state.phase === 'night' ? `夜晚第 ${state.night} 夜（行动收集中）` : `第 ${state.night} 天（白天）`;
  const last = (state.nightLog || []).at(-1);
  const dawn = last ? (last.died ? `昨夜 ${last.died} 号出局` : '昨夜平安') : '';
  return `狼人杀进行中：${head}；存活 ${aliveList(state).length}/${state.roles.length} 人 —— ${list}${dawn ? `；${dawn}` : ''}。`
    + '身份、夜晚行动与查验结果都只走私聊，你只知道上面这些。';
}

export function hostBrief(state) {
  if (state.phase === 'night') return '现在是夜晚：群里可以正常闲聊，但别催行动内容（行动走私聊），也别猜谁的身份。';
  if (state.phase === 'day') return '白天讨论中：顺着大家的话接，别替人报身份、别引导投谁，按号码称呼（"3 号"）。';
  return '投票中：只报票数进度，不站队、不评价谁可疑。';
}
