// 运行期表情库管理：同步 QQ 收藏表情 + 本地认知层（备注/笔记/使用计数）。
// 纯函数在 stickers.js；这里管缓存、TTL 和 OneBot 交互。
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { OneBotClient, extractMediaFromSegments } from './onebot.js';
import { DATA_DIR, getConfig } from '../core/config.js';
import { resolveSelfName } from '../core/util.js';
import {
  loadStickerStore, saveStickerStore, mergeStickerLibrary,
  findSticker, formatStickerList, applyStickerNote, markStickerUsed,
  normalizeStickerEntry
} from './stickers.js';

const STICKER_ASSET_DIR = path.join(DATA_DIR, 'sticker-assets');
const MAX_STICKER_BYTES = 8 * 1024 * 1024;

// QQ 收藏表情的写入方式见 #addToQqFavorites：
// app 进程**写不进 QQ 容器**（macOS com.apple.macl 保护，实测 EPERM），
// 所以改为让 NapCat 用 `download_file` 自己把图下进容器，再把容器内路径交给 add_custom_face。

const IMAGE_EXTENSIONS = Object.freeze({
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'image/gif': 'gif',
  'image/webp': 'webp'
});

function imageType(buffer) {
  if (
    buffer.length >= 8
    && buffer.subarray(0, 8).equals(Buffer.from('89504e470d0a1a0a', 'hex'))
  ) return 'image/png';
  if (
    buffer.length >= 3
    && buffer[0] === 0xFF
    && buffer[1] === 0xD8
    && buffer[2] === 0xFF
  ) return 'image/jpeg';
  if (buffer.length >= 6 && /^GIF8[79]a$/.test(buffer.subarray(0, 6).toString('ascii'))) {
    return 'image/gif';
  }
  if (
    buffer.length >= 12
    && buffer.subarray(0, 4).toString('ascii') === 'RIFF'
    && buffer.subarray(8, 12).toString('ascii') === 'WEBP'
  ) return 'image/webp';
  return '';
}

function cleanMetadata(value, max) {
  return String(value ?? '').replace(/\s+/g, ' ').trim().slice(0, max);
}

/** 从消息的 media 里找出与这个 URL 对应的 NapCat file 名（get_image 兜底要用）。 */
function fileForUrl(message, url) {
  const media = Array.isArray(message?.media) ? message.media : [];
  const target = String(url || '').trim();
  const exact = media.find((m) => m && String(m.url || '').trim() === target && m.file);
  if (exact) return String(exact.file);
  // URL 对不上（可能已被刷新过）时退回第一条图片的 file —— 同一条消息里基本就一张
  const first = media.find((m) => m && m.kind === 'image' && m.file);
  return first ? String(first.file) : '';
}

/** 图片地址里的稳定标识（fileid 参数），用于自动收藏去重。 */
function stickerSourceKey(url) {
  const text = String(url || '');
  const fileid = /[?&]fileid=([^&]+)/.exec(text)?.[1];
  return fileid || text.slice(0, 120);
}

import { chatCompletionWithRetry } from '../llm/llm.js';
import { safeFetchBinary, validateImageUrl } from '../llm/safe-fetch.js';
import { resolveToolCalls } from '../tools/inline-tools.js';

export class StickerManager {
  constructor(onebot) {
    this.onebot = onebot;
    this.storageError = null;
    try {
      this.entries = loadStickerStore(undefined, { strict: true });
    } catch (error) {
      this.entries = [];
      this.storageError = error;
    }
    this.syncedAt = 0;
    this.syncing = null;
    this.collectTimes = [];
  }

  get enabled() {
    return getConfig().sticker?.enabled !== false;
  }

  assertStorageWritable() {
    try {
      loadStickerStore(undefined, { strict: true });
      this.storageError = null;
    } catch (error) {
      this.storageError = error;
      throw error;
    }
  }

  saveEntries(entries) {
    this.assertStorageWritable();
    saveStickerStore(entries);
    this.entries = entries;
  }

  /** 同步 QQ 收藏表情（带 TTL 缓存；force 立即刷新）。失败时退回本地缓存。 */
  async sync(force = false) {
    if (!this.enabled) return { entries: this.entries, fromCache: true, disabled: true };
    try {
      this.assertStorageWritable();
    } catch (error) {
      return {
        entries: this.entries,
        fromCache: true,
        error: String(error?.message ?? error)
      };
    }
    const ttl = 60000;
    const now = Date.now();
    if (!force && this.syncedAt && now - this.syncedAt < ttl) {
      return { entries: this.entries, fromCache: true };
    }
    if (this.syncing) return this.syncing;
    this.syncing = (async () => {
      try {
        // 同步窗口固定按上限拉，**不能**挂在 sticker.promptMaxStickers 上 ——
        // 那个设置只决定"系统提示里常驻几条"，改小它会让同步只拉到一小截，
        // 而 mergeStickerLibrary 会把没出现在这次响应里的 QQ 收藏剪掉（连同备注、使用计数）。
        const count = 500;
        const data = await this.onebot.call('fetch_custom_face_detail', { count });
        const fetched = Array.isArray(data) ? data : (Array.isArray(data?.data) ? data.data : null);
        if (!fetched) throw new Error('fetch_custom_face_detail 返回 data 不是数组');
        // 只有拿到合法数组才合并，避免异常响应清空本地库
        const nextEntries = mergeStickerLibrary(this.entries, fetched);
        this.saveEntries(nextEntries);
        this.syncedAt = Date.now();
        return { entries: this.entries, fromCache: false };
      } catch (error) {
        // 同步失败不致命：本地缓存继续用
        return { entries: this.entries, fromCache: true, error: String(error?.message ?? error) };
      } finally {
        this.syncing = null;
      }
    })();
    return this.syncing;
  }

  async list(query = '', limit = 48, force = false) {
    const synced = await this.sync(force);
    return formatStickerList(synced.entries, query, limit);
  }

  /** 只读查看当前本地快照，不触发 QQ 同步或刷新临时 URL。 */
  peek(ref) {
    return findSticker(this.entries, ref);
  }

  async find(ref) {
    const synced = await this.sync(false);
    return findSticker(synced.entries, ref);
  }

  /** QQ 消息图片 URL 带短期 rkey；发送 AI 收藏图前按原消息刷新。 */
  async findForSend(ref) {
    const sticker = await this.find(ref);
    if (sticker?.localFile) {
      const image = this.readImage(ref);
      if (!image) return null;
      return {
        ...sticker,
        url: `base64://${image.buffer.toString('base64')}`
      };
    }
    const messageId = /^collected_(-?\d+)$/.exec(String(sticker?.id || ''))?.[1];
    if (!sticker || sticker.source !== 'ai' || !messageId) return sticker;
    try {
      const data = await this.onebot.getMsg(Number(messageId));
      const segments = Array.isArray(data?.message) ? data.message : [];
      const image = segments.find((segment) => segment?.type === 'image');
      const freshUrl = String(image?.data?.url || image?.data?.file || '').trim();
      if (/^https?:\/\//i.test(freshUrl) && freshUrl !== sticker.url) {
        const refreshed = { ...sticker, url: freshUrl, updatedAt: new Date().toISOString() };
        this.saveEntries(
          this.entries.map((entry) => entry.id === sticker.id ? refreshed : entry)
        );
        return refreshed;
      }
    } catch { /* 源消息过期就退回下面"探活"这条路 */ }
    // 刷不到新链接（原消息已过期/被撤回）的早期条目只有一条老 URL：链接可能早就 400 了。
    // 直接发出去群友看到的是一张坏图 —— 先探一下，探不通就明确报"已失效"（2026-09-27）。
    try {
      const safeUrl = await validateImageUrl(sticker.url);
      try {
        // 只取一小段探活；"响应体超过 N 字节"说明图比这个上限大 —— 链接是好的，不能判死
        // （2026-09-27 审查 P1：绝大多数表情图都 >64KiB，那样会把好链接全判死）
        const { buffer } = await safeFetchBinary(safeUrl, 64 * 1024, AbortSignal.timeout(8000));
        if (!buffer?.length) throw new Error('内容为空');
      } catch (error) {
        if (!/响应体超过/.test(String(error?.message || ''))) throw error;
      }
    } catch (error) {
      const dead = new Error(`这张的图片链接已经失效（${String(error?.message ?? error)}），发出去会是一张坏图`);
      dead.code = 'STICKER_LINK_DEAD';
      throw dead;
    }
    return sticker;
  }

  note(id, patch) {
    const result = applyStickerNote(this.entries, id, patch);
    if (result.entry) this.saveEntries(result.entries);
    return result.entry;
  }

  /**
   * 把图片字节落到托管目录（sticker-assets/），返回相对路径。
   * 两类条目共用：控制台上传的自定义表情、以及从消息里收藏下来的图（消息链接是临时的，
   * 不落盘就只能靠会过期的 URL 发出去 —— 2026-09-27 实测就有一条已 400）。
   * 顺手当校验用：非图片/超大直接抛错。
   */
  #writeAsset(imageBuffer, id) {
    const buffer = Buffer.isBuffer(imageBuffer) ? imageBuffer : Buffer.from(imageBuffer || []);
    if (!buffer.length) throw new Error('图片内容为空');
    if (buffer.length > MAX_STICKER_BYTES) throw new Error('图片不能超过 8 MiB');
    const contentType = imageType(buffer);
    if (!contentType) throw new Error('仅支持 PNG、JPEG、GIF 或 WebP 图片');
    const relativeFile = `sticker-assets/${id}.${IMAGE_EXTENSIONS[contentType]}`;
    const file = path.join(DATA_DIR, relativeFile);
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const tmp = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, buffer, { mode: 0o600 });
    fs.renameSync(tmp, file);
    return { relativeFile, file };
  }

  addManual({
    imageBuffer,
    desc = '',
    localNote = '',
    tags = [],
    usage = ''
  }) {
    this.assertStorageWritable();
    const id = `manual_${crypto.randomUUID().replaceAll('-', '').slice(0, 16)}`;
    const { relativeFile, file } = this.#writeAsset(imageBuffer, id);
    const now = new Date().toISOString();
    const entry = normalizeStickerEntry({
      id,
      resId: id,
      localFile: relativeFile,
      desc: cleanMetadata(desc, 80),
      localNote: cleanMetadata(localNote, 300),
      tags: Array.isArray(tags)
        ? tags.map((tag) => cleanMetadata(tag, 40)).filter(Boolean).slice(0, 20)
        : [],
      usage: cleanMetadata(usage, 300),
      source: 'manual',
      metadataEdited: true,
      createdAt: now,
      updatedAt: now
    });
    const nextEntries = [...this.entries, entry];
    try {
      this.saveEntries(nextEntries);
    } catch (error) {
      try { fs.rmSync(file, { force: true }); } catch { /* ignore */ }
      throw error;
    }
    return entry;
  }

  update(id, patch = {}) {
    const target = findSticker(this.entries, id);
    if (!target) return null;
    const index = this.entries.findIndex((entry) => entry.id === target.id);
    const next = normalizeStickerEntry({
      ...target,
      desc: patch.desc !== undefined ? cleanMetadata(patch.desc, 80) : target.desc,
      localNote: patch.localNote !== undefined
        ? cleanMetadata(patch.localNote, 300)
        : target.localNote,
      tags: patch.tags !== undefined
        ? (Array.isArray(patch.tags) ? patch.tags : [])
        : target.tags,
      usage: patch.usage !== undefined ? cleanMetadata(patch.usage, 300) : target.usage,
      metadataEdited: true,
      updatedAt: new Date().toISOString()
    });
    const nextEntries = [...this.entries];
    nextEntries[index] = next;
    this.saveEntries(nextEntries);
    return next;
  }

  remove(id) {
    const target = findSticker(this.entries, id);
    if (!target) return null;
    const nextEntries = target.source === 'qq'
      ? this.entries.map((entry) =>
        entry.id === target.id
          ? normalizeStickerEntry({ ...entry, hidden: true, updatedAt: new Date().toISOString() })
          : entry)
      : this.entries.filter((entry) => entry.id !== target.id);
    this.saveEntries(nextEntries);
    let cleanupPending = false;
    let warning = '';
    if (target.localFile) {
      try {
        fs.rmSync(path.join(DATA_DIR, target.localFile), { force: true });
      } catch (error) {
        cleanupPending = true;
        warning = `表情已从资产库移除，但图片文件清理失败：${String(error?.message ?? error)}`;
      }
    }
    return { removed: true, cleanupPending, warning };
  }

  readImage(ref) {
    const sticker = findSticker(this.entries, ref);
    if (!sticker?.localFile) return null;
    const file = path.resolve(DATA_DIR, sticker.localFile);
    const root = `${path.resolve(STICKER_ASSET_DIR)}${path.sep}`;
    if (!file.startsWith(root)) return null;
    try {
      const buffer = fs.readFileSync(file);
      const contentType = imageType(buffer);
      return contentType ? { buffer, contentType } : null;
    } catch {
      return null;
    }
  }

  markUsed(id, context = '') {
    const result = markStickerUsed(this.entries, id, context);
    if (result.entry) this.saveEntries(result.entries);
    return result.entry;
  }

  /**
   * 让模型自己挑图：看一眼这张图，判断值不值得收进表情库（像人挑表情包）。
   * 只处理别人发来的图片，同一张图只判断一次，每小时判断次数受限；
   * 判断失败/图片取不到就安静放弃，绝不影响聊天。
   */
  async autoCollect(chatKey, message) {
    const cfg = getConfig().sticker || {};
    if (cfg.autoCollect !== true || cfg.enabled === false || !this.enabled) return null;
    const media = (message?.media || []).find((item) => item?.kind === 'image' && item.url);
    if (!media) return null;
    const srcKey = String(media.file || '').trim() || stickerSourceKey(media.url);
    const urlKey = stickerSourceKey(media.url);
    if (!srcKey) return null;
    const fileKey = String(media.file || '').replace(/\.[a-z0-9]+$/i, '').toUpperCase();
    if (this.entries.some((entry) => entry.hidden !== true
      && (entry.srcKey === srcKey || entry.srcKey === urlKey
        || (fileKey && String(entry.md5 || '').toUpperCase() === fileKey)))) return null;
    if (!this.judgedKeys) this.judgedKeys = new Set();
    if (this.judgedKeys.has(srcKey)) return null;
    const now = Date.now();
    this.judgeTimes = (this.judgeTimes || []).filter((t) => now - t < 3600000);
    const cap = Math.max(1, Number(cfg.maxCollectPerHour) || 10);
    if (this.judgeTimes.length >= cap) return null;
    this.judgedKeys.add(srcKey);
    // 只增不减会随判过的图片数无限涨（约 10 张/小时封顶也一样）；超上限时丢最旧的一批
    if (this.judgedKeys.size > 2000) {
      const drop = this.judgedKeys.size - 1500;
      let removed = 0;
      for (const key of this.judgedKeys) {
        if (removed >= drop) break;
        this.judgedKeys.delete(key);
        removed += 1;
      }
    }
    this.judgeTimes.push(now);

    let pick = null;
    let usedUrl = media.url;
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      // 图片链接是一次性的：每次重新取一条新链接再下载，失败就再来一次
      const url = await this.#refreshImageUrl(message, media.url);
      usedUrl = url;
      try {
        pick = await this.#judgeSticker({ ...media, url }, message);
        break;
      } catch (error) {
        if (attempt < 3) {
          await new Promise((resolve) => {
            const timer = setTimeout(resolve, 800 * attempt);
            timer.unref?.();
          });
          continue;
        }
        // 三次都拿不到图就安静放弃，不要打扰群聊
        console.log('[sticker] 取图或判断失败（已重试 3 次），这次跳过：' + (error?.message ?? error));
        return null;
      }
    }
    if (!pick) {
      console.log('[sticker] 判断没有返回结果，这次跳过');
      return null;
    }
    console.log('[sticker] 判断：' + (pick.save ? '收下' : '不收') + ' —— ' + (pick.reason || '（没说理由）'));
    if (pick.save !== true) return null;
    const sender = String(message?.senderName || '').trim().slice(0, 12);
    const note = String(pick.note || '').trim() || (sender ? `自动收藏 · ${sender}` : '自动收藏');
    // 优先加进 QQ 收藏表情：链接稳定、QQ 客户端里也能用、发出去更可靠
    if (cfg.saveToQqFavorites !== false && !(await this.#qqFavoritesFull())) {
      const qq = await this.#addToQqFavorites(usedUrl);
      if (qq?.added) {
        // #addToQqFavorites 内部已经 sync 过一次，这里只补写备注并返回条目
        try {
          if (qq.emojiId && this.peek(qq.emojiId)) this.note(qq.emojiId, { note });
        } catch { /* 备注失败不影响收藏 */ }
        console.log('[sticker] 已加进 QQ 收藏表情：' + note);
        const saved = qq.emojiId ? this.peek(qq.emojiId) : null;
        return saved || { id: qq.emojiId || '', localNote: note, source: 'qq' };
      }
    }
    const entry = await this.collect(message?.mid, {
      url: usedUrl,
      srcKey,
      note,
      file: fileForUrl(message, usedUrl)
    });
    if (entry && !entry.srcKey) {
      entry.srcKey = srcKey;
      this.saveEntries(this.entries);
    }
    return entry;
  }

  /**
   * QQ 收藏表情的容量状态（非会员 500 个）：控制台据此说明"新收藏会进本地库（发出去是图片）"。
   * 复用 #qqFavoritesFull 的 10 分钟缓存，别多打一次接口。
   */
  async qqFavoritesState() {
    const full = await this.#qqFavoritesFull();
    const count = Number.isFinite(this.qqCount) ? this.qqCount : null;
    return { count, limit: 500, full, checkedAt: this.qqCountAt || 0 };
  }

  /** QQ 收藏表情有上限（非会员 500 个）；满了就改存本地库。结果缓存 10 分钟。 */
  async #qqFavoritesFull() {
    const now = Date.now();
    if (this.qqCountAt && now - this.qqCountAt < 600000) return this.qqFull === true;
    if (typeof this.onebot?.call !== 'function') return false;
    // 失败也记一笔（60 秒）：协议端不可达时，表情页不必每次都等它超时（2026-09-27 审查 P2）
    if (this.qqFailAt && now - this.qqFailAt < 60000) return false;
    try {
      const data = await this.onebot.call('fetch_custom_face_detail', { count: 500 }, 30000, null);
      const list = Array.isArray(data) ? data : (Array.isArray(data?.data) ? data.data : null);
      if (!list) return false;
      this.qqCountAt = now;
      this.qqCount = list.length;
      this.qqFull = list.length >= 500;
      if (this.qqFull) console.log('[sticker] QQ 收藏表情已满（' + list.length + '/500），这张改存本地库');
      return this.qqFull;
    } catch {
      this.qqFailAt = now;
      return false;
    }
  }

  /** 把图加进 QQ 收藏表情（协议端 add_custom_face）；成功返回 { added: true, emojiId }。
   *
   *  ⚠️ 沙箱：app 进程**写不进 QQ 容器**（macOS `com.apple.macl` 保护，实测 EPERM），
   *  而 NapCat 的 add_custom_face 只认「容器内的本地路径」——传 http URL 或 `base64://`
   *  都会被它当成路径去 stat → ENOENT。
   *  ✅ 解法：先让 **NapCat 自己**用 `download_file` 把图下进它的容器（返回容器内路径），
   *  再把该路径交给 add_custom_face。全程不需要 app 具备容器写权限。
   */
  async #addToQqFavorites(url) {
    if (typeof this.onebot?.call !== 'function' || !/^https?:\/\//i.test(String(url || ''))) return null;
    try {
      // 1) 让 NapCat 把图下载进它自己的容器
      //    返回形如 <QQ容器>/Data/.config/QQ/NapCat/temp/xxx.jpg
      const extMatch = /\.(png|jpe?g|gif|webp)(?:[?#]|$)/i.exec(String(url));
      const ext = extMatch ? extMatch[1].toLowerCase().replace('jpeg', 'jpg') : 'jpg';
      const dl = await this.onebot.call(
        'download_file',
        { url: String(url), name: `qqagent-${crypto.randomUUID()}.${ext}`, base64: 'false' },
        60000,
        null
      );
      const localPath = String(dl?.file || dl?.path || dl?.file_path || '').trim();
      if (!localPath) return null;
      // 2) 用容器内路径收藏
      //    NapCat 成功时返回 {"result":0,"errMsg":"success"|"","isExist":0|1} ——
      //    errMsg 可能是 "success" 而不是空，所以只能看 result；有些封装会直接回 emoji_id，也认。
      const before = new Set((this.entries || []).map((e) => String(e?.id || '')));
      const res = await this.onebot.call('add_custom_face', { file: localPath }, 60000, null);
      const emojiIdRaw = String(res?.emoji_id || res?.resId || res?.data?.emoji_id || '').trim();
      if (Number(res?.result) !== 0 && !emojiIdRaw) return null;
      // 3) 拉一次同步定位新条目：QQ 会重新编码，md5 变了，只能靠 diff
      try { await this.sync(true); } catch { /* 拉不到就等下一次同步 */ }
      const addedEntry = (this.entries || []).find((e) => !before.has(String(e?.id || '')));
      return { added: true, emojiId: emojiIdRaw || String(addedEntry?.id || '') };
    } catch (error) {
      const msg = String(error?.message ?? error);
      const maybeFull = /full|limit|上限|超过|超出|500/i.test(msg);
      console.log('[sticker] 加进 QQ 收藏失败' + (maybeFull ? '（可能收藏已满）' : '') + '，改存本地库：' + msg);
      return null;
    }
  }

  /** 存档链接会过期：能刷新就刷新一次（拿最新一条带图消息的地址），限时 20 秒。 */
  async #refreshImageUrl(message, fallback) {
    if (message?.mid == null || typeof this.onebot?.getMsg !== 'function') return fallback;
    try {
      const data = await Promise.race([
        this.onebot.getMsg(message.mid),
        new Promise((resolve) => {
          const timer = setTimeout(() => resolve(null), 20000);
          timer.unref?.();
        })
      ]);
      const segments = Array.isArray(data?.message) ? data.message : [];
      const item = extractMediaFromSegments(segments).find((x) => x.kind === 'image' && x.url);
      return item?.url ? item.url : fallback;
    } catch {
      return fallback;
    }
  }

  /**
   * 取图 → { buffer, contentType }。URL 优先；URL 失败（多半是 QQ 的 rkey 过期，
   * 服务端回 `download url has expired`）时尝试用 NapCat 的 `get_image` 兜底。
   *
   * ⚠️ **这条兜底在 macOS 上实际不生效**：macl / provenance 对服务进程**既挡写也挡读**，
   * 服务读容器内文件会 EPERM（沙箱里测试能读，服务不能 —— 别被测试环境骗了）。
   * 反向也不行：QQ 沙箱挡写容器外。唯一正路是 NapCat 的 get_rkey，
   * 但它依赖 PacketBackend，而 PacketBackend 不支持本机 QQ 版本（7.0.2-53644-arm64）。
   * 详见 tools-core.js 里 readImageViaNapCat 的说明。
   */
  async #fetchImageBuffer(url, { file = '', maxBytes = 4 * 1024 * 1024, signal } = {}) {
    try {
      const safeUrl = await validateImageUrl(url);
      const { buffer, contentType } = await safeFetchBinary(safeUrl, maxBytes, signal);
      if (!buffer?.length) throw new Error('图片内容为空');
      return { buffer, contentType };
    } catch (error) {
      const name = String(file || '').trim();
      if (!name || typeof this.onebot?.call !== 'function') throw error;
      const data = await this.onebot.call('get_image', { file: name }, 20000, signal);
      const localPath = String(data?.file || data?.path || '').trim();
      if (!localPath) throw error;
      const buffer = await fs.promises.readFile(localPath);
      if (!buffer?.length) throw error;
      return { buffer, contentType: imageType(buffer) || 'image/jpeg' };
    }
  }

  /** 把图片转成 data URL（视觉模型看的就是它）。 */
  async #stickerDataUrl(url, signal, file = '') {
    const { buffer, contentType } = await this.#fetchImageBuffer(url, {
      file,
      maxBytes: 4 * 1024 * 1024,
      signal
    });
    const mime = /^image\//.test(String(contentType || '')) ? String(contentType) : 'image/jpeg';
    return `data:${mime};base64,${buffer.toString('base64')}`;
  }

  /** 判断一张图值不值得收（工具收藏与自动收藏共用同一口径：只收真正的表情包）。 */
  async judgeImage({ url = '', message = null, signal = null } = {}) {
    if (!url) return { save: false, reason: '这条消息里没有可收藏的图片' };
    return this.#judgeSticker({ url }, message, signal);
  }

  /** 现在还能不能收藏（限频闸门）：工具层在"看图判断"之前先问一句，别白跑一次视觉调用。 */
  collectRateLimited(now = Date.now()) {
    const times = (this.collectTimes || []).filter((t) => now - t < 3600000);
    this.collectTimes = times;
    return times.length >= Math.max(1, Number(getConfig().sticker?.maxCollectPerHour) || 10);
  }

  /** 一次极小的视觉判断：这张图收不收？收的话备注写什么？ */
  async #judgeSticker(media, message, outerSignal = null) {
    const timeoutSignal = AbortSignal.timeout(90000);
    // 主运行被中止/超时后，这次视觉判断也该停（否则工具早返回了它还在跑）
    const signal = outerSignal ? AbortSignal.any([outerSignal, timeoutSignal]) : timeoutSignal;
    const dataUrl = await this.#stickerDataUrl(
      media.url,
      signal,
      media.file || fileForUrl(message, media.url)
    );
    const botName = resolveSelfName(getConfig().persona || {}, this.onebot?.selfNickname || '');
    const sender = String(message?.senderName || '群友').trim().slice(0, 20) || '群友';
    const tool = {
      type: 'function',
      function: {
        name: 'submit_sticker_pick',
        description: '提交对这张图的收藏决定。',
        parameters: {
          type: 'object',
          additionalProperties: false,
          properties: {
            save: { type: 'boolean', description: 'true=值得收进表情库；false=不值得' },
            note: { type: 'string', maxLength: 60, description: 'save=true 时写一行备注，供以后挑表情时判断贴不贴切：先写画面主体（谁/什么形象、什么表情动作），再写适合的聊天场合（用逗号分隔），20~45 字。⚠️必须描述画面本身，不要只照抄图里的文字（「S」「危」这种没用）；图里的文字有梗意就放括号里附在最后。save=false 留空' },
            reason: { type: 'string', maxLength: 40, description: '一句话说明为什么收/不收（给日志看）' }
          },
          required: ['save']
        }
      }
    };
    const messagesUsed = [
      {
        role: 'system',
        content: `你是「${botName}」，一个混在 QQ 群里的普通群友，正在看群友刚发的一张图。`
          + '判断标准只有一条：以后聊天时用得上吗。'
          + '值得收：真正的表情包——带字的梗图、猫猫狗狗、卡通形象、抽象搞笑图，能拿来表达情绪、吐槽或怼人的。'
          + '不值得收：本人或朋友的生活照、随手拍、自拍，以及跟聊天无关的截图（游戏、聊天记录、网页）、二维码、证件、广告、纯风景照。'
          + '拿不准就问自己一句：以后聊天时真会用上吗。会就用得上才收，不会就别收。'
          // 备注是「以后挑表情」的唯一依据：只写图里的字等于没写（实测出现过「S」「梆」「危」这类单字备注，
          // 导致模型挑不出贴切的表情、干脆不发）。所以这里把「怎么写备注」讲清楚。
          + '决定收时，note 要写一行真正有用的备注：先描述画面主体（谁/什么形象、什么表情和动作），'
          + '再写适合的聊天场合（如"犯懵、被点名、答不上话时用"），20~45 字。'
          + '必须描述画面本身，不要只照抄图里的文字；图里的文字有梗意就放括号里附在最后。'
      },
      {
        role: 'user',
        content: [
          { type: 'text', text: `${sender} 发的这张图，收还是不收？` },
          { type: 'image_url', image_url: { url: dataUrl } }
        ]
      }
    ];
    const ask = (messages) => chatCompletionWithRetry({
      messages,
      tools: [tool],
      toolChoice: { type: 'function', function: { name: 'submit_sticker_pick' } },
      temperature: 0.3,
      purpose: 'judge',   // 收不收这张表情 = 判断类任务
      signal,
      // 思考会先吃掉 80~595 个 token，200 会把它截断到一个字段都收不到
      maxTokens: 600
    });
    // 模型有时不用结构化 tool_calls，而是写成 <tool_call> Hermes 文本或裸 JSON。
    // 只认结构化调用会把这些决定整条丢掉（线上出现过"两次都没提交决定，这次跳过"）。
    const pickFromText = (text) => {
      const raw = String(text || '');
      const key = (name) => {
        const m = new RegExp('<parameter\\s*=\\s*' + name + '\\s*>([\\s\\S]*?)(?=<parameter\\s*=|</function>|</tool_call>|$)', 'i').exec(raw);
        return m ? m[1].trim() : undefined;
      };
      if (/<function\s*=\s*submit_sticker_pick/i.test(raw)) {
        const save = key('save');
        return { save: /^(true|是|收|yes)$/i.test(String(save ?? '')), note: key('note') || '', reason: key('reason') || '' };
      }
      const jsonMatch = raw.match(/\{[\s\S]*?"save"[\s\S]*?\}/);
      if (jsonMatch) {
        try {
          const obj = JSON.parse(jsonMatch[0]);
          if (typeof obj?.save === 'boolean') return obj;
        } catch { /* 不是合法 JSON，放弃 */ }
      }
      return null;
    };
    const pickArgs = (resp) => {
      const call = (resp?.message?.tool_calls || [])[0];
      if (call) {
        try { return JSON.parse(call.function?.arguments || '{}'); } catch { /* 参数坏了就落到文本解析 */ }
      }
      const fromText = pickFromText(resp?.message?.content) || pickFromText(resp?.message?.reasoning_content);
      if (fromText) return fromText;
      // 共享解析器兜住更多内联格式（<tool_call> 包裹的 JSON、带 name 的 JSON 等）
      const inline = resolveToolCalls(resp?.message)[0];
      if (inline?.function?.name === 'submit_sticker_pick') {
        try { return JSON.parse(inline.function.arguments || '{}'); } catch { return null; }
      }
      return null;
    };

    // 最多试 3 次：服务商的内容过滤是概率性的（同一张图多数时候能过），多给一次机会；
    // 同时把"被内容过滤"和"模型没提交"在日志里分开，便于判断到底是哪种原因。
    let response = null;
    let args = null;
    let filtered = 0;
    for (let attempt = 1; attempt <= 3 && !args; attempt += 1) {
      response = await ask(attempt === 1
        ? messagesUsed
        : [
          ...messagesUsed,
          { role: 'assistant', content: String(response?.message?.content || '（无内容）').slice(0, 200) },
          { role: 'user', content: '请用 submit_sticker_pick 工具正式提交你的决定（save: true/false）。' }
        ]);
      if (String(response?.finishReason || '') === 'content_filter') filtered += 1;
      args = pickArgs(response);
    }
    if (!args) {
      console.log('[sticker] 三次都没拿到决定'
        + (filtered ? `（其中 ${filtered} 次被服务商内容过滤：图片含敏感内容，属正常拦截）` : '')
        + '，这张跳过：' + String(response?.message?.content || '').slice(0, 100));
      return null;
    }
    if (!args) args = {};
    return {
      save: args.save === true,
      note: String(args.note || '').slice(0, 24),
      reason: String(args.reason || '').slice(0, 40)
    };
  }

  /** 收藏一条消息里的图片（本地新增条目，不入 QQ 收藏）。 */
  async collect(messageId, { url, note = '', srcKey = '', signal, file = '' } = {}) {
    note = String(note ?? '').slice(0, 300);
    if (!getConfig().sticker?.collectEnabled) throw new Error('收藏表情功能未开启');
    // 限频
    const now = Date.now();
    this.collectTimes = this.collectTimes.filter((t) => now - t < 3600000);
    if (this.collectTimes.length >= Math.max(1, Number(getConfig().sticker?.maxCollectPerHour) || 10)) {
      throw new Error('收藏太频繁了，一小时后再试');
    }
    url = String(url || '');
    if (!url) throw new Error('该消息没有可收藏的图片地址');
    const id = `collected_${messageId}`;
    const existing = this.entries.find((e) => e.id === id);
    if (existing) {
      return this.note(id, { note: String(note || '') });
    }
    // 入库即落盘：消息图片的链接是临时地址（带 rkey、随时可能 400），只存 URL 的条目
    // 过一阵子就发不出去了（实测有一条已失效）。落盘后发送走 base64，永不过期。
    let localFile = '';
    let assetFile = '';
    try {
      const { buffer } = await this.#fetchImageBuffer(url, {
        file,
        maxBytes: MAX_STICKER_BYTES,
        signal
      });
      const asset = this.#writeAsset(buffer, id);
      localFile = asset.relativeFile;
      assetFile = asset.file;
    } catch (error) {
      throw new Error(`这张图取不到（${String(error?.message ?? error)}），没有收藏`);
    }
    const entry = {
      id,
      resId: id,
      url,
      localFile,
      md5: '',
      srcKey: String(srcKey || '').trim() || stickerSourceKey(url),
      desc: String(note || '').slice(0, 20),
      localNote: String(note || ''),
      tags: [],
      usage: '',
      source: 'ai',
      useCount: 0,
      lastUsedAt: 0,
      lastContext: '',
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString()
    };
    try {
      this.saveEntries([...this.entries, entry]);
    } catch (error) {
      // 写库失败就别把刚落的图留在磁盘上（与 addManual 同款：失败要收尾干净）
      try { fs.rmSync(assetFile, { force: true }); } catch { /* 清不掉无害 */ }
      throw error;
    }
    this.collectTimes.push(now);
    return entry;
  }
}
