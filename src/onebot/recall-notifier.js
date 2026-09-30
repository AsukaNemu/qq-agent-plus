import { formatFullTime, safeSlice } from '../core/util.js';

function recallKindOf(event) {
  if (event?.notice_type === 'group_recall' && event?.group_id != null) return 'group';
  if (event?.notice_type === 'friend_recall' && event?.user_id != null) return 'private';
  return '';
}

function mediaSegmentOf(media) {
  if (!media || typeof media !== 'object') return null;
  if (media.kind === 'image') {
    const file = String(media.url || media.file || '').trim();
    return file ? { type: 'image', data: { file } } : null;
  }
  if (media.kind === 'audio') {
    const file = String(media.url || media.file || '').trim();
    return file ? { type: 'record', data: { file } } : null;
  }
  if (media.kind === 'video') {
    const file = String(media.url || media.file || '').trim();
    return file ? { type: 'video', data: { file } } : null;
  }
  if (media.kind === 'face' && String(media.faceId || '').trim()) {
    return { type: 'face', data: { id: String(media.faceId).trim() } };
  }
  return null;
}

function dedupeMedia(media = []) {
  const seen = new Set();
  const result = [];
  for (const item of media) {
    const segment = mediaSegmentOf(item);
    if (!segment) continue;
    const key = `${segment.type}:${JSON.stringify(segment.data)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(segment);
  }
  return result;
}

export function formatRecallNotice({ kind, id, event, message }) {
  const source = kind === 'group' ? `群 ${id}` : `私聊 ${id}`;
  const sender = message.senderName && message.senderId
    ? `${message.senderName}（${message.senderId}）`
    : (message.senderName || message.senderId || '未知发送者');
  const operator = kind === 'group' && event?.operator_id != null
    ? `\n撤回者：${event.operator_id}`
    : '';
  const body = String(message.text || '').trim() || '[无文字内容]';
  const mediaCount = Array.isArray(message.media)
    ? dedupeMedia(message.media).length
    : 0;
  const mediaHint = mediaCount ? `\n媒体：${mediaCount} 个（随后发送）` : '';
  return [
    '【撤回消息】',
    `来源：${source}`,
    `发送者：${sender}`,
    `时间：${formatFullTime(message.ts)}`,
    `消息ID：${message.mid}`,
    `内容：${safeSlice(body, 6000)}${body.length > 6000 ? '…（内容过长，已截断）' : ''}`,
    operator,
    mediaHint
  ].filter(Boolean).join('\n');
}

/**
 * 将已记录的撤回消息通知给 admin.ownerUin。
 * 这不是阻止 QQ 撤回，而是在本地已有记录的前提下发送一份私聊副本。
 */
export class RecallNotifier {
  constructor({ store, onebot, getTargetUin, emit = () => {}, log = () => {} }) {
    this.store = store;
    this.onebot = onebot;
    this.getTargetUin = typeof getTargetUin === 'function' ? getTargetUin : () => '';
    this.emit = emit;
    this.log = log;
  }

  async handle(event) {
    const kind = recallKindOf(event);
    if (!kind || event.message_id == null) return { status: 'ignored' };

    const targetUin = String(this.getTargetUin() || '').trim();
    if (!/^\d{5,15}$/.test(targetUin)) return { status: 'disabled', reason: 'admin.ownerUin 未配置' };

    const id = String(kind === 'group' ? event.group_id : event.user_id);
    const chatKey = `${kind}:${id}`;
    const mid = String(event.message_id);
    const message = this.store.findByMid(chatKey, mid);
    if (!message) return { status: 'not-found', chatKey, mid };
    if (this.store.hasRecallNotification(chatKey, mid)) return { status: 'duplicate', chatKey, mid };

    const notice = formatRecallNotice({ kind, id, event, message });
    const privateKey = `private:${targetUin}`;
    const header = await this.onebot.sendText('private', targetUin, notice);
    this.store.appendSelf(privateKey, {
      mid: header?.message_id ?? null,
      ts: Date.now(),
      text: notice,
      eventKind: 'recall-notice'
    });

    // 先确认文字通知已送达，再标记去重；媒体失败不能导致下一次事件重复发整条通知。
    this.store.markRecallNotification(chatKey, mid);
    this.emit('chat-update', privateKey);

    let mediaSent = 0;
    for (const segment of dedupeMedia(message.media)) {
      try {
        const data = await this.onebot.sendSegments('private', targetUin, [segment]);
        this.store.appendSelf(privateKey, {
          mid: data?.message_id ?? null,
          ts: Date.now(),
          text: `[撤回原媒体:${segment.type}]`,
          media: [{ kind: segment.type, file: segment.data?.file || '', faceId: segment.data?.id || '' }],
          eventKind: 'recall-media'
        });
        mediaSent += 1;
      } catch (error) {
        this.log(`[recall] 原媒体发送失败（${chatKey}#${mid}/${segment.type}）：${error?.message ?? error}`);
      }
    }
    this.emit('chat-update', privateKey);
    return { status: 'notified', chatKey, mid, mediaSent };
  }
}
