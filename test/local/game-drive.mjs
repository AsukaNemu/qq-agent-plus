// 离群实测：用临时数据目录 + 假发送端，把「数字炸弹」和「谁是卧底」各跑完整一局，
// 逐条打印引擎/插件真正会发出去的消息（群里看得到的 + 私聊发词的）。不碰线上任何数据。
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-game-drive-'));
process.env.QQ_AGENT_DATA_DIR = tmp;
const CHAT = 'group:900000001';
fs.writeFileSync(path.join(tmp, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { groups: ['900000001'], private: ['1001', '1002', '1003', '1004'] },
  api: { baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'm' },
  groupGame: {
    enabled: true, chats: [CHAT], allowPrivateInvite: true,
    games: ['number-bomb', 'undercover', 'werewolf'], maxPlayers: 10, dailyLimitPerChat: 6,
    allowGamePrivateDm: true, roundSeconds: 0, revealWords: true, recruitSeconds: 0
  }
}));

const { ChatStore } = await import('../../src/core/store.js');
const { GroupGameManager } = await import('../../src/features/group-game.js');

const store = new ChatStore(0, { dataDir: tmp });
const out = [];
const history = [];   // 全程留档：show() 会把 out 清空，收尾断言用这份
const sender = {
  async sendTextBatch(chatKey, msgs, options = {}) {
    for (const m of msgs) {
      const rec = { chatKey, text: String(m), gameScoped: options.gameScoped === true };
      out.push(rec);
      history.push(rec);
    }
    return { sent: msgs.map((_, i) => ({ messageId: `m${out.length + i}` })) };
  }
};
let clock = Date.parse('2026-09-29T01:00:00+08:00');
const mgr = new GroupGameManager({ store, sender, log: () => {}, now: () => clock, rng: () => 0.24, wake: () => {} });

const say = (uid, name, text) => store.appendIncoming(CHAT, {
  mid: `d${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, ts: (clock += 1000),
  senderId: String(uid), senderName: name, text, reply: null, media: [], mentionsSelf: false, eventKind: 'message'
}, { recordOnly: true });   // 真实链路里这些消息已被编排器确认（acked）；引擎扫描不再要求 acked，
      // 但按 acked 造数据最贴近"已经处理过的一批"，不影响结论
const drain = () => { const rows = out.splice(0); return rows; };
const label = (r) => (r.chatKey === CHAT ? '群里' : `私聊→${r.chatKey.split(':')[1]}${r.gameScoped ? '[豁免]' : ''}`);

function show(title, rows) {
  console.log(`\n── ${title} ──`);
  if (!rows.length) console.log('  （无输出）');
  for (const r of rows) console.log(`  [${label(r)}] ${r.text}`);
}

// ── 数字炸弹 ──────────────────────────────────────────────────────────
console.log('=== 数字炸弹（4 人参与，炸弹由固定 rng 定为 25）===');
for (const [uid, name, t] of [[1001, '阿猫', '小鲸鱼 来个数字炸弹'], [1002, '阿狗', '我也来'], [1003, '小北', '算我一个']]) say(uid, name, t);
await mgr.tick();
const r1 = await mgr.start({ chatKey: CHAT, gameId: 'number-bomb' });
console.log('  start 返回：', JSON.stringify(r1));
show('开局', drain());

for (const [uid, name, guess] of [[1002, '阿狗', '50'], [1003, '小北', '78'], [1001, '阿猫', '12'], [1001, '阿猫', '25']]) {
  say(uid, name, guess);
  await mgr.tick();
  show(`${name} 猜 ${guess}`, drain());
}
console.log('  局面：', JSON.stringify(mgr.games.get(CHAT) || '已结束'));

// ── 谁是卧底 ───────────────────────────────────────────────────────────
console.log('\n=== 谁是卧底（4 人一局，固定 rng：词对与卧底都可复现）===');
for (const [uid, name, t] of [[1001, '阿猫', '来局谁是卧底'], [1002, '阿狗', '带我'], [1003, '小北', '带我一个'], [1004, '老四', '我也玩']]) say(uid, name, t);
await mgr.tick();
const r2 = await mgr.start({
  chatKey: CHAT, gameId: 'undercover',
  players: [{ userId: '1001', name: '阿猫' }, { userId: '1002', name: '阿狗' }, { userId: '1003', name: '小北' }, { userId: '1004', name: '老四' }]
});
console.log('  start 返回：', JSON.stringify(r2));
const started = drain();
show('开局（群里公告 + 私聊发词）', started);
const words = started.filter((r) => r.chatKey !== CHAT).map((r) => `${r.chatKey.split(':')[1]}：「${r.text.replace(/^【谁是卧底】你的词是「|」。.*$/g, '')}」`);
console.log('  私聊发出的词：', words.join('  '));

// 第一轮：按顺序各描述一句
for (const [uid, name, txt] of [
  [1001, '阿猫', '我这东西白白嫩嫩的，早上常喝'],
  [1002, '阿狗', '我这杯是凉的，夏天爽'],
  [1003, '小北', '我这碗里能加糖'],
  [1004, '老四', '我这瓶要放冰箱']
]) {
  say(uid, name, txt);
  await mgr.tick();
  show(`${name} 描述`, drain());
}
// 第一轮投票：三个人投 2 号（阿狗），阿狗投 1 号
for (const [uid, name, vote] of [[1001, '阿猫', '投 2'], [1003, '小北', '投 2'], [1004, '老四', '投 2'], [1002, '阿狗', '投 1']]) {
  say(uid, name, vote);
  await mgr.tick();
  show(`${name} ${vote}`, drain());
}
console.log('  局面：', JSON.stringify(mgr.games.get(CHAT) || '已结束'));

// 活着的继续：描述 + 投票，直到分出胜负（每人都投"除自己以外编号最小的那个"）
for (let i = 0; i < 12 && mgr.games.has(CHAT); i += 1) {
  const g = mgr.games.get(CHAT);
  if (g.state.phase === 'speak') {
    // 新模型：谁想说就说（去重靠 spoken 集合），这里让所有存活者各说一句推进
    for (const r of g.state.roles.filter((x) => !g.state.eliminated.includes(x.userId))) say(r.userId, r.name, '我这边是白色的');
  } else {
    const alive = g.state.roles.filter((r) => !g.state.eliminated.includes(r.userId));
    for (const p of alive) {
      const target = alive.find((r) => r.userId !== p.userId);
      say(p.userId, p.name, `投 ${g.state.roles.indexOf(target) + 1}`);
    }
  }
  await mgr.tick();
  show(`第 ${i + 1} 步`, drain());
}
console.log('  局面：', JSON.stringify(mgr.games.get(CHAT) || '已结束'));

// ── 狼人杀（7 人：守卫 + 女巫都在场；夜里私聊行动、白天讨论投票）────────────
console.log('\n=== 狼人杀（7 人：私聊行动 → 回执/查验/女巫两瓶药 → 天亮 → 白天投票整条链路）===');
for (const [uid, name, t] of [[1001, '阿猫', '有人玩狼人杀吗'], [1002, '阿狗', '我来'], [1003, '小北', '+1'], [1004, '老四', '带上我'], [1005, '小五', '算我'], [1006, '小六', '我也来'], [1007, '小七', '还有位置吗']]) {
  store.appendIncoming(CHAT, { mid: `w${uid}`, ts: (clock += 1000), senderId: String(uid), senderName: name, text: t, reply: null, media: [] }, { recordOnly: true });
}
const rw = await mgr.start({ chatKey: CHAT, gameId: 'werewolf' });
console.log('  start 返回：', JSON.stringify(rw));
show('开局（群公告 + 7 条身份私聊 + 夜行动提示）', drain());

const wState = () => mgr.games.get(CHAT).state;
const roleOf = (role) => wState().roles.filter((r) => r.role === role && r.alive);
const numOf = (uid) => wState().roles.findIndex((r) => r.userId === String(uid)) + 1;
// 私聊行动：走与线上一致的入口（ingest 会调的 consumePrivateAction）
const pmAction = async (uid, text) => {
  const stored = store.appendIncoming(`private:${uid}`, { mid: `pm-${uid}-${clock}`, ts: (clock += 1000), senderId: String(uid), senderName: '玩家', text, reply: null, media: [] });
  const took = await mgr.consumePrivateAction(`private:${uid}`, stored);
  show(`${uid} 私聊「${text}」${took ? '' : '（未接管 → 会落到模型兜底）'}`, drain());
};
const gsay = async (uid, name, text) => {
  store.appendIncoming(CHAT, { mid: `g-${uid}-${clock}`, ts: (clock += 1000), senderId: String(uid), senderName: name, text, reply: null, media: [] }, { recordOnly: true });
  await mgr.tick();
  show(`${name}: ${text}`, drain());
};

// 第 1 夜：狼刀预言家；守卫守一个村民（避开女巫的解药，别撞"同守同救必死"）；女巫被问后救预言家；预言家查狼 1
console.log('\n— 第 1 夜：狼刀预言家 → 守卫守村民 → 女巫用解药救人 → 预言家查验 → 平安夜 —');
{
  const seer = roleOf('seer')[0];
  const guard = roleOf('guard')[0];
  const villager = roleOf('villager')[0];
  const [w1, w2] = roleOf('wolf');
  await pmAction(w1.userId, `刀 ${numOf(seer.userId)}`);
  await pmAction(w2.userId, `刀 ${numOf(seer.userId)}`);   // 两只狼都交 → 刀口定下，引擎这才去问女巫
  await pmAction(guard.userId, `守 ${numOf(villager.userId)}`);
  await pmAction(seer.userId, `查 ${numOf(w1.userId)}`);
  await pmAction(roleOf('witch')[0].userId, '救');          // 解药：救今晚被刀的人
}
// 第 1 天：全员发言 → 投狼 1 出局
for (const r of wState().roles.filter((x) => x.alive)) await gsay(r.userId, r.name, '我先说说我的看法');
{
  const [w1] = roleOf('wolf');
  for (const r of wState().roles.filter((x) => x.alive)) {
    const target = r.userId === w1.userId ? (roleOf('seer')[0] || roleOf('villager')[0]) : w1;
    if (target) await gsay(r.userId, r.name, `投 ${numOf(target.userId)}`);
  }
}
// 第 2 夜：狼刀守卫（守卫守自己 → 挡刀）；女巫改用毒药毒最后一只狼；预言家再查一次
console.log('\n— 第 2 夜：狼刀守卫 → 守卫自守（挡刀）→ 女巫毒最后一只狼 → 狼全灭 —');
{
  const aliveW = roleOf('wolf');
  const guard = roleOf('guard')[0];
  const seer = roleOf('seer')[0];
  const witch = roleOf('witch')[0];
  if (aliveW[0]) await pmAction(aliveW[0].userId, `刀 ${numOf(guard.userId)}`);
  if (guard) await pmAction(guard.userId, `守 ${numOf(guard.userId)}`);
  if (seer) await pmAction(seer.userId, `查 ${numOf(aliveW[0].userId)}`);
  if (witch && aliveW[0]) await pmAction(witch.userId, `毒 ${numOf(aliveW[0].userId)}`);
}
// 第 2 天：若还没结束（毒药把最后一只狼毒掉就该结算好人胜），再走一轮发言投票
if (mgr.games.has(CHAT)) {
  for (const r of wState().roles.filter((x) => x.alive)) await gsay(r.userId, r.name, '我觉得再想想');
  const aliveW = roleOf('wolf');
  if (aliveW[0]) {
    for (const r of wState().roles.filter((x) => x.alive && x.userId !== aliveW[0].userId)) {
      await gsay(r.userId, r.name, `投 ${numOf(aliveW[0].userId)}`);
    }
    const other = wState().roles.find((x) => x.alive && x.userId !== aliveW[0].userId);
    if (other) await gsay(aliveW[0].userId, aliveW[0].name, `投 ${numOf(other.userId)}`);
  }
}
console.log('  狼人杀局面：', mgr.games.has(CHAT) ? '仍在进行' : '已结束（上面应有胜负结算）');

// ── 收尾断言：这个脚本在 run.mjs 里被当 PASS/FAIL 跑，不能只靠"没抛异常" ──
const asserts = [];
const groupTexts = (re) => history.filter((r) => r.chatKey === CHAT && re.test(r.text)).map((r) => r.text);
const check = (cond, why) => asserts.push({ ok: Boolean(cond), why });
check(groupTexts(/数字炸弹结束|踩中炸弹/).length > 0, '数字炸弹要有结算');
check(groupTexts(/谁是卧底结束/).length > 0, '谁是卧底要有结算');
check(groupTexts(/狼人杀结束：/).length > 0, '狼人杀要有胜负结算');
check(/夜晚记录：/.test(groupTexts(/狼人杀结束：/).join('|')), '狼人杀的结算里要有夜晚记录（刀/守/查/救/毒）');
check(groupTexts(/你是\*\*/).length === 0, '身份私聊绝不能发到群里');
check(groupTexts(/天亮了|天黑请闭眼/).length >= 2, '狼人杀至少走了两个阶段（天亮/天黑）');
const bad = asserts.filter((a) => !a.ok);
for (const a of asserts) console.log(`  ${a.ok ? 'OK  ' : 'FAIL'} ${a.why}`);
if (bad.length) {
  console.error(`\n驱动断言失败 ${bad.length} 条`);
  process.exitCode = 1;
}

console.log('\n=== 全部输出结束 ===');
mgr.stopLoop?.();
store.close();
fs.rmSync(tmp, { recursive: true, force: true });
