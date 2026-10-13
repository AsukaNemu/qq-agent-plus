import { adminPages } from "./admin-pages.ts";

export interface ArchiveRecord {
  id: string;
  /** Stable bookmark key; never shown as the archive's display number. */
  key: string;
  title: string;
  en: string;
  category: string;
  module: string;
  dataSource: string;
  status: string;
  abstract: string;
  findings: string[];
}

type ModuleDescriptor = [
  title: string,
  en: string,
  category: string,
  module: string,
  source: string,
  status: string,
  abstract: string,
  findings: string[],
  id: string,
];

const moduleDescriptors: ModuleDescriptor[] = [
  ["消息档案","MESSAGE ARCHIVE","会话档案","会话","RInBot 运行时","管理页面","查看私聊和群聊消息，按页筛选会话、发送者和正文。",["查看私聊和群聊消息，按页筛选会话、发送者和正文。"],"X-001"],
  ["关系档案","RELATIONSHIP PILOT","会话档案","关系试点","RInBot 关系试点","管理页面","查看人物关系与评估记录，配置评估参数。",["查看人物关系与评估记录，配置评估参数。"],"X-005"],
  ["身份与申请","IDENTITY & REQUESTS","会话档案","身份","RInBot 身份记录","管理页面","查看人物身份，管理好友申请与群邀请。",["查看人物身份，管理好友申请与群邀请。"],"X-006"],
  ["事件流","EVENT STREAM","会话档案","实时状态","RInBot 实时事件流","管理页面","查看本次连接期间的运行事件。",["查看本次连接期间的运行事件。"],"X-007"],
  ["功能模块","FEATURE ARCHIVE","能力档案","能力","RInBot 功能模块","管理页面","管理实验功能的启停与毕业状态。",["管理实验功能的启停与毕业状态。"],"X-009"],
  ["策略引擎","POLICY ENGINE","能力档案","策略","RInBot 消息策略","管理页面","管理回复策略、时间表与实验功能。",["管理回复策略、时间表与实验功能。"],"X-010"],
  ["模型路由","MODEL ROUTING","能力档案","模型","RInBot 模型路由","管理页面","配置对话、备用与图片识别模型。",["配置对话、备用与图片识别模型。"],"X-011"],
  ["语音识别","ASR ADAPTER","能力档案","多模态","RInBot 语音识别模块","管理页面","配置语音转写服务、模型和调用限制。",["配置语音转写服务、模型和调用限制。"],"X-013"],
  ["工具执行","TOOL EXECUTOR","能力档案","工具","RInBot 工具执行模块","管理页面","配置联网搜索与语音识别。",["配置联网搜索与语音识别。"],"X-014"],
  ["贴纸管理","STICKER MANAGER","资源档案","媒体能力","RInBot 贴纸管理模块","管理页面","预览和编辑本地贴纸，按运行状态同步 QQ 收藏。",["预览和编辑本地贴纸，按运行状态同步 QQ 收藏。"],"X-015"],
  ["空间互动","QZONE INTERACTIONS","能力档案","外部互动","RInBot 空间互动模块","管理页面","查看空间互动记录，配置互动与每日内容。",["查看空间互动记录，配置互动与每日内容。"],"X-016"],
  ["任务档案","TASK ARCHIVE","自动化档案","任务","RInBot / inbox","管理页面","查看消息队列，取消未开始任务与核对未知外发。",["查看消息队列，取消未开始任务与核对未知外发。"],"X-017"],
  ["主动消息","PROACTIVE MESSAGES","自动化档案","主动消息","RInBot 主动消息模块","管理页面","查看唤醒记录，取消计划和配置运行参数。",["查看唤醒记录，取消计划和配置运行参数。"],"X-018"],
  ["未知外发对账","UNKNOWN OUTBOX","自动化档案","出站","RInBot 外发队列","管理页面","核对 QQ 中的发送结果，只记账，不重发。",["核对 QQ 中的发送结果，只记账，不重发。"],"X-019"],
  ["每日内容","DAILY CONTENT","自动化档案","主动消息","RInBot 每日内容任务","管理页面","生成内容草稿、审核发布和核对未知发布。",["生成内容草稿、审核发布和核对未知发布。"],"X-021"],
  ["系统终端","SYSTEM TERMINAL","系统档案","运行时","RInBot 运行时","管理页面","查看服务、NapCat、队列和数据状态。",["查看服务、NapCat、队列和数据状态。"],"X-033"],
  ["配置档案","CONFIGURATION","系统档案","配置","RInBot 运行配置","管理页面","配置队列参数，查看服务配置。",["配置队列参数，查看服务配置。"],"X-037"],
  ["异常台账","INCIDENT LEDGER","系统档案","异常处理","RInBot 异常台账","管理页面","处理异常、控制会话与配置告警。",["处理异常、控制会话与配置告警。"],"X-038"],
  ["切换与恢复","CUTOVER & RECOVERY","系统档案","发布","RInBot 迁移与恢复","管理页面","查看数据库版本和数据目录。",["查看数据库版本和数据目录。"],"X-040"],
  ["会话交接","CONVERSATION HANDOFF","会话档案","记忆","RInBot 记忆模块","管理页面","查看、编辑或清除会话交接。",["查看、编辑或清除会话交接。"],"X-041"],
  ["群邀请管理","GROUP INVITATIONS","能力档案","群接入","RInBot 群邀请模块","管理页面","查看群邀请与入群记录，配置接入规则。",["查看群邀请与入群记录，配置接入规则。"],"X-042"],
  ["记忆整理","MEMORY CONSOLIDATION","会话档案","记忆","RInBot / memory-consolidation","管理页面","整理人物记忆，查看版本与恢复记忆变更。",["整理人物记忆，查看版本与恢复记忆变更。"],"X-043"],
];

export const archiveColumns = ["会话档案", "能力档案", "自动化档案", "资源档案", "系统档案"];
export const categories = ["全部档案", ...archiveColumns];
export const managementNavigation = [
  { id: "overview", label: "总览", items: [{ id: "overview", label: "运行总览", title: "系统终端" }, { id: "events", label: "实时事件", title: "事件流" }] },
  { id: "conversations", label: "会话", items: [{ id: "messages", label: "消息档案", title: "消息档案" }, { id: "memory", label: "记忆档案", title: "记忆整理" }, { id: "identity", label: "身份与申请", title: "身份与申请" }] },
  { id: "capabilities", label: "能力", items: [{ id: "features", label: "功能模块", title: "策略引擎" }, { id: "models", label: "模型设置", title: "模型路由" }, { id: "tools", label: "工具与多模态", title: "工具执行" }] },
  { id: "automation", label: "自动化", items: [{ id: "tasks", label: "任务档案", title: "任务档案" }, { id: "proactive", label: "主动消息", title: "主动消息" }, { id: "qzone", label: "空间互动", title: "空间互动" }] },
  { id: "assets", label: "资源", items: [{ id: "library", label: "贴纸与媒体", title: "贴纸管理" }] },
  { id: "system", label: "系统", items: [{ id: "diagnostics", label: "诊断", title: "异常台账" }, { id: "settings", label: "全局设置", title: "配置档案" }, { id: "maintenance", label: "更新与备份", title: "切换与恢复" }] },
] as const;
const sourceLabels: Record<string, string> = {
  "RInBot / API": "RInBot 接口服务",
  "RInBot / SSE": "RInBot 实时事件流",
  "RInBot / asr": "RInBot 语音识别模块",
  "RInBot / assets": "RInBot 媒体资源模块",
  "RInBot / axum": "RInBot 接口服务",
  "RInBot / config": "RInBot 运行配置",
  "RInBot / diagnostics": "RInBot 诊断记录",
  "RInBot / export": "RInBot 档案导出",
  "RInBot / feature": "RInBot 功能模块",
  "RInBot / group-invite": "RInBot 群邀请模块",
  "RInBot / identity": "RInBot 身份记录",
  "RInBot / incident": "RInBot 异常台账",
  "RInBot / leases": "RInBot 消息队列",
  "RInBot / media": "RInBot 媒体处理",
  "RInBot / memory": "RInBot 记忆模块",
  "RInBot / migration": "RInBot 迁移与恢复",
  "RInBot / model": "RInBot 模型路由",
  "RInBot / moments": "RInBot 每日内容任务",
  "RInBot / napcat": "NapCat 接入适配器",
  "RInBot / ops": "RInBot 运维工具",
  "RInBot / outbox": "RInBot 外发队列",
  "RInBot / policy": "RInBot 消息策略",
  "RInBot / proactive": "RInBot 主动消息模块",
  "RInBot / qzone": "RInBot 空间互动模块",
  "RInBot / runtime": "RInBot 运行时",
  "RInBot / relationship": "RInBot 关系试点",
  "RInBot / scheduler": "RInBot 任务调度器",
  "RInBot / sqlite": "RInBot SQLite 数据库",
  "RInBot / sticker": "RInBot 贴纸管理模块",
  "RInBot / tools": "RInBot 工具执行模块",
  "RInBot / vision": "RInBot 图片识别模块",
  "RInBot / workflow": "RInBot 工作流模块",
  "RhineLabUI / fixed snapshot": "RhineLabUI 固定版本资源",
  "RhineLabUI / model": "RhineLabUI 三维模型",
  "SQLite inbox": "SQLite 消息收件箱",
  "SQLite 存储": "SQLite 数据库",
};
export const records: ArchiveRecord[] = moduleDescriptors
  .filter((module) => Object.hasOwn(adminPages, module[0]))
  .sort((a, b) => archiveColumns.indexOf(a[2]) - archiveColumns.indexOf(b[2]))
  .map((module, index) => {
    return {
      id: `X-${String(index + 1).padStart(3, "0")}`,
      key: module[8],
      title: module[0],
      en: module[1],
      category: module[2],
      module: module[3],
      dataSource: sourceLabels[module[4]] ?? module[4],
      status: module[5],
      abstract: module[6],
      findings: module[7],
    };
  });

/** One global ordinal is shared by the caption, counter, search and 3D label. */
export function archiveSelection(index: number) {
  const normalized = ((index % records.length) + records.length) % records.length;
  return { record: records[normalized], index: normalized, number: normalized + 1, total: records.length };
}

export function restoreSavedArchiveKeys(value: unknown) {
  const available = new Set(records.map((record) => record.key));
  return new Set(Array.isArray(value) ? value.filter((key): key is string => typeof key === "string" && available.has(key)) : []);
}

export function columnFiles(lane: number) {
  return records
    .map((record, index) => ({ record, index }))
    .filter(({ record }) => record.category === archiveColumns[lane])
    .map(({ index }) => index);
}
export function fileLocation(index: number) {
  const lane = archiveColumns.indexOf(records[index].category);
  const row = 12 + columnFiles(lane).indexOf(index);
  return { lane, row, slot: lane * 32 + row };
}
export function fileAtSlot(slot: number) {
  const files = columnFiles(Math.floor(slot / 32));
  return files[Math.max(0, Math.min(files.length - 1, (slot % 32) - 12))];
}
