import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { formatRecallNotice, RecallNotifier } from '../src/onebot/recall-notifier.js';

describe('RecallNotifier', () => {
  it('formats the source, sender, timestamp and original content', () => {
    const text = formatRecallNotice({
      kind: 'group',
      id: '123',
      event: { operator_id: 9 },
      message: {
        mid: '77', ts: Date.UTC(2026, 0, 2, 3, 4, 5),
        senderId: '42', senderName: '群友', text: '撤回前的内容', media: []
      }
    });
    assert.match(text, /来源：群 123/);
    assert.match(text, /发送者：群友（42）/);
    assert.match(text, /消息ID：77/);
    assert.match(text, /内容：撤回前的内容/);
    assert.match(text, /撤回者：9/);
  });

  it('notifies only recorded messages and deduplicates repeated recall events', async () => {
    const stored = new Map([['group:123:77', {
      mid: '77', ts: Date.now(), senderId: '42', senderName: '群友', text: 'hello', media: []
    }]]);
    const notified = new Set();
    const calls = [];
    const store = {
      findByMid: (chatKey, mid) => stored.get(`${chatKey}:${mid}`) || null,
      hasRecallNotification: (chatKey, mid) => notified.has(`${chatKey}:${mid}`),
      markRecallNotification: (chatKey, mid) => notified.add(`${chatKey}:${mid}`),
      appendSelf: (chatKey, message) => calls.push(['appendSelf', chatKey, message]),
    };
    const onebot = {
      sendText: async (...args) => { calls.push(['sendText', ...args]); return { message_id: 100 }; },
      sendSegments: async (...args) => { calls.push(['sendSegments', ...args]); return { message_id: 101 }; }
    };
    const notifier = new RecallNotifier({ store, onebot, getTargetUin: () => '123456789' });
    const event = { notice_type: 'group_recall', group_id: 123, user_id: 42, operator_id: 42, message_id: 77 };
    assert.equal((await notifier.handle(event)).status, 'notified');
    assert.equal((await notifier.handle(event)).status, 'duplicate');
    assert.equal(calls.filter((item) => item[0] === 'sendText').length, 1);
  });

  it('does not send when the message was not recorded', async () => {
    let sent = false;
    const notifier = new RecallNotifier({
      store: { findByMid: () => null },
      onebot: { sendText: async () => { sent = true; } },
      getTargetUin: () => '123456789'
    });
    assert.equal((await notifier.handle({ notice_type: 'friend_recall', user_id: 42, message_id: 77 })).status, 'not-found');
    assert.equal(sent, false);
  });
});
