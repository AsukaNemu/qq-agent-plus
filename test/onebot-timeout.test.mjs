// 发送链路超时：文本 15s、语音/图片 60s。
// 背景（2026-09-29 实机测试）：模型确实会调 send_voice，但一条 38KB（7 秒）的 mp3 走
// send_private_msg 要 16.4 秒（协议端转码 + 上传），被 15 秒超时掐掉，发出去的语音被记成
// unknown —— "发没发出去说不清"。所以媒体段必须用更长超时。
import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-onebot-timeout-'));
process.env.QQ_AGENT_DATA_DIR = dataDir;
fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({ runtime: { mode: 'active' } }));

const { OneBotClient } = await import('../src/onebot/onebot.js');

function clientWithRecorder() {
  const bot = new OneBotClient({ wsUrl: 'ws://127.0.0.1:1', httpUrl: 'http://127.0.0.1:1' });
  const seen = [];
  bot.call = async (action, params, timeoutMs) => {
    seen.push({ action, timeoutMs, segments: params?.message || [] });
    return { message_id: 1, status: 'ok', retcode: 0 };
  };
  return { bot, seen };
}

test('文本与表情用 15 秒超时，语音与图片（表情包）用 60 秒', async () => {
  const { bot, seen } = clientWithRecorder();
  await bot.sendText('group', 123, '你好');
  await bot.sendFace('group', 123, '178');
  await bot.sendRecord('private', 456, 'base64://AAAA');
  await bot.sendSticker('group', 123, 'https://example.com/s.png');

  assert.equal(seen.length, 4);
  assert.equal(seen[0].timeoutMs, 15000, '文本仍是 15 秒');
  assert.equal(seen[1].timeoutMs, 15000, '系统表情不上传文件，仍是 15 秒');
  for (const item of seen.slice(2)) {
    assert.equal(item.timeoutMs, 60000, `${item.segments.map((s) => s.type).join('+')} 段要用媒体超时`);
  }
  // 段类型没被改坏：record 仍是 record，图片仍是 image
  assert.equal(seen[2].segments[0].type, 'record');
  assert.equal(seen[3].segments[0].type, 'image');
});
