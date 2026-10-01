import { formatFullTime, safeSlice } from '../core/util.js';
import { extractMediaFromSegments } from './onebot.js';

function recallKindOf(event) {
  if (event?.notice_type === 'group_recall' && event?.group_id != null) return 'group';
  if (event?.notice_type === 'friend_recall' && event?.user_id != null) return 'private';
  return '';
}

function mediaValuesOf(media) {
  if (!media || typeof media !== 'object') return [];
  // QQ 的 URL 带短期 rkey，撤回发生时很可能已经失效；file 是 NapCat 的稳定缓存名。
  return [...new Set([
    media.localFile,
    media.cacheFile,
    media.file,
    media.url
  ].map((value) => String(value || '').trim()).filter(Boolean))];
}

function mediaSegmentOf(media, value = '') {
  if (!media || typeof media !== 'object') return null;
  if (media.kind === 'image') {
    const file = String(value || mediaValuesOf(media)[0] || '').trim();
    return file ? { type: 'image', data: { file } } : null;
  }
  if (media.kind === 'audio') {
    const file = String(value || mediaValuesOf(media)[0] || '').trim();
    return file ? { type: 'record', data: { file } } : null;
  }
  if (media.kind === 'video') {
    const file = String(value || mediaValuesOf(media)[0] || '').trim();
    return file ? { type: 'video', data: { file } } : null;
  }
  if (media.kind === 'face' && String(media.faceId || '').trim()) {
    return { type: 'face', data: { id: String(media.faceId).trim() } };
  }
  return null;
}

function mediaSegmentsOf(media) {
  return mediaValuesOf(media)
    .map((value) => mediaSegmentOf(media, value))
    .filter(Boolean);
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

  async #messageMedia(message) {
    const stored = Array.isArray(message?.media) ? message.media : [];
    if (message?.mid == null || typeof this.onebot?.getMsg !== 'function') return stored;
    try {
      // 撤回事件通常已经让 get_msg 变成空消息；若事件和查询并发，仍尽量拿一次新鲜 file/url。
      const data = await Promise.race([
        this.onebot.getMsg(message.mid),
        new Promise((resolve) => {
          const timer = setTimeout(() => resolve(null), 2500);
          timer.unref?.();
        })
      ]);
      const fresh = extractMediaFromSegments(Array.isArray(data?.message) ? data.message : [])
        .filter((item) => item.kind === 'image' || item.kind === 'audio' || item.kind === 'video');
      return fresh.length ? fresh : stored;
    } catch {
      return stored;
    }
  }

  async #getImageAlternatives(file) {
    if (!file || typeof this.onebot?.call !== 'function') return [];
    try {
      const data = await this.onebot.call('get_image', { file }, 8000);
      return [...new Set([
        data?.file,
        data?.path,
        data?.url,
        data?.data?.file,
        data?.data?.path,
        data?.data?.url
      ].map((value) => String(value || '').trim()).filter(Boolean))];
    } catch {
      return [];
    }
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
    const media = await this.#messageMedia(message);
    for (const item of media) {
      const candidates = mediaSegmentsOf(item);
      let sent = null;
      let lastError = null;
      for (const segment of candidates) {
        try {
          const data = await this.onebot.sendSegments('private', targetUin, [segment]);
          sent = { data, segment };
          break;
        } catch (error) {
          lastError = error;
          // 只在协议端明确拒绝（常见为 rkey 过期/下载失败）时尝试替代定位，避免未知网络错误造成重复发送。
          const retryable = error?.outcome === 'failed'
            || /HTTP 400|Bad Request|下载文件失败|expired|过期|找不到|not found/i.test(String(error?.message ?? error));
          if (!retryable) break;
        }
      }
      // 仍失败时，让 NapCat 根据稳定 file 名刷新本地路径或新鲜 URL，再尝试一次。
      if (!sent && item.kind === 'image') {
        const file = String(item.file || '').trim();
        const alternatives = await this.#getImageAlternatives(file);
        for (const value of alternatives) {
          if (candidates.some((candidate) => candidate.data?.file === value)) continue;
          const segment = mediaSegmentOf(item, value);
          if (!segment) continue;
          try {
            const data = await this.onebot.sendSegments('private', targetUin, [segment]);
            sent = { data, segment };
            break;
          } catch (error) {
            lastError = error;
          }
        }
      }
      if (sent) {
        const { data, segment } = sent;
        this.store.appendSelf(privateKey, {
          mid: data?.message_id ?? null,
          ts: Date.now(),
          text: `[撤回原媒体:${segment.type}]`,
          media: [{ kind: segment.type, file: segment.data?.file || '', faceId: segment.data?.id || '' }],
          eventKind: 'recall-media'
        });
        mediaSent += 1;
      } else {
        this.log(`[recall] 原媒体发送失败（${chatKey}#${mid}/${item.kind}）：${lastError?.message ?? lastError ?? '没有可用媒体地址'}`);
      }
    }
    this.emit('chat-update', privateKey);
    return { status: 'notified', chatKey, mid, mediaSent };
  }
}
