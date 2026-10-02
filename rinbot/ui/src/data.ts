import content from "../content/archives.json" with { type: "json" };

export interface ArchiveRecord {
  id: string;
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
];

const moduleDescriptors: ModuleDescriptor[] = [
  ["消息档案", "MESSAGE ARCHIVE", "会话档案", "会话", "RInBot / runtime", "已归档 · 可读取", "消息持久化、分页阅读和会话时间线。", ["消息先写入 SQLite，再进入有界 inbox 队列。", "前端只调用 Rust API，不直接读取数据库。", "SSE 变化只刷新数据，不抢占用户滚动位置。"]],
  ["私聊接入", "PRIVATE MESSAGE INBOX", "会话档案", "OneBot 接入", "RInBot / napcat", "已归档 · 可读取", "私聊消息从 OneBot 适配层进入 canonical message。", ["原始 OneBot JSON 只在 adapter 边界转换。", "conversation_id 与 message_id 共同去重。", "正文和媒体段保留可审计的规范形态。"]],
  ["群聊接入", "GROUP MESSAGE INBOX", "会话档案", "OneBot 接入", "RInBot / napcat", "已归档 · 可读取", "群聊消息、@、回复和媒体段的统一入口。", ["群聊身份与会话分开存储。", "媒体段不被错误拼进纯文本投影。", "未识别事件进入 runtime trace。"]],
  ["消息去重", "MESSAGE DEDUPLICATION", "会话档案", "存储层", "SQLite inbox", "已归档 · 可读取", "重复投递不会重复生成业务消息。", ["数据库唯一约束提供最终去重。", "队列只传递 inbox ID，不复制正文。", "重启后可从 queued 状态恢复。"]],
  ["记忆档案", "MEMORY ARCHIVE", "会话档案", "记忆", "RInBot / memory", "迁移准备", "人物、群聊和会话摘要的未来承载位置。", ["数据关闭功能后仍保留。", "摘要更新需要独立任务和审计记录。", "敏感字段不进入前端静态资源。"]],
  ["身份与申请", "IDENTITY & REQUESTS", "会话档案", "身份", "RInBot / identity", "迁移准备", "好友、身份印象和申请记录的档案入口。", ["启用状态和 graduated 状态分离。", "身份合并必须可追踪、可回滚。", "当前页面先提供只读档案位置。"]],
  ["事件流", "EVENT STREAM", "会话档案", "实时状态", "RInBot / SSE", "已归档 · 可读取", "后端事件和快照缺口的浏览入口。", ["连接恢复后重新获取完整快照。", "断线期间保留最后一次页面状态。", "事件不能成为唯一的信息传达方式。"]],
  ["消息检索", "MESSAGE SEARCH", "会话档案", "检索", "RInBot / API", "迁移准备", "按会话、发送者和时间查找消息的入口。", ["分页查询避免一次性加载全部消息。", "筛选条件保持在 URL 和页面状态中。", "后续接入真实搜索 API。"]],

  ["功能模块", "FEATURE ARCHIVE", "能力档案", "能力", "RInBot / feature", "已归档 · 可读取", "功能开关、运行状态和毕业状态的总入口。", ["实验功能独立拥有 enabled 与 graduated。", "禁用只停止副作用，不删除数据。", "每个有操作 UI 的功能拥有专属详情。"]],
  ["策略引擎", "POLICY ENGINE", "能力档案", "策略", "RInBot / policy", "已归档 · 可读取", "allow/deny、时间门控、关键词和限频策略。", ["策略与 OneBot 传输适配器分离。", "判断结果写入可审计轨迹。", "未知外部写入不自动重试。"]],
  ["模型路由", "MODEL ROUTING", "能力档案", "模型", "RInBot / model", "迁移准备", "主模型对话、副模型图片识别的路由档案。", ["主模型负责对话。", "副模型专门识别图片。", "密钥只从受保护配置读取。"]],
  ["视觉识别", "VISION ADAPTER", "能力档案", "多模态", "RInBot / vision", "迁移准备", "图片识别、尺寸限制和降级策略。", ["图片先经过大小和格式保护。", "识别失败不阻塞文本消息。", "响应内容不把密钥写入日志。"]],
  ["语音识别", "ASR ADAPTER", "能力档案", "多模态", "RInBot / asr", "迁移准备", "语音和视频音轨的识别入口。", ["资源大小和时长需要限额。", "外部服务失败时保留原消息。", "结果与原始媒体建立关联。"]],
  ["工具执行", "TOOL EXECUTOR", "能力档案", "工具", "RInBot / tools", "迁移准备", "搜索、网页和本地工具调用的审计入口。", ["工具权限独立于对话模型。", "超时和失败有明确状态。", "未知结果不进入自动发送重试。"]],
  ["贴纸管理", "STICKER MANAGER", "能力档案", "媒体能力", "RInBot / sticker", "迁移准备", "贴纸识别、收藏和发送策略。", ["真人图片政策保持在策略模块。", "资源失效只标记，不伪造可用链接。", "外发动作进入 outbox。"]],
  ["空间互动", "QZONE INTERACTIONS", "能力档案", "外部互动", "RInBot / qzone", "已保留 · 未启用", "QZone 读取、互动和失败台账的功能位置。", ["当前功能保持关闭。", "已有数据和配置不删除。", "启用前完成外部写入对账。"]],

  ["任务档案", "TASK ARCHIVE", "自动化档案", "任务", "RInBot / scheduler", "迁移准备", "定时任务、执行记录和下一次触发时间。", ["任务状态与业务数据分离。", "重复执行必须具备幂等键。", "任务暂停不删除历史记录。"]],
  ["主动消息", "PROACTIVE MESSAGES", "自动化档案", "主动消息", "RInBot / proactive", "已保留 · 未启用", "主动发言和计划消息的安全入口。", ["默认不启动定时器。", "发送前经过策略和白名单。", "发送结果进入 outbox 对账。"]],
  ["未知外发对账", "UNKNOWN OUTBOX", "自动化档案", "出站", "RInBot / outbox", "需人工对账", "网络超时或响应不明的外发记录。", ["unknown 不会自动重试。", "人工只能明确标记 sent 或 failed。", "对账动作写入审计事件。"]],
  ["计划调度", "SCHEDULE CONTROL", "自动化档案", "调度", "RInBot / scheduler", "迁移准备", "调度器、租约和恢复窗口。", ["服务重启后恢复未完成任务。", "不使用 PM2 或面板启动。", "调度写入保持可追踪。"]],
  ["每日内容", "DAILY CONTENT", "自动化档案", "主动消息", "RInBot / moments", "已保留 · 未启用", "每日内容任务的独立功能档案。", ["关闭后不创建定时器。", "已有内容仍可只读查看。", "发送动作需要单独验收。"]],
  ["QZone 任务", "QZONE JOBS", "自动化档案", "外部互动", "RInBot / qzone", "已保留 · 未启用", "QZone 任务和外部动作的调度位置。", ["读取与写入状态分离。", "外部失败保留原始原因。", "关闭不删除历史数据。"]],
  ["工作流", "WORKFLOW RUNS", "自动化档案", "编排", "RInBot / workflow", "迁移准备", "模型、工具、记忆和外发动作的编排轨迹。", ["每一步拥有独立状态。", "失败不隐式重放外部动作。", "运行记录可关联到消息和任务。"]],
  ["恢复队列", "RECOVERY QUEUE", "自动化档案", "恢复", "RInBot / leases", "已归档 · 可读取", "inbox lease 过期、恢复和重新领取。", ["租约 owner 必须匹配。", "过期记录可安全恢复。", "队列容量保持有界。"]],

  ["贴纸与媒体", "ASSET LIBRARY", "资源档案", "资源", "RInBot / assets", "迁移准备", "贴纸、图片、语音和视频资源的索引。", ["业务数据库只保存资源元数据。", "媒体处理有大小和格式上限。", "资源失效不会删除历史引用。"]],
  ["图片降采样", "IMAGE DOWNSAMPLE", "资源档案", "图像", "RInBot / media", "已归档 · 可读取", "视觉识别前的图片压缩和安全处理。", ["优先使用 sips，保留 ffmpeg 降级。", "最大尺寸和字节数受限。", "处理失败返回明确错误。"]],
  ["语音与音频", "AUDIO ASSETS", "资源档案", "音频", "RInBot / media", "迁移准备", "语音资源、音轨和识别结果的索引。", ["不把大文件直接塞入消息表。", "音频处理与 ASR 结果分离。", "资源清理必须可恢复。"]],
  ["三维查看器", "MODEL VIEWER", "资源档案", "视觉资源", "RhineLabUI / model", "已归档 · 可读取", "沿用 RhineLabUI 的 360° 模型旋转、拆解和复位。", ["模型按需加载。", "离开查看器后暂停渲染。", "WebGL 失败时保留静态详情。"]],
  ["档案导出", "ARCHIVE EXPORTS", "资源档案", "导出", "RInBot / export", "迁移准备", "消息、任务和诊断档案的可审计导出。", ["导出内容经过 API 掩码。", "不把 token 或密码复制到导出物。", "导出动作记录审计事件。"]],
  ["字体与视觉资源", "VISUAL RESOURCES", "资源档案", "前端资源", "RhineLabUI / fixed snapshot", "已归档 · 可读取", "固定 RhineLabUI 快照中的字体、图标、模型和音频。", ["资源版本记录在 manifest。", "不跟踪 main 分支。", "大型资源按需加载。"]],
  ["备份快照", "BACKUP SNAPSHOTS", "资源档案", "备份", "RInBot / ops", "迁移准备", "数据库、配置和媒体索引的备份入口。", ["备份先验证目标路径。", "不覆盖现有生产快照。", "恢复前执行完整性检查。"]],
  ["资源治理", "ASSET RETENTION", "资源档案", "资源治理", "RInBot / assets", "迁移准备", "资源失效、清理和保留策略的治理入口。", ["历史引用保持可追溯。", "清理前先完成引用和备份检查。", "未知外部写入不自动重试。"]],

  ["系统终端", "SYSTEM TERMINAL", "系统档案", "运行时", "RInBot / runtime", "已归档 · 可读取", "Rust 核心、队列、迁移和运行时间总览。", ["默认监听 127.0.0.1:3211。", "默认 observe-only。", "不与 Node 生产目录共享数据。"]],
  ["NapCat / OneBot", "NAPCAT CONNECTION", "系统档案", "连接", "RInBot / napcat", "观察模式", "WebSocket、鉴权、心跳和连接状态。", ["连接必须显式开启。", "真实 OneBot 写入仍被 observe-only 保护。", "断开后指数退避重连。"]],
  ["SQLite 存储", "SQLITE STORAGE", "系统档案", "数据库", "RInBot / sqlite", "已归档 · 可读取", "WAL、migration、inbox、messages 和 outbox。", ["synchronous=FULL。", "迁移版本可在状态页查看。", "旧 Node 数据库不被原型直接打开。"]],
  ["Rust API", "AXUM API", "系统档案", "API", "RInBot / axum", "已归档 · 可读取", "health、status、snapshot、messages、events 和对账接口。", ["API 默认只监听本机。", "远程绑定需要 API token。", "前端不直接访问 NapCat。"]],
  ["配置档案", "CONFIGURATION", "系统档案", "配置", "RInBot / config", "迁移准备", "端口、数据目录、OneBot 和功能设置的掩码视图。", ["密钥只显示是否存在。", "安装目录和数据目录保持一致。", "配置写入需要审计与回滚。"]],
  ["诊断日志", "DIAGNOSTICS", "系统档案", "诊断", "RInBot / diagnostics", "已归档 · 可读取", "连接异常、unknown outbox、迁移和版本状态。", ["异常按来源和严重程度分类。", "未知结果保留，不自动重试。", "后端重启不清空前端状态。"]],
  ["更新维护", "MAINTENANCE", "系统档案", "运维", "RInBot / ops", "迁移准备", "版本、升级、备份和回滚的运维入口。", ["不使用 PM2。", "更新先离线预检。", "切换失败回到 Node 快照。"]],
  ["切换与恢复", "CUTOVER & RECOVERY", "系统档案", "发布", "RInBot / migration", "迁移准备", "Node 到 RInBot 的一次性受控切换和回退。", ["完整迁移矩阵通过后才切换。", "切换期间记录 API、NapCat、Worker 时间点。", "失败立即恢复 Node。"]],
];

export const archiveColumns = ["会话档案", "能力档案", "自动化档案", "资源档案", "系统档案"];
export const categories = ["全部档案", ...archiveColumns];
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
  "RInBot / identity": "RInBot 身份记录",
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
export const records: ArchiveRecord[] = content.records.map((record, index) => {
  const module = moduleDescriptors[index];
  return {
    ...record,
    id: `X-${String(index + 1).padStart(3, "0")}`,
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
