# GitHub 自动更新部署

## 运行边界

自动更新由主进程之外的两个 systemd 用户单元执行：

```text
qq-agent-linux-update.timer
  -> qq-agent-linux-update.service
  -> scripts/auto-update.mjs
  -> deploy.sh
```

timer 每小时唤醒一次，应用配置中的 `intervalHours` 决定是否已到达实际检查时间。
默认每 6 小时检查一次，功能默认关闭。更新器不复用聊天会话，也不调用模型。

## 更新依据：已发布的 Release

更新提示与部署目标均以 GitHub 上**已发布的 Release** 为准。branch 上新增提交不触发更新，
以避免部署未经发布的中间状态：

1. 读取仓库最新的 Release，草稿与预发布均不计入。仓库尚无 Release 时不提示、不更新。
2. 以「当前部署 revision 与 Release tag」的比较结果判断方向：

   | 比较结果 | 含义 | 行为 |
   | --- | --- | --- |
   | `ahead` | Release 领先当前部署 | 提示新版本，部署目标为该 tag 的提交 |
   | `diverged` | 两侧均有提交 | 按 Release 部署，以发布版本为准 |
   | `behind` | 当前部署已经包含该 Release | 提示“已是最新”，不回退 |
   | `identical` | 为同一个提交 | 提示“已是最新” |

3. 比较接口不可用时不提示、不部署，等待下次检查，不推测方向，以避免部署错误版本。
4. 当前部署 revision 不是 git 提交（如压缩包安装）时无法比较方向：控制台仅作说明、不弹窗，
   但「立即更新」与自动更新会直接安装最新 Release，安装完成后基线即为 git 提交。

发布流程：推送 `v*` tag 触发 `.github/workflows/release.yml`，该工作流先执行与 CI 相同的检查，
再创建**草稿** Release。确认无误并发布后，控制台的“发现新版本”弹窗与自动更新才会识别该版本。

## 配置

```json
{
  "autoUpdate": {
    "enabled": false,
    "ownerUin": "",
    "repository": "https://github.com/AsukaNemu/qq-agent-plus.git",
    "branch": "main",
    "intervalHours": 6,
    "networkRetries": 4,
    "retryBaseMs": 1500,
    "retryMaxMs": 15000,
    "connectivityTimeoutSeconds": 20,
    "fetchTimeoutSeconds": 300,
    "forceHttp11": true,
    "disableOnFailure": true
  }
}
```

- 仓库只接受 GitHub HTTPS 地址。
- 分支名经过格式校验，默认 `main`；控制页可直接切换目标分支。
- `networkRetries` 为网络操作失败后的额外重试次数，范围 0-10。
- 重试采用指数退避：从 `retryBaseMs` 开始，最多增长到 `retryMaxMs`。
- `connectivityTimeoutSeconds` 控制轻量连通性预检超时；`fetchTimeoutSeconds` 控制实际 Git 拉取超时。
- `forceHttp11=true` 时 Git 使用 HTTP/1.1，并设置低速保护，可规避部分 HTTP/2 / GnuTLS 链路抖动。
- `disableOnFailure=true` 保持既有行为：更新失败后暂停后续自动更新；关闭时失败仅记录并告警，后续周期继续尝试。
- 管理员 QQ 必须位于私聊白名单；单独的“测试 GitHub 连通性”不要求配置管理员。
- 控制台“控制 -> 更新部署”可保存网络策略、测试连通性、立即手动更新、暂停或恢复自动更新。

## 更新流程

1. 使用 `data/update-repository.git` 作为持久 bare 仓库，保留 Git 对象缓存。
2. 先执行目标仓库与目标分支的 `git ls-remote` 轻量连通性测试；失败时按配置重试。
3. 连通性正常后读取最新已发布 Release，并按上一节的标准判断方向；没有可部署的版本时记录“已是最新”并结束。
4. 浅拉取该 Release 的 tag（`refs/tags/<tag>:refs/tags/<tag>`），解析出它指向的提交；
   与 `data/deployed-revision` 相同则记录“已是最新”并结束。`git fetch` 同样按配置重试。
5. 将目标提交检出到 `data/update-work/` 的临时目录。
6. 在独立临时数据目录中执行 `npm ci`、全部 `node:test` 单元测试及关键语法检查，不读取或修改生产数据。
   `npm ci` 使用 `--prefer-offline` 优先复用 npm cache，并把同一组重试参数传给 npm 的 fetch 层。
7. 调用目标提交中的 `deploy.sh`。部署脚本创建代码快照、保留数据和凭据、重装依赖、
   校验 systemd 单元、启动服务并检查 `/healthz`。
8. 成功后记录目标提交（`currentRevision`）与 Release tag（`targetVersion`）；失败时由 `deploy.sh` 恢复旧代码和服务。

`deploy.sh` 会同时安装和校验更新 service/timer，并在部署失败时恢复旧单元及原启用状态。

## 两条下载通道：git 与 API + 源码包

部分网络环境下，到 `github.com` 的 **git 通道**不可用：TCP 可以建立连接，
但 `ls-remote` 会持续阻塞直至超时，而 `api.github.com` 与 `codeload.github.com` 可以正常访问。
该情况下无需手动升级：

1. `git ls-remote` 连通性预检失败后，再通过 GitHub API 探测同一仓库与分支
   （`GET /repos/<owner>/<repo>/commits/<branch>`）。该请求同样失败才判定为不可达，错误与告警逻辑不变。
2. 确定待部署的 Release 后，通过 API 将 tag 解析为 commit（`/commits/<tag>` → `sha`），
   再从 `codeload.github.com/<owner>/<repo>/tar.gz/<sha>` 下载该提交的源码包，
   使用系统 `tar` 解压到工作目录，并去掉压缩包顶层目录。
3. 两条通道均为 HTTPS，均以 GitHub 给出的 commit sha 为锚点，后续步骤
   （`npm ci` → 单元测试 → `deploy.sh`）完全一致；`deploy.sh` 通过
   `QQ_AGENT_SOURCE_REVISION` 记录版本，不依赖工作目录中存在 `.git`。
4. 解析 tag 与获取源码均**先尝试首选通道，失败后切换另一条**：git 可用时使用 git，
   git 不可用时改用 API，API 中途失败时回退 git（回退到 git 获取源码时先 `fetch` 该 tag
   再 `checkout`，因为 API 解析出的提交对象不在本地缓存中）。两条通道均不可用时，
   状态中的错误会同时包含两条通道的原因，不会只保留最后一条。
5. 重试策略对两条通道均生效，次数与退避沿用前述 `autoUpdate` 配置：
   API 探测、tag 解析、源码包下载各自进行退避重试；`4xx`（429 除外）判定为不可重试，不进行无效等待。
6. 源码包在下载过程中累计计算大小，上限为 64 MiB，超限立即断开；默认基地址必须为 HTTPS，
   以防止默认地址被代理或镜像降级为明文。
7. `QQ_AGENT_CODELOAD` 可覆盖 codeload 基地址，供测试桩或自建镜像使用，普通部署无需设置。
   该变量与 `QQ_AGENT_GITHUB_API` 的信任级别不同：后者仅修改只读查询地址，
   前者修改的是**会由 `deploy.sh` 执行的源码来源**。该变量不得接入配置或控制台。
8. 本次实际使用的通道记录在 `data/auto-update.json` 的 `transport` 与
   `connectivity.transport`（`git` / `api`）中，控制页的连通性一行同样显示通道名。

## 连通性测试

控制页中的“测试 GitHub 连通性”会提交一个独立的 `probe` 请求：

- 先通过 Git transport（`git ls-remote`）检查目标仓库与分支；git 通道不可用时自动改用
  GitHub API 探测同一分支，两条通道均失败才判定为不可达。
- 适用 HTTP/1.1、超时、重试与指数退避设置。
- 不执行 `git fetch`、`npm ci`、测试或部署。
- 测试失败不会修改自动更新开关，也不会发送部署失败告警。
- 结果保存在 `data/auto-update.json` 的 `connectivity` 字段中，控制页可查看尝试次数、耗时、
  实际通道（`transport`）、目标 revision 和错误。

## 失败策略

检查、测试或部署失败都会在 `data/auto-update.json` 保存失败阶段、脱敏错误和目标提交，并在服务可用且协议端（OneBot）已连接后向管理员发送一次通知。

`disableOnFailure=true` 时：

1. 将 `autoUpdate.enabled` 持久化为 `false`。
2. `autoDisabled=true`，timer 后续唤醒时不再继续部署。
3. 管理员核对后可从控制页恢复自动更新。

`disableOnFailure=false` 时：

1. 自动更新开关保持原状。
2. `autoDisabled=false`，下一个检查周期仍会继续尝试。
3. 告警会明确说明自动更新保持启用，不会误报为“已停止”。

通知发送失败分两种（`src/auto-update.js` 按 `beforeWrite` 判定）：

- **确定没发出去**（协议端未连接等）：保留 pending，等 Agent 启动或协议端重连后继续发送；
- **结果未知**（发送超时、业务拒绝等）：不自动重发，只记 `deliveryUnknown: true` 与错误原因
  （项目规则：结果未知不外发重试），需要人工在控制台确认。
- **发送途中崩溃/重启**：发送前通知会先以"结果未知"状态落盘（`pending=false`），重启后
  不会重发同一条；此时 `notification.error` 为空，部署失败原因看状态里的 `error` 字段。

两种都不会因告警失败再次触发部署，也不会递归创建异常。

“没有新 Release”“当前部署已包含最新 Release”“比较接口不可用”均不计为失败：不发送告警、
不触发 `disableOnFailure`，仅在状态文件与控制页“更新部署”面板中说明原因。

## 状态与运维

状态文件：

```text
data/auto-update.json          # 状态、连通性、updateNotice（弹窗判定结果）、ignoredVersion
data/auto-update-request.json  # 一次性请求（manual / scheduled / probe）
data/deployed-revision         # 当前部署的 git 提交，deploy.sh 写入
```

`updateNotice` 中 `reason` 的取值：`no-release`（尚无 Release）、`ahead-of-release`（当前部署已包含最新 Release）、
`unknown-deployed`（部署基线不是 git 提交）、`compare-failed`（无法比较方向）、`unreachable`（无法连接 GitHub）、
`unconfigured`（未配置仓库）。仅当 `available=true` 时弹窗，`version` 为待部署的 Release tag。

判定使用的 GitHub API 默认为 `api.github.com`；仅测试桩或自建镜像需要设置 `QQ_AGENT_GITHUB_API` 覆盖该地址。

接口：

```text
GET  /api/auto-update/status
PUT  /api/auto-update/settings
POST /api/auto-update/run
POST /api/auto-update/pause
POST /api/auto-update/resume
POST /api/auto-update/notify-pending
GET  /api/auto-update/check
POST /api/auto-update/ignore
```

高级网络配置由控制页通过通用 `/api/config` 持久化；连通性测试使用一次性的 `probe` 请求，不引入额外常驻服务。

命令：

```bash
manage.sh update-status
manage.sh update-now --confirm
manage.sh update-pause --confirm
manage.sh update-resume --confirm
```
