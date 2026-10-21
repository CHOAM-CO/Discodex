<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — 文字与语音对话" width="960" /></p>
<p align="center"><strong>在 Discord 中交谈，并延续同一上下文。</strong><br />随附受管理的 Codex 分支，将 Discord 连接到持久对话的 Rust 桥接程序。</p>
<p align="center"><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · 简体中文 · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#快速开始">快速开始</a> · <a href="docs/installation.zh-CN.md">安装指南</a> · <a href="docs/design.zh-CN.md">产品设计</a></p>

Discodex 将一个 Discord 应用配置连接到一个持久对话。文字和语音共享 Codex 上下文，也可选择 Antigravity CLI 进行文字对话。原生任务和子智能体生命周期由随附的 Codex 分支管理。

本页描述 1.0 源码。下载的二进制文件以已发布的版本标签为准。

## 功能

| 功能 | 说明 |
| --- | --- |
| 持续上下文 | worker 重启或语音通话结束后，仍可继续同一个 Codex 对话。 |
| 语音与转录 | 在配置允许的语音频道中交谈；确认后的转录保存在每次通话独立的 Discord 线程中。 |
| 问题与批准 | 在 Discord 表单和按钮中接收 Codex 的问题与批准请求，并将选择提交给对应请求。 |
| 智能体设置 | 在 `/discodex settings` 中选择受支持的模型、推理强度、服务等级、权限配置、输入传递模式与语音；配置还可控制记忆行为。 |
| 内置 Discodex 技能 | 提供所选安装语言的指南，以及对应已安装构建的安装、设置与故障排查参考文档；更新时刷新受管理的文件。 |
| Discord 上下文与附件 | 查看消息、频道和用户，将图片及文件传入对话；只有用户明确要求时才单独向其他频道发送消息。 |
| Discord 消息策略 | 通过 `Everyone`、`RegisteredServer`、`RegisteredChannels` 或 `Paired` 决定普通消息的接收资格，并独立维护允许列表和优先级更高的拒绝列表；获准消息仍按主页、机器人提及、注册频道/线程及获准私信的路由处理。 |
| 语音收件箱 | 通话期间暂存主页输入和其他频道的机器人提及；提供预览，仅在对话需要或用户要求时读取，未读项目在通话结束后继续处理。 |
| 回复与提及 | 主页回复关联原消息；普通回复可明确提及输入者，状态和错误通知不触发提及。 |
| Markdown 表格 | 已完成的普通回复和 `discord_send_message` 输出会将表格显示为自动换行、分页的 PNG 附件，保留行内粗体、斜体和下划线格式，同时保留周围文字。点击本地化的 **查看原文** 按钮后，点击者可查看并复制分页的 Markdown 原文。 |
| 任务与工作树 | 每次计划变化都会在主页频道创建新的 Embed 卡片，绝不编辑旧卡片；显示完成/总数、完成勾选与删除线、当前步骤箭头、待处理标记和状态颜色，并抑制相同事件的重复回放；完整计划文本按 Discord 实际限制分页，不会截断。任务与工作树工具沿用同一 app-server 连接。 |
| Native 功能 | 每个配置文件通过 `native-features.json` 管理官方 app-server 功能；设置界面区分实际生效状态与保存值，并标明需要重启的更改。 |
| 插件 | 在 `/discodex plugins` 中浏览当前 native 插件目录，并通过 Discord 按钮执行可用的读取、安装和卸载；推荐与授权沿用官方 native 流程。 |
| 异步问题与通知 | 可通过 Discord 按钮或自由文本回答 native 问题，响应关联原问题 ID 和 thread。异步通知发送到主页频道，不会结束活动 turn。 |
| 事件处理与日志 | 主控制事件由受监督的消费者处理，并保存配置文件级日志，便于排查无控制台启动的 Windows GUI worker。更新使用预编译版本。 |

## 快速开始

从[最新发行版](https://github.com/CHOAM-CO/Discodex/releases/latest)下载 `discodex-installers.zip`，解压并在该目录运行。安装脚本下载对应平台的 worker 和匹配的 Codex 运行时，无需 Rust、Cargo 或 Git。可以选择现有 Codex 数据目录共享账户、记忆和历史（检测到时推荐），或选择 Discodex 独立目录；共享设置不会被覆盖。

Linux/macOS：

```sh
./install
```

Windows PowerShell：

```powershell
./scripts/install.ps1
```

安装时可选择 English（默认）、한국어、简体中文或日本語。所选语言用于安装提示、Discodex Discord 界面和智能体基础指令。预先设置 `DISCODEX_LANGUAGE=en`、`ko`、`zh` 或 `ja` 可跳过语言选择。

安装器会连接 Codex 与 Discord，配置用户配置文件和登录后启动服务。迁移、更新和卸载步骤见[安装指南](docs/installation.zh-CN.md)。

## 自定义与本地模型

主模型和**默认子智能体模型**列出原生目录与明确配置的提供商/模型。随附分支允许子智能体使用不同于父会话的提供商。保存的子智能体默认值在下次创建或恢复线程时读取，包括 worker 重启后的恢复。当前线程在恢复前保持其有效设置；现有子会话的设置和父会话均保留。提供商认证与模型能力仍由 Codex 处理。示例见[提供商指南](docs/installation.zh-CN.md#自定义与本地-codex-provider)。

历史兼容性由独立的 `encrypted_content` 与 `agent_messages` 控制。发送时保留可读上下文，保存的原始历史不变；详见[历史兼容性](docs/design.zh-CN.md#提供商切换与加密历史)。

Antigravity 设置在下次模型选择或 AGY 会话启动时生效；原生进程功能开关需要重启 worker。在设置中启用 **Antigravity** 后，从已安装 CLI 的 `agy models` 读取准确的模型 ID 和名称，显示在两处选择器中；手动填写的 ID 用于补充列表。推理强度来自 CLI 的 `--help`，不按模型名称猜测或映射。列表读取失败会在设置中显示，明确指定的不受支持强度也会原样传给 CLI，由其返回错误。Discodex 使用单独安装的 AGY CLI 及其现有订阅登录，不使用 SDK。重叠的内置工具由 AGY 执行，额外的 Discord 和任务工具通过 MCP 连接；子智能体管理由 Codex 分支负责。每次响应启动新 CLI 并传入原生对话和可见工具结果，不恢复 AGY 私有状态。不支持 AGY 语音。

## 更新

`/discodex update` 会立即检查最新版本。**更新 / 取消**确认页面说明更新将中断当前工作（包括子智能体）并结束语音。确认后直接应用，无需空闲队列；重启后在主页频道报告完成或失败，不会自动重新启动已中断的响应，也不会插入恢复提示或空输入。

自动检查成功后间隔 6 小时，失败后 15 分钟重试；空闲 5 分钟后显示通知，提供**更新**、**跳过此版本**、**不再通知**。这些时间条件只控制通知，不会延迟已确认的更新。

手动更新使用安装包中的脚本：

```sh
./update --config "<path-to-existing-config.json>"
```

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

更新程序下载 worker 及其匹配的预编译 Codex 包。清单固定 worker 版本、分支提交、平台和校验和。先准备运行时，再替换所选配置拥有的文件。替换或重启失败时，更新程序会尝试恢复原 worker、配置、运行时引用、所有权记录和服务状态。必要的恢复步骤失败时，停止依赖该步骤的后续操作并报告原因，不会重启新旧文件混用的安装。未使用的暂存运行时会在必要的服务恢复后清理，清理失败也会报告。Codex 数据目录、账户、记忆、历史和设置保持不变。0.1.x 自动更新器保留原可执行文件资产名和 helper 入口；0.1.3 以前的版本需要先运行手动脚本。客户端不编译源码。

## Discord 命令

设置面板提供主模型、默认子智能体模型、推理强度、Fast 模式、权限配置、批准策略、输入方式和语音。批准策略默认 **on-request**，与权限配置独立。**高级设置**集中详细原生功能，不重复子智能体模型选择。语音变更从下一次通话开始生效。 审批选择按配置文件保存在各自的 `native-features.json` 中，不覆盖共享 Codex 配置。

- `/discodex settings`：使用按钮查看和选择设置
- `/discodex voice`：选择语音配置
- `/discodex status`：查看当前对话和设置
- `/discodex voice-join channel:<允许的语音频道>`：加入允许的语音频道
- `/discodex voice-leave`：结束语音通话
- `/discodex interrupt`：请求停止当前响应
- `/discodex compact`：手动压缩当前对话上下文
- `/discodex update`：检查并应用更新
- `/discodex new`：开始新对话

结束语音通话只会断开语音；持久 Codex 对话仍可继续。详细说明见[产品设计](docs/design.zh-CN.md)。

<p align="center"><a href="README.md">English README</a> · <a href="README.ko.md">한국어 README</a> · 简体中文 README · <a href="README.ja.md">日本語 README</a></p>
