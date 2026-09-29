// TTS 配置接口的 Key 归属与脱敏（2026-09-29 审查 P0/P1 的回归）：
//  - 「自定义/自建」预设的 Key 必须落 keys.custom（之前落 keys.openai，运行时读不到，
//    还会把硅基流动的 Key 发给自建地址）；
//  - /api/config 不得下发 tts.keys 明文（SECRET_KEY_PATTERN 匹配不到 keys 这个字段名）。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-tts-config-api-'));
process.env.QQ_AGENT_DATA_DIR = root;
const { DEFAULT_CONFIG, updateConfig, getConfig } = await import('../src/core/config.js');
const { createApp } = await import('../src/console/app.js');
const { ttsServiceOf, ttsKeyFor } = await import('../src/llm/tts-presets.js');
const { synthesizeSpeech } = await import('../src/llm/tts.js');

async function freePort() {
  const server = http.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

test('自建网关的 Key 存进 keys.custom，运行时也按 custom 取；/api/config 不下发明文', async (t) => {
  const port = await freePort();
  const cfg = structuredClone(DEFAULT_CONFIG);
  cfg.server = { ...cfg.server, host: '127.0.0.1', port, token: '' };
  cfg.runtime.mode = 'active';
  cfg.onebot.wsUrl = 'ws://127.0.0.1:1';
  cfg.onebot.httpUrl = 'http://127.0.0.1:1';
  cfg.api = { ...cfg.api, baseUrl: 'https://example.com/v1', apiKey: 'k', model: 'm' };
  cfg.tts = { ...cfg.tts, enabled: true, baseUrl: 'https://my-gw.example/v1', model: 'cosy', voice: 'a' };
  updateConfig(cfg);
  const app = createApp({ log: () => {} });
  t.after(async () => {
    await app.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });
  await app.start();
  const request = async (route, { method = 'GET', body } = {}) => {
    const response = await fetch(`http://127.0.0.1:${port}${route}`, {
      method,
      headers: { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body)
    });
    return { status: response.status, body: await response.json() };
  };

  // 1) 用「自定义/自建」预设保存（provider=openai、地址不在预设表里、带 service=custom）
  const saved = await request('/api/config', {
    method: 'POST',
    body: { tts: { ...getConfig().tts, service: 'custom', provider: 'openai', apiKeyInput: 'sk-selfhosted' } }
  });
  assert.equal(saved.status, 200);
  // Key 明文不出现在响应里的任何地方
  assert.equal(JSON.stringify(saved.body).includes('sk-selfhosted'), false, '/api/config 响应不得含明文 Key');
  assert.deepEqual(saved.body.config?.tts?.keys, {}, 'keys 必须整体脱敏');
  assert.ok(saved.body.config?.tts?.keyServices?.includes('custom'), 'keyServices 要说明 custom 存过');
  // 落盘归属正确
  assert.equal(getConfig().tts.keys.custom, 'sk-selfhosted');
  assert.equal(getConfig().tts.keys.openai, undefined, '不能再落 keys.openai（旧 bug）');

  // 2) 运行端按同一归属取 Key：未知 openai 地址 → custom 服务
  assert.equal(ttsServiceOf({ provider: 'openai', baseUrl: 'https://my-gw.example/v1' }).id, 'custom');
  assert.equal(ttsKeyFor({ provider: 'openai', baseUrl: 'https://my-gw.example/v1', apiKey: '', keys: { custom: 'sk-selfhosted', siliconflow: 'sk-sf' } }), 'sk-selfhosted');

  // 3) 端到端：发出去的 Authorization 用的是自建那把，不是硅基流动的
  const calls = [];
  await synthesizeSpeech({
    cfg: { ...getConfig().tts, keys: { custom: 'sk-selfhosted', siliconflow: 'sk-sf' } },
    text: '归属测试',
    fetchFn: async (url, req) => {
      calls.push({ url: String(url), auth: req.headers.authorization });
      return new Response(Buffer.from('ID3ok'), { status: 200, headers: { 'content-type': 'audio/mpeg' } });
    }
  });
  assert.match(calls[0].url, /^https:\/\/my-gw\.example\/v1\/audio\/speech$/);
  assert.equal(calls[0].auth, 'Bearer sk-selfhosted');
});
