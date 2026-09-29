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
    games: ['number-bomb', 'undercover'], maxPlayers: 10, dailyLimitPerChat: 6,
    roundSeconds: 0, revealWords: true
  }
}));

const { ChatStore } = await import('../../src/core/store.js');
const { GroupGameManager } = await import('../../src/features/group-game.js');

const store = new ChatStore(0, { dataDir: tmp });
const out = [];
const sender = {
  async sendTextBatch(chatKey, msgs) {
    for (const m of msgs) out.push({ chatKey, text: String(m) });
    return { sent: msgs.map((_, i) => ({ messageId: `m${out.length + i}` })) };
  }
};
let clock = Date.parse('2026-09-29T01:00:00+08:00');
const mgr = new GroupGameManager({ store, sender, log: () => {}, now: () => clock, rng: () => 0.24, wake: () => {} });

const say = (uid, name, text) => store.appendIncoming(CHAT, {
  mid: `d${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, ts: (clock += 1000),
  senderId: String(uid), senderName: name, text, reply: null, media: [], mentionsSelf: false, eventKind: 'message'
}, { recordOnly: true });   // 真实链路里这些消息已被编排器确认（acked），tick 只认 acked 的行
const drain = () => { const rows = out.splice(0); return rows; };
const label = (r) => (r.chatKey === CHAT ? '群里' : `私聊→${r.chatKey.split(':')[1]}`);

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
    const uid = g.state.order[g.state.cursor];
    const name = g.state.roles.find((r) => r.userId === uid)?.name || uid;
    say(uid, name, '我这边是白色的');
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

console.log('\n=== 全部输出结束 ===');
mgr.stopLoop?.();
store.close();
fs.rmSync(tmp, { recursive: true, force: true });
