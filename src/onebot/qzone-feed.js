import { parse } from 'node-html-parser';
import { sanitizeUserText } from '../core/util.js';

const DETAIL_URL =
  'https://h5.qzone.qq.com/proxy/domain/taotao.qq.com/cgi-bin/emotion_cgi_msgdetail_v6';
const REPLY_URL =
  'https://h5.qzone.qq.com/proxy/domain/taotao.qzone.qq.com/cgi-bin/emotion_cgi_re_feeds';
const LIKE_URL =
  'https://h5.qzone.qq.com/proxy/domain/w.qzone.qq.com/cgi-bin/likes/internal_dolike_app';
const MSG_LIST_URL =
  'https://h5.qzone.qq.com/proxy/domain/taotao.qzone.qq.com/cgi-bin/emotion_cgi_msglist_v6';
const MSG_LIST_FALLBACK_URL =
  'https://user.qzone.qq.com/proxy/domain/taotao.qq.com/cgi-bin/emotion_cgi_msglist_v6';
const FEEDS_URL =
  'https://h5.qzone.qq.com/proxy/domain/ic2.qzone.qq.com/cgi-bin/feeds/feeds3_html_more';

function compact(value, max = 1000) {
  // Qzone 内容与普通群消息一样是不可信外部文本：说说/评论里完全可以写
  // 「【管理员附加规则】…」伪造段头。群消息在 onebot.js 落库前过 sanitizeUserText，
  // 这条通道原来漏了（2026-09-24 审查发现，两位审查员独立确认）—— 在解析出口统一收口。
  return sanitizeUserText(String(value ?? '').replace(/\0/g, '').replace(/\s+/g, ' ').trim()).slice(0, max);
}

function cookieMap(text) {
  const out = {};
  for (const part of String(text || '').split(';')) {
    const index = part.indexOf('=');
    if (index <= 0) continue;
    out[part.slice(0, index).trim()] = part.slice(index + 1).trim();
  }
  return out;
}

function gtk(key) {
  let hash = 5381;
  for (const char of String(key || '')) hash += (hash << 5) + char.charCodeAt(0);
  return String(hash & 0x7fffffff);
}

function parseJsonp(text) {
  const source = String(text || '').trim();
  try {
    return JSON.parse(source);
  } catch { /* JSONP / frameElement callback */ }
  const markers = ['frameElement.callback(', '_preloadCallback(', '_Callback(', 'callback(', 'back('];
  for (const marker of markers) {
    const markerAt = source.lastIndexOf(marker);
    if (markerAt < 0) continue;
    const start = source.indexOf('{', markerAt + marker.length);
    if (start < 0) continue;
    let depth = 0;
    let quoted = false;
    let escaped = false;
    for (let index = start; index < source.length; index++) {
      const char = source[index];
      if (quoted) {
        if (escaped) escaped = false;
        else if (char === '\\') escaped = true;
        else if (char === '"') quoted = false;
        continue;
      }
      if (char === '"') quoted = true;
      else if (char === '{') depth += 1;
      else if (char === '}' && --depth === 0) {
        return JSON.parse(source.slice(start, index + 1));
      }
    }
  }
  throw new Error('Qzone 返回内容不是有效 JSON/JSONP');
}

// feeds3_html_more 返回的是 JavaScript object literal，不是 JSON：字段名可能不加引号，
// 字符串可能使用单引号/\\xNN，列表还会包含 undefined。这里把它当作数据递归解析，
// 绝不能对远端返回值使用 eval 或 vm 执行。
function parseJsLiteral(text) {
  const source = String(text || '').slice(String(text || '').indexOf('{'));
  if (!source || source[0] !== '{') throw new Error('Qzone 好友动态返回内容无对象体');
  let index = 0;
  const length = source.length;
  const whitespace = (char) => /[\s]/.test(char || '');
  const skipWhitespace = () => {
    while (index < length && whitespace(source[index])) index += 1;
  };
  const parseString = (quote) => {
    index += 1;
    let value = '';
    while (index < length) {
      const char = source[index];
      if (char === quote) {
        index += 1;
        return value;
      }
      if (char !== '\\') {
        value += char;
        index += 1;
        continue;
      }
      const escaped = source[index + 1];
      if (escaped === 'x' || escaped === 'u') {
        const size = escaped === 'x' ? 2 : 4;
        const hex = source.slice(index + 2, index + 2 + size);
        if (!new RegExp(`^[0-9a-fA-F]{${size}}$`).test(hex)) {
          throw new Error('Qzone 好友动态字符串转义无效');
        }
        value += String.fromCodePoint(Number.parseInt(hex, 16));
        index += 2 + size;
        continue;
      }
      const simple = { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f', v: '\v', '0': '\0' };
      value += simple[escaped] ?? escaped ?? '';
      index += 2;
    }
    throw new Error('Qzone 好友动态字符串未闭合');
  };
  const parseKey = () => {
    skipWhitespace();
    const char = source[index];
    if (char === '"' || char === "'") return parseString(char);
    const start = index;
    while (index < length && /[A-Za-z0-9_$]/.test(source[index])) index += 1;
    if (start === index) throw new Error('Qzone 好友动态对象键无效');
    return source.slice(start, index);
  };
  const parseValue = () => {
    skipWhitespace();
    const char = source[index];
    if (char === '{') return parseObject();
    if (char === '[') return parseArray();
    if (char === '"' || char === "'") return parseString(char);
    const start = index;
    while (index < length && !/[\s,}\]:]/.test(source[index])) index += 1;
    const token = source.slice(start, index);
    if (token === 'true') return true;
    if (token === 'false') return false;
    if (token === 'null') return null;
    if (token === 'undefined') return undefined;
    if (token && !Number.isNaN(Number(token))) return Number(token);
    throw new Error(`Qzone 好友动态值无效: ${token.slice(0, 30)}`);
  };
  const parseArray = () => {
    index += 1;
    const value = [];
    skipWhitespace();
    if (source[index] === ']') {
      index += 1;
      return value;
    }
    while (index < length) {
      // 兼容 [undefined] 以及偶发的数组空洞。
      if (source[index] === ',') value.push(undefined);
      else value.push(parseValue());
      skipWhitespace();
      if (source[index] === ',') {
        index += 1;
        skipWhitespace();
        if (source[index] === ']') {
          index += 1;
          return value;
        }
        continue;
      }
      if (source[index] === ']') {
        index += 1;
        return value;
      }
      throw new Error('Qzone 好友动态数组格式无效');
    }
    throw new Error('Qzone 好友动态数组未闭合');
  };
  const parseObject = () => {
    index += 1;
    const value = {};
    skipWhitespace();
    if (source[index] === '}') {
      index += 1;
      return value;
    }
    while (index < length) {
      const key = parseKey();
      skipWhitespace();
      if (source[index] !== ':') throw new Error('Qzone 好友动态对象缺少冒号');
      index += 1;
      const child = parseValue();
      if (key !== '__proto__') value[key] = child;
      skipWhitespace();
      if (source[index] === ',') {
        index += 1;
        skipWhitespace();
        if (source[index] === '}') {
          index += 1;
          return value;
        }
        continue;
      }
      if (source[index] === '}') {
        index += 1;
        return value;
      }
      throw new Error('Qzone 好友动态对象格式无效');
    }
    throw new Error('Qzone 好友动态对象未闭合');
  };
  return parseValue();
}

function qzoneApiError(data, label) {
  for (const key of ['code', 'subcode', 'ret']) {
    if (data?.[key] == null || Number(data[key]) === 0) continue;
    return new Error(
      `Qzone ${label} ${key}=${data[key]}: ${compact(data.message || data.msg || '', 300)}`
    );
  }
  return null;
}

function removeNativeMention(value) {
  let targetUin = '';
  let targetName = '';
  const text = String(value || '').replace(
    /@\{uin:(\d+),nick:([^,}]*),[^}]*\}/g,
    (_all, uin, name) => {
      targetUin ||= String(uin);
      targetName ||= String(name);
      return '';
    }
  );
  return {
    targetUin,
    targetName: compact(targetName, 80),
    text: compact(text.replace(/\[em\][\s\S]*?\[\/em\]/g, ''), 800)
  };
}

export function qzonePostKey(post) {
  return `${String(post?.uin || '')}:${String(post?.tid || post?.key || '')}`;
}

export function qzoneCommentKey(post, comment) {
  return [
    qzonePostKey(post),
    String(comment?.parentTid || 'root'),
    String(comment?.commentId || comment?.tid || ''),
    String(comment?.uin || '')
  ].join(':');
}

export function estimateQzoneTokens(value) {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  let tokens = 0;
  for (const char of text) {
    const code = char.codePointAt(0);
    tokens += code > 0x2ff ? 1 : 0.25;
  }
  return Math.ceil(tokens);
}

function commentFromRaw(raw, parentTid = '') {
  const parsed = removeNativeMention(raw?.content);
  const tid = String(raw?.tid ?? raw?.id ?? '');
  return {
    commentId: String(
      raw?.commentid ?? raw?.comment_id ?? raw?.cid ?? raw?.commentId ?? tid
    ),
    tid,
    parentTid: String(parentTid || ''),
    uin: String(raw?.uin || ''),
    nickname: compact(raw?.name || raw?.nickname || '', 80),
    content: parsed.text,
    targetUin: parsed.targetUin,
    targetName: parsed.targetName,
    time: Number(raw?.create_time ?? raw?.createTime ?? 0) || 0
  };
}

export function parseQzoneRawComments(commentList = []) {
  const comments = [];
  for (const raw of Array.isArray(commentList) ? commentList : []) {
    const root = commentFromRaw(raw);
    if (root.commentId && root.uin) comments.push(root);
    for (const child of Array.isArray(raw?.list_3) ? raw.list_3 : []) {
      const reply = commentFromRaw(child, root.tid || root.commentId);
      if (reply.commentId && reply.uin) comments.push(reply);
    }
  }
  return comments;
}

function parsedCommentContent(item) {
  const content = item.querySelector('.comments-content');
  if (!content) return '';
  const fragment = parse(content.innerHTML);
  for (const node of fragment.querySelectorAll('.comments-op, .nickname')) node.remove();
  return compact(fragment.textContent.replace(/^[\s:：]+/, ''), 800);
}

export function parseQzoneFeed(feed) {
  const html = String(feed?.html || '');
  const root = parse(html);
  const comments = root.querySelectorAll('li.comments-item').map((item) => {
    let parent = item.parentNode;
    let parentTid = '';
    while (parent) {
      if (parent.classList?.contains('mod-comments-sub')) {
        let owner = parent.parentNode;
        while (owner && owner.tagName !== 'LI') owner = owner.parentNode;
        parentTid = String(owner?.getAttribute?.('data-tid') || '');
        break;
      }
      parent = parent.parentNode;
    }
    return {
      commentId: String(item.getAttribute('data-commentid') || item.getAttribute('data-tid') || ''),
      tid: String(item.getAttribute('data-tid') || ''),
      parentTid,
      uin: String(item.getAttribute('data-uin') || ''),
      nickname: compact(item.getAttribute('data-nick') || '', 80),
      content: parsedCommentContent(item),
      targetUin: '',
      targetName: '',
      time: 0
    };
  }).filter((comment) => comment.commentId && comment.uin);
  const images = root.querySelectorAll('.img-box img')
    .map((image) => String(image.getAttribute('src') || '').replace(/&amp;/g, '&'))
    .filter((url) => /^https?:\/\//i.test(url) && !/qzonestyle\.gtimg\.cn/i.test(url));
  const like = root.querySelector('.qz_like_btn_v3');
  return {
    tid: String(feed?.key || ''),
    uin: String(feed?.uin || ''),
    nickname: compact(feed?.nickname || '', 80),
    time: Number(feed?.time) || 0,
    appid: Number(feed?.appid) || 0,
    content: compact(root.querySelector('.f-info')?.textContent || '', 1200),
    images: [...new Set(images)].slice(0, 9),
    comments,
    isLiked: String(like?.getAttribute('data-islike') || '') === '1',
    likeCount: Number(like?.getAttribute('data-likecnt')) || 0
  };
}

export class QzoneWebClient {
  constructor(onebot, { fetchImpl = fetch } = {}) {
    this.onebot = onebot;
    this.fetch = fetchImpl;
  }

  async #context() {
    const result = await this.onebot.call(
      'get_cookies',
      { domain: 'user.qzone.qq.com' },
      10000
    );
    const cookies = String(result?.cookies || result?.cookie || '');
    const values = cookieMap(cookies);
    const key = values.p_skey || values.skey || '';
    if (!cookies || !key) throw new Error('无法取得有效的 Qzone Cookie');
    return {
      cookies,
      selfUin: String(this.onebot.selfId || ''),
      gtk: gtk(key)
    };
  }

  async #requestData(url, options = {}, parser = parseJsonp) {
    const response = await this.fetch(url, {
      ...options,
      redirect: 'error',
      signal: options.signal
        ? AbortSignal.any([options.signal, AbortSignal.timeout(20000)])
        : AbortSignal.timeout(20000)
    });
    if (!response.ok) throw new Error(`Qzone HTTP ${response.status}`);
    return parser(await response.text());
  }

  async #request(url, options = {}) {
    const data = await this.#requestData(url, options);
    const error = qzoneApiError(data, 'API');
    if (error) throw error;
    return data;
  }

  async getQzoneMsgList({ targetUin = this.onebot.selfId, pos = 0, num = 20 } = {}, signal) {
    const ctx = await this.#context();
    const target = String(targetUin || '');
    const offset = Math.max(0, Number(pos) || 0);
    const count = Math.min(50, Math.max(1, Number(num) || 20));
    if (!/^\d+$/.test(target)) throw new Error('Qzone 说说列表作者无效');
    const legacyUrl = `${MSG_LIST_URL}?${new URLSearchParams({
      uin: target,
      ftype: '0',
      sort: '0',
      pos: String(offset),
      num: String(count),
      replynum: '100',
      g_tk: ctx.gtk,
      callback: '_preloadCallback',
      code_version: '1',
      format: 'jsonp',
      need_private_comment: '1'
    })}`;
    const headers = {
      cookie: ctx.cookies,
      referer: `https://user.qzone.qq.com/${target}`,
      'user-agent': 'Mozilla/5.0'
    };
    let data = await this.#requestData(legacyUrl, { headers, signal });
    // 旧 h5 路由偶发返回 -10000（使用人数过多），同一请求改走 user.qzone 路由。
    if (Number(data?.code) === -10000) {
      const fallbackUrl = `${MSG_LIST_FALLBACK_URL}?${new URLSearchParams({
        uin: target,
        ftype: '0',
        sort: '0',
        pos: String(offset),
        num: String(count),
        g_tk: ctx.gtk,
        code_version: '1',
        format: 'json'
      })}`;
      data = await this.#requestData(fallbackUrl, { headers, signal });
    }
    const error = qzoneApiError(data, '说说列表');
    if (error) throw error;
    if (!Array.isArray(data?.msglist)) throw new Error('Qzone 说说列表返回格式无效');
    return {
      total: Number(data.total) || data.msglist.length,
      msglist: data.msglist.map((item) => ({
        tid: String(item?.tid || ''),
        content: compact(item?.content || '', 1200),
        time: Number(item?.created_time ?? item?.time ?? 0) || 0,
        comment_num: Number(item?.cmtnum ?? item?.comment_num ?? 0) || 0
      }))
    };
  }

  async getQzoneFeeds({ selfUin = this.onebot.selfId, pageNum = 1, count = 30 } = {}, signal) {
    const ctx = await this.#context();
    const target = String(selfUin || '');
    const page = Math.max(1, Number(pageNum) || 1);
    const limit = Math.min(50, Math.max(1, Number(count) || 30));
    if (!/^\d+$/.test(target)) throw new Error('Qzone 好友动态作者无效');
    const url = `${FEEDS_URL}?${new URLSearchParams({
      uin: target,
      scope: '0',
      view: '1',
      filter: 'all',
      flag: '1',
      applist: 'all',
      pagenum: String(page),
      count: String(limit),
      aisortEndTime: '0',
      aisortOffset: '0',
      aisortBeginTime: '0',
      begintime: '0',
      g_tk: ctx.gtk,
      callback: '_preloadCallback',
      format: 'jsonp',
      useutf8: '1',
      outputhtmlfeed: '1'
    })}`;
    const data = await this.#requestData(url, {
      headers: {
        cookie: ctx.cookies,
        referer: `https://user.qzone.qq.com/${target}`,
        'user-agent': 'Mozilla/5.0'
      },
      signal
    }, parseJsLiteral);
    const error = qzoneApiError(data, '好友动态');
    if (error) throw error;
    if (!Array.isArray(data?.data?.data)) throw new Error('Qzone 好友动态返回格式无效');
    return {
      feeds: data.data.data.filter(Boolean).map((item) => ({
        uin: String(item?.uin || ''),
        nickname: compact(item?.nickname || '', 80),
        time: Number(item?.abstime ?? item?.time ?? 0) || 0,
        appid: Number(item?.appid) || 0,
        key: String(item?.key || item?.feedskey || ''),
        html: String(item?.html || '')
      })),
      has_more: Number(data.data.hasmore) !== 0
    };
  }

  async commentPost({ ownerUin, tid, content, signal }) {
    const ctx = await this.#context();
    const owner = String(ownerUin || '');
    const postId = String(tid || '');
    const text = compact(content, 200);
    if (!/^\d+$/.test(owner) || !postId || !text) throw new Error('Qzone 评论参数不完整');
    const raw = await this.#request(`${REPLY_URL}?${new URLSearchParams({ g_tk: ctx.gtk })}`, {
      method: 'POST',
      headers: {
        cookie: ctx.cookies,
        'content-type': 'application/x-www-form-urlencoded;charset=UTF-8',
        referer: `https://user.qzone.qq.com/${owner}`,
        origin: 'https://user.qzone.qq.com',
        'user-agent': 'Mozilla/5.0'
      },
      body: new URLSearchParams({
        qzreferrer: `https://user.qzone.qq.com/${ctx.selfUin}`,
        inCharset: 'utf-8',
        outCharset: 'utf-8',
        hostUin: owner,
        format: 'json',
        ref: 'feeds',
        topicId: `${owner}_${postId}__1`,
        feedsType: '100',
        private: '0',
        paramstr: '1',
        richtype: '',
        richval: '',
        isSignIn: '',
        uin: ctx.selfUin,
        content: text,
        plat: 'qzone',
        source: 'ic',
        platformid: '52'
      }).toString(),
      signal
    });
    const commentId = raw.commentid ?? raw.commentId;
    return { commentId: commentId == null ? '' : String(commentId) };
  }

  async likePost({ ownerUin, tid, time = 0, signal }) {
    const ctx = await this.#context();
    const owner = String(ownerUin || '');
    const postId = String(tid || '');
    if (!/^\d+$/.test(owner) || !postId) throw new Error('Qzone 点赞参数不完整');
    const unikey = `http://user.qzone.qq.com/${owner}/mood/${postId}`;
    await this.#request(`${LIKE_URL}?${new URLSearchParams({ g_tk: ctx.gtk })}`, {
      method: 'POST',
      headers: {
        cookie: ctx.cookies,
        'content-type': 'application/x-www-form-urlencoded;charset=UTF-8',
        referer: `https://user.qzone.qq.com/${owner}`,
        origin: 'https://user.qzone.qq.com',
        'user-agent': 'Mozilla/5.0'
      },
      body: new URLSearchParams({
        qzreferrer: `https://user.qzone.qq.com/${ctx.selfUin}`,
        opuin: ctx.selfUin,
        unikey,
        curkey: unikey,
        appid: '311',
        typeid: '0',
        abstime: String(Number(time) || 0),
        fid: postId,
        from: '1',
        active: '0',
        fupdate: '1',
        format: 'json'
      }).toString(),
      signal
    });
    return { ok: true };
  }

  async getPostDetail(ownerUin, tid, signal) {
    const ctx = await this.#context();
    const owner = String(ownerUin || '');
    const postId = String(tid || '');
    if (!/^\d+$/.test(owner) || !postId) throw new Error('动态作者或 tid 无效');
    const url = `${DETAIL_URL}?${new URLSearchParams({
      tid: postId,
      uin: owner,
      t1_source: '1',
      not_trunc_con: '1',
      need_right: '1',
      not_adapt_outpic: '1',
      g_tk: ctx.gtk
    })}`;
    const raw = await this.#request(url, {
      headers: {
        cookie: ctx.cookies,
        referer: `https://user.qzone.qq.com/${owner}`,
        'user-agent': 'Mozilla/5.0'
      },
      signal
    });
    return {
      tid: String(raw.tid || postId),
      uin: String(raw.uin || owner),
      nickname: compact(raw.name || '', 80),
      content: compact(raw.content || '', 1200),
      time: Number(raw.created_time ?? raw.createTime ?? 0) || 0,
      comments: parseQzoneRawComments(raw.commentlist),
      commentCount: Number(raw.cmtnum) || 0
    };
  }

  async replyComment({ ownerUin, tid, comment, rootComment, content, signal }) {
    const ctx = await this.#context();
    const owner = String(ownerUin || '');
    const postId = String(tid || '');
    const root = rootComment || comment;
    if (!/^\d+$/.test(owner) || !postId || !root?.commentId || !root?.uin) {
      throw new Error('回复评论参数不完整');
    }
    if (!ctx.selfUin) throw new Error('无法确认当前登录 QQ');
    const nativeName = compact(comment?.nickname, 80).replace(/[{},]/g, '');
    const replyText = comment?.parentTid
      ? `@{uin:${comment.uin},nick:${nativeName},who:1,auto:1}${content}`
      : String(content || '');
    const url = `${REPLY_URL}?${new URLSearchParams({ g_tk: ctx.gtk })}`;
    const raw = await this.#request(url, {
      method: 'POST',
      headers: {
        cookie: ctx.cookies,
        'content-type': 'application/x-www-form-urlencoded;charset=UTF-8',
        referer: 'https://user.qzone.qq.com/',
        origin: 'https://user.qzone.qq.com',
        'user-agent': 'Mozilla/5.0'
      },
      body: new URLSearchParams({
        topicId: `${owner}_${postId}__1`,
        uin: ctx.selfUin,
        hostUin: owner,
        feedsType: '100',
        inCharset: 'utf-8',
        outCharset: 'utf-8',
        plat: 'qzone',
        source: 'ic',
        platformid: '52',
        format: 'fs',
        ref: 'feeds',
        content: replyText,
        commentId: String(root.tid || root.commentId),
        commentUin: String(root.uin),
        richval: '',
        richtype: '',
        private: '0',
        paramstr: '2',
        qzreferrer: `https://user.qzone.qq.com/${ctx.selfUin}/main`
      }).toString(),
      signal
    });
    const commentId = raw.commentid ?? raw.commentId;
    return { commentId: commentId == null ? '' : String(commentId) };
  }
}
