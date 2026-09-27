// emoji 截断与请求体安全（2026-09-27 线上问题）：
// 记忆"新建印象"把原始发言按 200 字符切片，正好把 emoji 切成两半 —— 请求体里留下孤立代理项，
// 模型网关直接 400，该群友永远建不出印象（控制台只显示"1 位失败（已保留原印象）"）。
// 覆盖：safeSlice 不切断代理对、stripLoneSurrogates 清理、chatCompletion 发出去的请求体里没有孤立代理项。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { describe, it } from 'node:test';

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-text-safety-'));
process.env.QQ_AGENT_DATA_DIR = root;
process.on('exit', () => { try { fs.rmSync(root, { recursive: true, force: true }); } catch { /* 句柄占用就算了 */ } });

const { safeSlice, stripLoneSurrogates } = await import('../src/core/util.js');
const { chatCompletion } = await import('../src/llm/llm.js');
const { DEFAULT_CONFIG, setRuntimeConfig } = await import('../src/core/config.js');

const lone = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;

describe('emoji 截断与请求体安全', () => {
  it('safeSlice 不切断代理对', () => {
    const s = 'ab😀cd';
    assert.equal(safeSlice(s, 10), s, '没超长就原样返回');
    assert.equal(safeSlice(s, 3), 'ab', '切在第 3 个位置正好落在 emoji 的高位 → 退一位');
    assert.equal(safeSlice(s, 4), 'ab😀', '切在低位之后 → emoji 完整保留');
    assert.equal(safeSlice(s, 2), 'ab');
    assert.equal(safeSlice('中文🙂天气', 3), '中文', '高位边界退一位');
    assert.equal(safeSlice('中文🙂天气', 4), '中文🙂');
    assert.equal(safeSlice('中文🙂天气', 5), '中文🙂天');
    assert.equal(safeSlice('', 5), '');
    assert.equal(safeSlice(null, 5), '');
    assert.equal(safeSlice('abc', 0), '');
    assert.equal(lone.test(safeSlice('x'.repeat(199) + '😀', 200)), false);
  });

  it('stripLoneSurrogates 只清孤立代理项，成对的 emoji 原样保留', () => {
    assert.equal(stripLoneSurrogates('好的😀开心'), '好的😀开心');
    assert.equal(stripLoneSurrogates('半个\ud83c 乱码'), '半个 乱码');
    assert.equal(stripLoneSurrogates('尾巴\ud83d'), '尾巴');
    assert.equal(stripLoneSurrogates('低位开头\udc00'), '低位开头');
  });

  it('chatCompletion 发出去的请求体里没有孤立代理项（含多模态分段）', async () => {
    const cfg = structuredClone(DEFAULT_CONFIG);
    cfg.api.model = 'probe-model';
    cfg.api.baseUrl = 'http://127.0.0.1:0';      // 会被下面的 stub 覆盖
    setRuntimeConfig(cfg);
    const { currentProviders } = await import('../src/core/providers.js');
    void currentProviders;

    const received = [];
    const server = http.createServer((req, res) => {
      let body = '';
      req.on('data', (c) => { body += c; });
      req.on('end', () => {
        received.push(body);
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ choices: [{ message: { content: 'ok' } }], usage: { total_tokens: 1 } }));
      });
    });
    await new Promise((r) => server.listen(0, '127.0.0.1', r));
    const port = server.address().port;

    const broken = `天气播报 🌡 湿度 94% \ud83c`;
    try {
      await chatCompletion({
        messages: [
          { role: 'system', content: '系统' },
          { role: 'user', content: broken },
          { role: 'user', content: [{ type: 'text', text: `分段里的坏字符 \ud83d 在此` }] }
        ],
        overrides: { baseUrl: `http://127.0.0.1:${port}`, apiKey: 'k', model: 'probe-model' }
      });
    } finally {
      server.close();
    }
    assert.equal(received.length, 1, '应该只发了一次');
    assert.equal(lone.test(received[0]), false, `请求体里不该有孤立代理项：${JSON.stringify(received[0].slice(0, 200))}`);
    const body = JSON.parse(received[0]);
    assert.ok(JSON.stringify(body.messages).includes('天气播报'), '正常内容要保留');
  });
});
