// 日志脱敏口径（src/core/redact.js）：incident-pilot 入库与 orchestrator 写 journal 共用。
// 背景：orchestrator 会把工具错误原文写进 journald，而 OneBot 的 access_token 挂在 URL
// 查询串上（src/onebot/onebot.js），真带上了就得在写日志这一步拦住。
import assert from 'node:assert/strict';
import test from 'node:test';

import { redactText } from '../src/core/redact.js';

test('redactText：Bearer 头与查询串里的令牌参数一律脱敏', () => {
  // 既有口径：Bearer 后面到空白为止全吃掉（连同结尾引号），这里保持原样不改语义
  const bearer = redactText('headers: {"authorization": "Bearer abc.def-123"}');
  assert.ok(!bearer.includes('abc.def-123'), '令牌本身不能留在文本里');
  assert.equal(bearer, 'headers: {"authorization": "Bearer [redacted]');
  // 本项目 OneBot 实际用的参数名（带下划线前缀，旧的纯 token/key 正则匹配不到）
  assert.equal(redactText('ws://127.0.0.1:3001/?access_token=SECRET123&x=1'), 'ws://127.0.0.1:3001/?access_token=[redacted]&x=1');
  assert.equal(redactText('GET /api?a=1&api_key=abcd1234'), 'GET /api?a=1&api_key=[redacted]');
  assert.equal(redactText('?apikey=k1&token=t2&secret=s3&password=p4&authorization=a5'),
    '?apikey=[redacted]&token=[redacted]&secret=[redacted]&password=[redacted]&authorization=[redacted]');
});

test('redactText：不该动的文本原样保留（只截断/去空字符）', () => {
  // 参数名相似的普通词不能误伤（前缀必须是 ? 或 &，且名字要对上）
  assert.equal(redactText('monkey=13 & tokenizer=x'), 'monkey=13 & tokenizer=x');
  assert.equal(redactText(`温度\u0000记录`), '温度记录');
  // max 截断与 trim
  assert.equal(redactText('   abcdef  ', 3), 'abc');
});
