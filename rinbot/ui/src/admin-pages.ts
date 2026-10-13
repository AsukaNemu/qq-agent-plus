export type AdminTab = { id: string; label: string; view: string };
export type AdminPage = { title: string; tabs: AdminTab[] };
const tab = (id: string, label: string, view = id): AdminTab => ({ id, label, view });

// Third-level navigation belongs to the current workspace, not to the archive template.
export const adminPages: Record<string, AdminPage> = {
  "系统终端": { title: "运行总览", tabs: [tab("status", "运行状态"), tab("recent", "最近事件", "events")] },
  "事件流": { title: "实时事件", tabs: [tab("events", "事件记录")] },
  "消息档案": { title: "消息档案", tabs: [tab("messages", "全部消息"), tab("private", "私聊", "messages"), tab("group", "群聊", "messages")] },
  "记忆整理": { title: "记忆档案", tabs: [tab("memory", "人物记忆"), tab("handoff", "会话交接"), tab("relationships", "关系档案"), tab("relationship-settings", "评估设置")] },
  "身份与申请": { title: "身份与申请", tabs: [tab("people", "人物身份"), tab("requests", "好友申请"), tab("invites", "群邀请"), tab("invite-settings", "接入设置")] },
  "策略引擎": { title: "功能模块", tabs: [tab("features", "实验功能"), tab("behavior", "回复策略"), tab("schedule", "回复时间表")] },
  "功能模块": { title: "实验功能", tabs: [tab("features", "启停与毕业")] },
  "模型路由": { title: "模型设置", tabs: [tab("models", "对话与图片识别")] },
  "工具执行": { title: "工具与多模态", tabs: [tab("web-search", "联网搜索"), tab("asr", "语音识别")] },
  "语音识别": { title: "语音识别", tabs: [tab("asr", "识别设置")] },
  "任务档案": { title: "任务档案", tabs: [tab("tasks", "消息队列"), tab("unknown", "外发对账")] },
  "主动消息": { title: "主动消息", tabs: [tab("wakes", "唤醒记录"), tab("proactive-settings", "运行设置")] },
  "空间互动": { title: "空间互动", tabs: [tab("qzone", "互动记录"), tab("qzone-settings", "互动设置"), tab("daily", "每日内容")] },
  "贴纸管理": { title: "贴纸与媒体", tabs: [tab("stickers", "贴纸管理")] },
  "异常台账": { title: "诊断", tabs: [tab("incidents", "异常记录"), tab("chat-controls", "会话控制"), tab("incident-settings", "告警设置"), tab("connection", "连接与存储")] },
  "配置档案": { title: "全局设置", tabs: [tab("queue-settings", "队列参数"), tab("connection", "服务配置")] },
  "切换与恢复": { title: "更新与备份", tabs: [tab("maintenance", "版本与数据")] },
  "会话交接": { title: "会话交接", tabs: [tab("handoff", "交接记录")] },
  "关系档案": { title: "关系档案", tabs: [tab("relationships", "关系记录"), tab("relationship-settings", "评估设置")] },
  "群邀请管理": { title: "群邀请", tabs: [tab("invites", "邀请记录"), tab("invite-settings", "接入设置")] },
  "未知外发对账": { title: "外发对账", tabs: [tab("unknown", "待核对记录")] },
  "每日内容": { title: "每日内容", tabs: [tab("daily", "草稿与发布")] },
};

export function pageFor(title: string): AdminPage {
  const page = adminPages[title];
  if (!page) throw new Error(`No operational page for ${title}`);
  return page;
}

export function resolveTab(title: string, id: string | null | undefined): AdminTab {
  const page = pageFor(title);
  return page.tabs.find((entry) => entry.id === id) ?? page.tabs[0];
}

export const stateLabels: Record<string, string> = {
  pending: "待处理", queued: "排队中", processing: "处理中", running: "运行中",
  held: "已挂起", held_unknown: "结果未知", unknown: "待人工核对", sending: "发送中",
  done: "已完成", sent: "已送达", failed: "失败", cancelled: "已取消", skipped: "已跳过",
  observed: "待审核", approved: "已同意", rejected: "已拒绝", none: "未执行",
  applied: "已同步", idle: "空闲", blocked: "已暂停", reconciled_sent: "已核对送达",
  reconciled_failed: "已核对未送达", feed: "动态", reply: "评论", like: "点赞", comment: "评论",
};
export const stateLabel = (value: string) => stateLabels[value] ?? value;
