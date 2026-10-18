<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — 文字与语音对话" width="960" /></p>
<p align="center"><strong>在 Discord 中交谈，并延续同一上下文。</strong><br />将文字和语音连接到官方 Codex app-server 持久对话的 Rust 桥接程序。</p>
<p align="center"><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · 简体中文 · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#快速开始">快速开始</a> · <a href="docs/installation.zh-CN.md">安装指南</a> · <a href="docs/design.zh-CN.md">产品设计</a></p>

Discodex 将一个 Discord 应用配置连接到官方 Codex app-server 的一个持久 thread。文字和语音共用该对话；任务判断和工具执行由 Codex 负责。

## 功能

| 功能 | 说明 |
| --- | --- |
| 持续上下文 | worker 重启或语音通话结束后，仍可继续同一个 Codex 对话。 |
| 语音与转录 | 在配置允许的语音频道中交谈；确认后的转录保存在每次通话独立的 Discord 线程中。 |
| 问题与批准 | 在 Discord 表单和按钮中接收 Codex 的问题与批准请求，并将选择提交给对应请求。 |
| 智能体设置 | 在 `/discodex settings` 中选择受支持的模型、推理强度、服务等级、权限配置、输入传递模式与语音；配置还可控制记忆行为。 |
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

从[最新发行版](https://github.com/CHOAM-CO/Discodex/releases/latest)下载 `discodex-installers.zip`，解压并在该目录运行。安装脚本会下载适合当前操作系统和架构的预编译程序，无需安装 Rust、Cargo 或 Git 客户端。

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

## 更新

使用发行包中的更新脚本，并指定现有配置文件：

```sh
./update --config "<path-to-existing-config.json>"
```

Windows PowerShell：

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

更新程序会下载并校验对应平台的正式发行版预编译程序，然后只更新 `--config` 指定的配置。Discord 内更新通知也使用相同的预编译程序。更新会保留配置语言、设置、凭据和持久 Codex 对话状态；若替换产品拥有的二进制或重启服务失败，会尝试恢复旧二进制、所有权记录和服务状态。若要更新低于 v0.1.3 的旧版本，首次请从发行包下载并运行新版更新脚本。`./install --update --config ...` 与 `./scripts/install.ps1 -Update -Config ...` 也会调用相同更新流程。

若尚无上次检查记录，则立即执行首次正式 Release 检查；此后按照持久化的上次检查时间，在到期后再检查。检查循环约每 15 秒轮询一次是否到期；成功检查间隔为 6 小时，检查出错后 15 分钟重试。worker 重启遵循已保存的检查时间，不会强制立即检查。通知另有空闲条件：原生连接必须在线，且没有活动 turn、语音通话或清理、待处理批准、未处理输入、消息处理或输出发送；满足这些条件并已空闲至少 5 分钟后，才在主页频道提示。发现更新时若已空闲，不会再额外等待 5 分钟。

## Discord 命令

设置面板显示当前选项，可通过按钮选择模型、推理强度、Fast 模式、权限配置、输入传递方式和语音。语音变更从下一次通话开始生效。

- `/discodex settings`：使用按钮查看和选择设置
- `/discodex voice`：选择语音配置
- `/discodex status`：查看当前对话和设置
- `/discodex voice-join channel:<允许的语音频道>`：加入允许的语音频道
- `/discodex voice-leave`：结束语音通话
- `/discodex interrupt`：请求停止当前响应
- `/discodex new`：开始新对话

结束语音通话只会断开语音；持久 Codex 对话仍可继续。详细说明见[产品设计](docs/design.zh-CN.md)。

<p align="center"><a href="README.md">English README</a> · <a href="README.ko.md">한국어 README</a> · 简体中文 README · <a href="README.ja.md">日本語 README</a></p>
