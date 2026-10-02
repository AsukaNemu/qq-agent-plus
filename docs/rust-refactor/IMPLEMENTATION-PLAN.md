# RInBot Rust 重构：可验证实施计划

本计划只扩展 RPD v0.3，不提前改变生产运行链路。每阶段完成后必须提交变更清单、
命令/样例、通过或阻塞结果，再进入下一阶段。

## 用户确认的切换策略

- Node 旧项目保持不动，继续作为当前运行主体。
- RInBot 在独立目录、独立数据目录和独立端口中完整建设与验收。
- 在功能迁移、数据对账、性能、回滚和恢复测试全部通过前，不进行生产切换。
- 达到切换门槛后，保留 Node 原始版本与数据快照，执行一次受控停机、切换和真实健康检查。
- 切换失败立即回到 Node；不采用“边改 Node、边切换”的长期混合状态。

## 当前已开始：阶段 0 与基础工程

### 阶段 0 验收

- [x] 读取 RPD、AGENTS、实验功能规范和现有架构。
- [x] 记录工作区 dirty 状态，保留全部用户改动。
- [x] 记录当前配置开关、端口、运行进程和非敏感数据文件形态。
- [x] 确认 `data/` 不被 Rust 原型直接打开。
- [x] 运行一个不需要停止生产服务的 Node 布局基线测试。
- [x] 运行现有 Node 单元基线：669 通过、21 失败、9 跳过；失败集中于既有部署脚本
  `IMAGE_MIRRORS[@]` 未定义问题。
- [x] 安装 stable Apple Silicon Rust toolchain、rustfmt，并运行完整 Cargo 编译/测试。

### 基础工程验收

- [x] 建立独立 `rinbot/` Cargo crate 与 stable toolchain 声明。
- [x] 建立 `domain -> application/ports -> adapters/api` 依赖骨架。
- [x] 建立 SQLite WAL migration：inbox、messages、outbox、runtime events。
- [x] OneBot 原始事件只在 NapCat adapter 中转换为内部 `IncomingMessage`。
- [x] NapCat adapter 已加入 action 统一入口、`get_login_info`、请求超时、响应上限、Ping
  和有界指数重连；仍待 Rust toolchain 下执行真实编译与隔离 WS smoke test。
- [x] 观察模式与外发写入保护分离；unknown 外发结果不自动重试。
- [x] 建立有界 256 条 inbox ID 队列，消息正文进入 SQLite 后才排队。
- [x] 建立 `/api/health`、`/api/status`、`/api/snapshot`、分页 messages 和 SSE。
- [x] 建立 unknown outbox 查询与人工 `sent/failed` reconcile API；unknown 不会重新进入发送队列。
- [x] 建立 inbox 租约 claim、过期恢复和 owner 校验。
- [x] 默认监听 `127.0.0.1:3211`，不占用现有控制台 `3210`。
- [x] 对 migration、manifest 和 whitespace 做无 Rust toolchain 的静态检查。
- [x] 添加私聊、群聊媒体、notice 的脱敏 OneBot fixtures，以及 SQLite 去重/分页测试。
- [x] `cargo fmt -- --check`、10 个 Rust 测试和临时端口 API smoke test 通过；未连接生产 NapCat。

## 后续阶段

### 阶段 1：NapCat adapter（只读/回放）

1. 在隔离端口或事件回放样本上验证 WS 重连、鉴权、心跳、`get_login_info`。
2. 增加真实 OneBot API 错误归一化和 confirmed/unknown 分类。
3. 生成私聊、群聊、图片、语音、贴纸、撤回、notice 的脱敏 fixture。
4. 验收：Rust 端观察连接可用，但不存在任何生产发送路径。

### 阶段 2：存储与迁移工具

1. 对旧 `messages.sqlite`、pilot 数据库、memory/sessions、媒体目录做一致性快照。
2. 实现 schema 映射、计数、抽样和失败清单；不让旧 Node 与 Rust 同时写同一库。
3. 为租约恢复、重复事件、unknown outbox 和会话接续补测试。
4. 验收：导入前后可复核，旧目录仍可回滚读取。

### 阶段 3：最小业务链路

1. 把 allow/deny、时间门控、关键词/@/概率/限频映射到 Rust policy。
2. 接入现有模型 provider 的最小抽象；密钥只从受保护配置/Keychain 读取。
3. 完成文本消息的 inbox → bounded ID queue → LLM → outbox → adapter 对照。
4. 先使用模拟 gateway 和回放，真实 QQ 只在小范围白名单切换验收。

### 阶段 4：Rust API 与控制台原型

1. 补齐 config/status/tasks/outbox reconcile API 与本地会话认证。
2. 仅通过 Rust API 提供静态前端；旧 UI 不作为 Rust 原型依赖。
3. 按 RPD 的一级/二级导航和 RhineLabUI 固定资源清单实现四个原型页面。
4. 验收 SSE 断线重连、snapshot 版本缺口、后端重启不刷新页面。

当前进度：

- [x] `rinbot/ui` 改为直接使用固定 RhineLabUI 快照的页面、开场、档案阵列、详情和模型查看器。
- [x] RhineLabUI 的 `src/`、`public/`、`content/` 和三维模型/音频资源已按固定 commit 编译进 RInBot。
- [x] 增加轻量 RInBot 状态桥接，页面保留 Rhine 原生视觉，同时显示 Rust 核心与 OneBot 状态。
- [x] RhineLabUI commit、完整资源来源与哈希记录在 `rinbot/ui/reference/rhinelabui/source-manifest.json`。
- [x] `tsc --noEmit`、`vite build` 通过；Rust API 已支持 `--ui-dir` 静态目录回退。
- [x] 将当前三维档案阵列固定为 RInBot 首页 1：根路径 `/` 和 PWA 启动入口统一指向该页面。
- [x] 静态入口增加缓存重新验证，避免构建后的哈希资源更新造成旧缓存空白页。
- [x] 将档案选择、搜索、收藏、设置、详情三标签、导出和 360° 查看器保留在 Rhine 原生位置。
- [x] 将 5 个分类、40 个档案槽位映射到 RInBot 会话/能力/自动化/资源/系统模块，并绑定快照、消息和 unknown outbox 只读数据。
- [x] 增加脱敏 `/api/config` 只读接口，并在“配置档案”原生详情位置显示实际端口、目录、OneBot 和令牌存在性。
- [x] 增加 API 集成测试：健康接口、脱敏配置、Bearer 鉴权和 snapshot 事件版本递增均有回归覆盖。
- [x] 将开场动画时间轴提速为原来的 2 倍；动效偏好仍由 Rhine 原生设置控制。
- [ ] 补齐 config/tasks/功能设置等需要写入权限的 RInBot API，并在切换前完成真实数据迁移与对账。

### 阶段 5：按矩阵迁移高级功能

按 RPD 顺序迁移 prompt/model、memory、stickers、scheduler、proactive、QZone、ASR/vision
和 tools。每个实验性功能保持 `enabled` 与 `graduated` 独立，专属页面与状态机独立，
禁用只停止运行副作用，不删除数据，不自动重试 unknown 外部写入。

### 阶段 6：首次生产切换与日常升级

1. RInBot 观察/回放对照达标后，停止 Node 写入并建立最终一致性快照。
2. RInBot 使用独立迁移目录；Node 原目录保留为只读回滚快照。
3. 完成小范围白名单、消息/outbox/任务对账后一次性切换生产写入权。
4. 代码升级采用离线预检、停收、有界退出、原子指针和真实健康检查；失败自动回到 Node。

## 每阶段固定报告格式

```text
变更：新增/修改的文件与行为
验证：执行的命令、样例、通过/失败数量
风险：未验证项、生产边界、回滚点
下一步：下一阶段最小可执行任务
```
