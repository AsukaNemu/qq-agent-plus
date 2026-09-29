// 虚拟群模拟：尽量贴近真群——带人设的群友 + 真群里必然出现的噪声，把整局跑出来。
//
// 人设（都取真人会干的事）：
//   阿猫 正常玩家        阿狗 话痨（一条接一条、边说边投、改票三次）
//   小北 潜水（白天不说话，只在投票冒泡）   老四 捣乱（群里喊身份、私聊发乱码、投自己、退赛）
//   小五 中途退出        小六 AFK（私聊不回、白天不说话）
//   路人甲/路人乙 围观群众（非参与者：猜数字、喊"投 5"、夜里闲聊）
//
// 覆盖面：数字炸弹（公共游戏，路人也能猜）→ 谁是卧底（路人捣乱 + 潜水 + 退出）
//        → 狼人杀（夜行动含乱码/AFK/路人、夜里群里闲聊、天亮乱序讨论、过半"投吧"、
//          改票、投自己、投不存在的人、中途重启恢复、退出，直到分出胜负）
// 每一步都跑不变量检查：公开消息不泄身份、私聊只发给在册玩家、单人回执不超上限、
// 阶段不卡死、重启后不重放不重发提示。
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-game-sim-'));
process.env.QQ_AGENT_DATA_DIR = tmp;
const CHAT = 'group:900000002';
const PLAYERS = [
  { uid: '2001', name: '阿猫', kind: 'normal' },
  { uid: '2002', name: '阿狗', kind: 'chatty' },
  { uid: '2003', name: '小北', kind: 'lurker' },
  { uid: '2004', name: '老四', kind: 'troll' },
  { uid: '2005', name: '小五', kind: 'quitter' },
  { uid: '2006', name: '小六', kind: 'afk' },
  { uid: '2009', name: '七七', kind: 'normal' }
];
const BYSTANDERS = [{ uid: '2007', name: '路人甲' }, { uid: '2008', name: '路人乙' }];

fs.writeFileSync(path.join(tmp, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { groups: ['900000002'], private: ['2001', '2002', '2003', '2004', '2005', '2006'] },
  api: { baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'm' },
  groupGame: {
    enabled: true, chats: [CHAT], allowPrivateInvite: true, allowGamePrivateDm: true,
    games: ['number-bomb', 'undercover', 'werewolf'], maxPlayers: 10, dailyLimitPerChat: 6,
    discussSeconds: 120, roundSeconds: 0, revealWords: true, recruitSeconds: 30
  }
}));

const { ChatStore } = await import('../../src/core/store.js');
const { GroupGameManager } = await import('../../src/features/group-game.js');

const log = [];        // 完整回放（含每条消息的路由标记）
const problems = [];   // 不变量违例
const ackCount = new Map();   // 每个玩家收到的"行动回执"条数（按局内夜计）
const out = [];
let clock = Date.parse('2026-09-29T20:00:00+08:00');
const stamp = () => new Date(clock).toLocaleTimeString('zh-CN', { hour12: false });
const tick = (s = 1) => { clock += s * 1000; return clock; };

function makeManager(store) {
  const sender = {
    async sendTextBatch(chatKey, msgs, options = {}) {
      for (const m of msgs) {
        const rec = { t: stamp(), chatKey, text: String(m), gameScoped: options.gameScoped === true };
        out.push(rec);
        log.push(rec);
        checkInvariants(rec);
        // 私聊必须发给在册玩家（身份/回执/夜间提示都不例外）；豁免开启时同样只发给在册者
        if (chatKey.startsWith('private:')) {
          const uid = chatKey.split(':')[1];
          const g = mgr.games.get(CHAT);
          const roles = (g?.state?.roles || []);
          const roster = new Set(roles.map((r) => String(r.userId)));
          if (roster.size && !roster.has(uid)) problems.push(`私聊发给了非在册者 ${uid}：${String(m).slice(0, 40)}`);
          // 查验结果只能发给预言家本人
          if (/查验结果/.test(m)) {
            const who = roles.find((r) => String(r.userId) === uid);
            if (!who || who.role !== 'seer') problems.push(`查验结果发给了非预言家 ${uid}`);
          }
          // 身份/词只能发给本人（回执文案由引擎生成，这里按"你是**"识别身份私聊）
          if (/你是\*\*/.test(m) && !roles.some((r) => String(r.userId) === uid)) problems.push(`身份发给非参与者 ${uid}`);
          // 行动回执配额：每人每夜最多 4 条（含"没看懂/你没行动"），🔮 与身份提示不计
          // 回执配额识别所有"引擎在夜里回给玩家"的文案（含女巫/预言家的拒绝与提醒：
          // 这些分支以前绕过了配额，2026-09-29 审查 P1）
          if (/^(✔|【狼人杀】(没看懂|夜里你没有行动|不能连着两晚|你已经出局|简化规则|你的解药|你的毒药|没看懂毒谁|不能毒自己|今晚已经查过))/.test(String(m))) {
            const night = Number(g?.state?.night || 0);
            const key = `${uid}@${night}`;
            ackCount.set(key, (ackCount.get(key) || 0) + 1);
            if (ackCount.get(key) > 4) problems.push(`回执超上限：${uid} 第 ${night} 夜第 ${ackCount.get(key)} 条`);
          }
        }
      }
      return { sent: msgs.map((_, i) => ({ messageId: `m${out.length + i}` })) };
    }
  };
  return new GroupGameManager({ store, sender, log: () => {}, now: () => clock, rng: () => 0.24, wake: () => {} });
}

let store = new ChatStore(0, { dataDir: tmp, filename: 'sim.sqlite' });
let mgr = makeManager(store);
const BODY = { ok: true };

// 不变量：公开消息不得出现身份词/卧底的词；不得出现"没接上"
const hasRoleWord = (t) => /预言家|守卫|女巫|平民/.test(t) || /狼人(?!杀)/.test(t);
function checkInvariants(rec) {
  if (rec.chatKey !== CHAT) return;
  // 结算行例外：revealWords=true 时它**本来就该**公开身份与词（关掉开关的场景单独验）
  const isSettlement = /^(谁是卧底|狼人杀|数字炸弹)结束：/.test(rec.text);
  if (!isSettlement && hasRoleWord(rec.text)) problems.push(`公开消息出现身份词：${rec.text.slice(0, 60)}`);
  if (/没接上/.test(rec.text)) problems.push(`公开消息出现点名式"没接上"：${rec.text.slice(0, 60)}`);
}

// ── 虚拟群友动作 ──────────────────────────────────────────────────────────
function groupSay(uid, name, text, { recordOnly = true } = {}) {
  store.appendIncoming(CHAT, { mid: `g-${uid}-${clock}-${Math.random().toString(36).slice(2, 5)}`, ts: tick(), senderId: uid, senderName: name, text, reply: null, media: [] }, { recordOnly });
  log.push({ t: stamp(), chatKey: 'in', text: `${name}: ${text}` });
}
async function groupStep() { await mgr.tick(); }
async function dm(uid, text) {
  const stored = store.appendIncoming(`private:${uid}`, { mid: `d-${uid}-${clock}-${Math.random().toString(36).slice(2, 5)}`, ts: tick(), senderId: uid, senderName: PLAYERS.find((p) => p.uid === uid)?.name || uid, text, reply: null, media: [] });
  log.push({ t: stamp(), chatKey: 'in', text: `${PLAYERS.find((p) => p.uid === uid)?.name || uid} 私聊: ${text}` });
  const took = await mgr.consumePrivateAction(`private:${uid}`, stored);
  if (!took) {
    // 没接管 → 会落到模型（这里只记下来，模拟"模型接话"）
    log.push({ t: stamp(), chatKey: 'model', text: `（未接管，转普通聊天）${text.slice(0, 20)}` });
  }
  return took;
}
// 报名：模拟真群里"想玩的回一句"。batch=true 时先让所有人都喊完再 tick——
// 这样 7 人局不会在第 6 个人报名后就被"够人数即开局"截断（真人也是七嘴八舌一起喊的）
async function recruit(players, { batch = false } = {}) {
  if (batch) {
    for (const p of players) groupSay(p.uid, p.name, Math.random() < 0.5 ? '我玩' : '报名');
    await groupStep();
    return state();
  }
  for (const p of players) {
    groupSay(p.uid, p.name, Math.random() < 0.5 ? '我玩' : '报名');
    await groupStep();
    const st = state();
    if (st && st.phase !== 'recruiting') return st;
  }
  return state();
}
const state = () => mgr.games.get(CHAT)?.state;
const alive = () => {
  const st = state();
  if (!st) return [];
  // 狼人杀：roles[].alive；谁是卧底：以 eliminated 列表为准（roles 里没有 alive 字段）
  return (st.roles || []).filter((r) => (r.role ? r.alive : !(st.eliminated || []).includes(r.userId)));
};
const numOf = (uid) => (state()?.roles || []).findIndex((r) => r.userId === String(uid)) + 1;
const roleOf = (role) => (state()?.roles || []).filter((r) => r.role === role && r.alive);

function show(title) {
  console.log(`\n── ${title} ──`);
  for (const r of out.splice(0)) {
    const where = r.chatKey === CHAT ? '群里' : (r.chatKey.startsWith('private:') ? `私聊→${PLAYERS.find((p) => p.uid === r.chatKey.split(':')[1])?.name || r.chatKey.split(':')[1]}${r.gameScoped ? '[豁免]' : ''}` : r.chatKey);
    console.log(`  [${r.t}] ${where} ${r.text.replace(/\n/g, ' ')}`);
  }
  if (!out.length) console.log('  （无输出）');
}

// ── 1. 数字炸弹：公共游戏，路人也来猜 ────────────────────────────────────
console.log('=== 场景 1：数字炸弹（路人也掺和，验证"谁都能猜"）===');
for (const p of PLAYERS.slice(0, 3)) groupSay(p.uid, p.name, '来个数字炸弹');
await groupStep();
await mgr.start({ chatKey: CHAT, gameId: 'number-bomb' });
show('开局');
groupSay('2007', '路人甲', '我凑个热闹，猜 50');
await groupStep();
show('路人甲猜 50');
{
  // 炸弹由 rng 固定（0.24 → 25）：让阿猫踩中，验证结算
  groupSay('2001', '阿猫', '猜 25');
  await groupStep();
  show('阿猫猜 25（炸弹）');
}
console.log('  局面：', mgr.games.has(CHAT) ? '仍在进行' : '已结束');

// ── 2. 谁是卧底：路人捣乱 + 潜水 + 有人退出 ──────────────────────────────
console.log('\n=== 场景 2：谁是卧底（路人喊"投5"、潜水者只投票、老四中途退出）===');
for (const p of PLAYERS) groupSay(p.uid, p.name, '来局谁是卧底');
await groupStep();
await mgr.start({ chatKey: CHAT, gameId: 'undercover' });
show('报名公告（此时一条私聊都不该有）');
await recruit(PLAYERS);
show('够 4 人报名 → 发牌（词私下发给报名的人）');
const roster2 = (state()?.roles || []).map((r) => String(r.userId));
if (roster2.includes('2007') || roster2.includes('2008')) problems.push('围观群众没报名却被拉进局');
// 乱序描述：阿狗连说两条（话痨）、路人插话、小北不说话
groupSay('2002', '阿狗', '我这东西白白的');
groupSay('2001', '阿猫', '我这也白色的');
groupSay('2007', '路人甲', '你们在说啥');
groupSay('2002', '阿狗', '再补一句 挺好的');
groupSay('2004', '老四', '我这杯是凉的');
groupSay('2006', '小六', '……');
groupSay('2005', '小五', '我这瓶放冰箱');
await groupStep();
show('乱序描述 + 路人插话 + 话痨多刷');
console.log('  已描述人数：', (state()?.spoken || []).length, '/', alive().length);
// 路人投票（无效）+ 全员就绪 → 过半"投吧"开投
groupSay('2007', '路人甲', '投 5');
groupSay('2003', '小北', '投吧');
groupSay('2001', '阿猫', '投吧');
groupSay('2004', '老四', '直接投吧');
await groupStep();
show('路人喊"投5"（无效）+ 三人说"投吧"（过半 4/6? 3/6 不开）');
groupSay('2006', '小六', '投吧');
await groupStep();
show('第四人说"投吧"→ 过半开投');
// 投票：话痨改票三次、老四投自己（被拒）
const aliveIds = alive().map((r) => r.userId);
for (const uid of aliveIds) {
  const target = aliveIds.find((x) => x !== uid);
  groupSay(uid, PLAYERS.find((p) => p.uid === uid)?.name || uid, uid === '2004' ? `投 ${numOf(uid)}` : `投 ${numOf(target)}`);
  if (uid === '2002') groupSay('2002', '阿狗', `改一下，投 ${numOf(aliveIds[0])}`);
}
await groupStep();
show('投票（含投自己、改票）');
// 老四退出
groupSay('2004', '老四', '不玩了，你们玩');
await groupStep();
show('老四退出');
// 推进到结束
for (let i = 0; i < 8 && mgr.games.has(CHAT); i += 1) {
  const st = state();
  if (st.phase === 'speak') {
    for (const r of alive()) groupSay(r.userId, r.name, '就这样吧');
  } else {
    const al = alive();
    for (const r of al) {
      const target = al.find((x) => x.userId !== r.userId);
      if (target) groupSay(r.userId, r.name, `投 ${numOf(target.userId)}`);
    }
  }
  await groupStep();
  const dbg = state();
  show(`推进第 ${i + 1} 步${dbg ? `　【状态：${dbg.phase} 存活${alive().length} 发言${(dbg.spoken || []).length} 票${Object.keys(dbg.votes || {}).length}】` : '　【已结束】'}`);
}
console.log('  局面：', mgr.games.has(CHAT) ? '仍在进行' : '已结束');

// ── 3. 狼人杀：重点场景 ────────────────────────────────────────────────
console.log('\n=== 场景 3：狼人杀（夜行动噪声 + 夜里闲聊 + 乱序讨论 + 中途重启 + 退出）===');
for (const p of PLAYERS) groupSay(p.uid, p.name, '来局狼人杀');
await groupStep();
await mgr.start({ chatKey: CHAT, gameId: 'werewolf' });
show('报名公告');
await recruit(PLAYERS, { batch: true });
show('7 人报名 → 发牌（身份 7 条 + 夜行动提示）');
const roster3 = (state()?.roles || []).map((r) => String(r.userId));
if (roster3.includes('2007') || roster3.includes('2008')) problems.push('围观群众没报名却被拉进狼人杀');
{
  const wolves = roleOf('wolf');
  const seer = roleOf('seer')[0];
  const guard = roleOf('guard')[0];
  const witch = roleOf('witch')[0];
  const villager = state().roles.find((r) => r.role === 'villager' && r.alive);
  // 捣乱者（村民）私聊"刀 3" → 应被告知没行动；路人私聊 → 不接管；AFK 不回
  if (villager) await dm(villager.userId, '刀 3');
  await dm('2007', '查 1');
  // 狼一先用名片点错人、再改成预言家（此时狼二还没交 → 未锁定，允许改）
  await dm(wolves[0].userId, '刀 5号');
  await dm(wolves[0].userId, `刀 ${numOf(seer.userId)}`);
  // 狼二交 → 刀口定下（女巫被问一次）；再想改会被拒（模拟真实流程：定下后不能再改）
  if (wolves[1]) await dm(wolves[1].userId, `刀 ${numOf(seer.userId)}`);
  if (wolves[1]) await dm(wolves[1].userId, '阿猫');
  // 女巫：看到提示里的刀口，选择救（她只想救好人：这里救被刀的预言家）
  if (witch) {
    const ask = out.filter((x) => x.chatKey === `private:${witch.userId}` && /女巫行动/.test(x.text)).length;
    if (!ask) problems.push('狼刀定了却没问女巫');
    await dm(witch.userId, '救');
  }
  // 守卫守的是村民，别跟女巫的解药撞同一个人（同守同救必死那套在单测"女巫①"里覆盖）
  await dm(guard.userId, `守 ${numOf(villager.userId)}`);
  // 迟到者：局开了才来报（以前是默默忽略、没有任何反馈）
  groupSay('2007', '路人甲', '我玩我玩，带我');
  groupSay('2008', '路人乙', '我也来');
  await groupStep();
  {
    const late = out.filter((x) => /来晚/.test(x.text));
    if (!late.length) problems.push('开局后有人报名，没给"来晚了"提示');
  }
  show('迟到者报名（应有"来晚了一步"提示）');
  // 夜里群里闲聊（不参与判定）
  groupSay('2008', '路人乙', '你们夜里在忙啥');
  groupSay('2007', '路人甲', '我猜他是狼');
  await groupStep();
  show('第 1 夜：乱格式/名片/AFK + 路人闲聊');
  // 预言家最后交（收齐 → 立刻结算）
  await dm(seer.userId, `查 ${numOf(wolves[0].userId)}`);
  {
    // 真实场景断言：守卫守村民 + 女巫救被刀的预言家 → 预言家必须活着，播报必须是平安夜
    const dawn1 = out.filter((x) => x.chatKey === CHAT && /天亮了/.test(x.text)).map((x) => x.text).join('|');
    if (!roleOf('seer').length) problems.push('女巫救人没生效：预言家第 1 夜就出局了');
    if (!/平安夜/.test(dawn1)) problems.push('第 1 夜应当是平安夜，实际播报：' + dawn1.slice(0, 60));
  }
  show('第 1 夜结算 → 天亮（女巫用解药救人 → 平安夜）');
}
// 中途重启：写盘 → 新实例
{
  const before = { has: mgr.games.has(CHAT), phase: state()?.phase, night: state()?.night, alive: alive().length };
  store.close();
  store = new ChatStore(0, { dataDir: tmp, filename: 'sim.sqlite' });
  mgr = makeManager(store);
  console.log('  （模拟服务重启：新 manager 从 games.json 恢复）');
  out.length = 0;
  await mgr.tick();
  const after = { has: mgr.games.has(CHAT), phase: state()?.phase, night: state()?.night, alive: alive().length };
  if (!before.has || !after.has) problems.push('重启后局丢了（games.json 没恢复）');
  if (JSON.stringify(before) !== JSON.stringify(after)) problems.push(`重启后局面变了：${JSON.stringify(before)} → ${JSON.stringify(after)}`);
  if (out.some((x) => x.chatKey.startsWith('private:'))) problems.push('重启后重发了私聊（夜提示不该重发）');
  show('重启后的第一个 tick（不该重发夜提示/不重放旧私聊）');
}
// 白天：乱序讨论 + 话痨 + 潜水 + 路人喊投 + 过半"投吧"
{
  const al = alive();
  const last = al.at(-1);
  groupSay(last.userId, last.name, '我先说！我怀疑 1 号');
  groupSay('2002', '阿狗', '我觉得 2 号有问题');
  groupSay('2002', '阿狗', '真的 2 号一看就像');
  groupSay('2007', '路人甲', '投 5');                       // 路人投票：无效
  groupSay('2001', '阿猫', `那我先说我的看法，投 ${numOf(al[0].userId)}`);   // 边说边投
  await groupStep();
  show('白天：乱序发言、话痨刷屏、路人喊投、边说边投、潜水者不说话');
  console.log('  已发言：', (state()?.spoken || []).length, '/', alive().length, '；想开投：', (state()?.readyVote || []).length);
  groupSay('2003', '小北', '投吧');
  groupSay('2004', '老四', '投吧');
  groupSay('2001', '阿猫', '投吧');
  await groupStep();
  show('三人"投吧"（存活 6 人，3/6 不算过半 → 不开）');
  groupSay('2006', '小六', '投吧');
  await groupStep();
  show('第四人"投吧" → 过半立刻开投');
}
// 投票：改票、投自己、投不存在的人
{
  const al = alive().map((r) => r.userId);
  for (const uid of al) {
    const name = PLAYERS.find((p) => p.uid === uid)?.name || uid;
    if (uid === '2004') { groupSay(uid, name, `投 ${numOf(uid)}`); continue; }        // 投自己 → 被拒
    if (uid === '2002') { groupSay(uid, name, '投 99'); groupSay(uid, name, `投 ${numOf(al[0])}`); continue; }   // 投不存在的人 → 无效；再改
    groupSay(uid, name, `投 ${numOf(al[0])}`);
  }
  await groupStep();
  show('投票（投自己/投不存在的人/改票）');
}
// 小五退出
groupSay('2005', '小五', '我有点事，不玩了');
await groupStep();
show('小五退出');
// 继续到结束（夜里不再行动 → 靠超时结算；白天用"投吧"推进）
let nightDriven = false;
for (let i = 0; i < 14 && mgr.games.has(CHAT); i += 1) {
  const st = state();
  if (st.phase === 'night') {
    if (!nightDriven) {
      // 第一次进夜晚（第 2 夜）：把整套夜行动演出来 —— 狼刀守卫、守卫自守、
      // 女巫先想救又改成毒（覆盖=一晚只用一瓶，解药不消耗）、预言家再查
      nightDriven = true;
      const aliveW = roleOf('wolf');
      const guard2 = roleOf('guard')[0];
      const seer2 = roleOf('seer')[0];
      const witch2 = roleOf('witch')[0];
      const killMe = guard2 || seer2 || aliveW[0];
      if (aliveW[0] && killMe) await dm(aliveW[0].userId, `刀 ${numOf(killMe.userId)}`);
      if (guard2) await dm(guard2.userId, `守 ${numOf(guard2.userId)}`);
      if (witch2 && state()?.pending?.killTarget) {
        await dm(witch2.userId, '救');
        const wolfNow = (state().roles.find((r) => r.role === 'wolf' && r.alive) || {}).userId;
        if (wolfNow) await dm(witch2.userId, `毒 ${numOf(wolfNow)}`);
      }
      if (seer2 && aliveW[0]) await dm(seer2.userId, `查 ${numOf(aliveW[0].userId)}`);
      show('第 2 夜：狼刀 + 守卫自守 + 女巫「救改毒」 + 预言家查验 → 结算');
    } else {
      tick(95);   // 夜行动窗口 90 秒 → 到点按已收到的结算
      await mgr.tick();
      show(`第 ${st.night} 夜超时结算（含 AFK 不交行动）`);
    }
  } else if (st.phase === 'day') {
    const al = alive();
    for (const r of al.slice(0, Math.ceil(al.length / 2) + 1)) groupSay(r.userId, r.name, '投吧');
    await mgr.tick();
    show('白天：过半"投吧"开投');
  } else {
    const al = alive();
    for (const r of al) {
      const target = al.find((x) => x.userId !== r.userId);
      if (target) groupSay(r.userId, r.name, `投 ${numOf(target.userId)}`);
    }
    await mgr.tick();
    show('投票');
  }
}
console.log('  局面：', mgr.games.has(CHAT) ? '仍在进行' : '已结束');

// ── 3b. 出局者还能说话（真人群里死人一定会继续说）：引擎必须完全不认 ──────
console.log('\n=== 场景 3b：出局者在白天继续发言/投票/"投吧"（引擎一律不认）===');
{
  for (const p of PLAYERS) groupSay(p.uid, p.name, '再来一局狼人杀');
  await groupStep();
  const r = await mgr.start({ chatKey: CHAT, gameId: 'werewolf' });
  console.log('  start：', r.ok ? r.text : r.error);
  await recruit(PLAYERS, { batch: true });
  show('报名后发牌（第 1 夜）');
  // 第 1 夜：全员 AFK → 超时结算（平安夜）
  tick(95);
  await mgr.tick();
  show('第 1 夜全员不交行动 → 超时，平安夜');
  // 白天：过半"投吧" → 投票；故意投出一只"平民"（狼还在 → 局继续）
  const victim = (state()?.roles || []).find((x) => x.role === 'villager');
  const all = alive();
  for (const x of all.slice(0, Math.ceil(all.length / 2) + 1)) groupSay(x.userId, x.name, '投吧');
  await mgr.tick();
  show('过半"投吧" → 开始投票');
  for (const x of all) {
    // 被投的人不能投自己（会被拒）→ 他改投别人，这样票才收得齐
    const t = x.userId === victim.userId ? all.find((y) => y.userId !== victim.userId) : victim;
    groupSay(x.userId, x.name, `投 ${numOf(t.userId)}`);
  }
  await groupStep();
  show(`投票：${victim.name}（平民）出局 → 天黑`);
  // 夜里没人动 → 超时进白天：出局者这时候开口，引擎一个字都不该认
  tick(95);
  await mgr.tick();
  const before = JSON.stringify({ s: state()?.spoken, v: state()?.votes, r: state()?.readyVote, p: state()?.phase });
  groupSay(victim.userId, victim.name, '我死了也要说：我怀疑 1 号是狼');
  groupSay(victim.userId, victim.name, `投 ${numOf((alive()[0] || {}).userId)}`);
  groupSay(victim.userId, victim.name, '投吧');
  await groupStep();
  const after = JSON.stringify({ s: state()?.spoken, v: state()?.votes, r: state()?.readyVote, p: state()?.phase });
  if (before !== after) problems.push(`出局者 ${victim.name} 的话改动了局面（发言/票/"投吧"被计入）`);
  if (!log.some((x) => x.chatKey === `private:${victim.userId}` && /不再计入本局/.test(x.text))) {
    problems.push(`出局者 ${victim.name} 没收到"你出局了、之后不算数"的私聊说明`);
  }
  show('出局者继续说三道四 → 引擎完全不认（发言/票/"投吧"一个都不算）');
  // 收尾：别让它挂着占住后面的场景 4
  await mgr.stop(CHAT, '模拟收尾');
  out.length = 0;
}

// ── 4. 关掉"结算公开身份"：整局公开消息里一个字都不许有 ──────────────────
console.log('\n=== 场景 4：revealWords=false（结算也不公开身份与词）===');
{
  const fs2 = await import('node:fs');
  const cfgPath = path.join(tmp, 'config.json');
  const cfgNow = JSON.parse(fs2.readFileSync(cfgPath, 'utf8'));
  cfgNow.groupGame.revealWords = false;
  fs2.writeFileSync(cfgPath, JSON.stringify(cfgNow));
  const { updateConfig } = await import('../../src/core/config.js');
  updateConfig({ groupGame: cfgNow.groupGame });
  for (const p of PLAYERS) groupSay(p.uid, p.name, '再来一局狼人杀');
  await groupStep();
  const r = await mgr.start({ chatKey: CHAT, gameId: 'werewolf' });
  console.log('  start：', r.ok ? r.text : r.error);
  if (!r.ok) problems.push('场景 4 开局失败（后面的检查会静默通过，所以这里要拦）：' + r.error);
  await recruit(PLAYERS);
  show('报名后发牌（身份照发私聊，但结算不公开）');
  // 直接让所有狼退出 → 立刻分胜负，看结算文本
  for (const w of roleOf('wolf')) await dm(w.userId, '不玩了');
  // 注意：show() 里 out.splice(0) 会把缓冲清空 —— 结算文本必须在 show 之前取
  const settle = out.filter((x) => x.chatKey === CHAT).map((x) => x.text).join('\n');
  if (!settle.trim()) problems.push('场景 4 没拿到任何结算文本（检查用例本身，别放过）');
  if (/身份：|预言家|守卫|女巫|平民/.test(settle)) problems.push(`revealWords=false 但结算泄露身份：${settle.slice(0, 80)}`);
  show('狼全部退出 → 结算');
}

// ── 汇总 ────────────────────────────────────────────────────────────────
console.log('\n=== 不变量检查 ===');
console.log('  公开消息泄身份/点名"没接上"：', problems.filter((p) => p.includes('公开')).length);
console.log('  私聊发给非在册者：', problems.filter((p) => p.includes('非在册')).length);
console.log('  行动回执超上限：见下方明细');
console.log('  问题总数：', problems.length);
for (const p of problems.slice(0, 10)) console.log('   ✗', p);

console.log('\n=== 全部输出结束 ===');
mgr.stopLoop?.();
store.close();
fs.rmSync(tmp, { recursive: true, force: true });
process.exit(problems.length ? 1 : 0);
