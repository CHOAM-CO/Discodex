<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — 文字与语音对话" width="960" /></p>
<p align="center"><strong>在 Discord 中交谈，并延续同一上下文。</strong><br />将文字和语音连接到官方 Codex app-server 持续对话的 Rust 桥接程序。</p>
<p align="center"><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · 简体中文 · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#快速开始">快速开始</a> · <a href="docs/installation.md">安装指南</a> · <a href="docs/design.md">设计</a></p>

Discodex 将一个 Discord 应用配置连接到一个官方 Codex app-server 持续对话。文字和语音共用该对话；任务判断和工具执行由 Codex 负责。

## 功能

| 功能 | 说明 |
| --- | --- |
| 持续上下文 | worker 重启或语音通话结束后，仍可继续同一个 Codex 对话。 |
| 语音与转录 | 在允许的语音频道中交谈；已确认的转录保存在每次通话独立的 Discord 线程中。 |
| 提问与批准 | 在 Discord 表单和按钮中接收 Codex 的问题与批准请求，并将选择提交给对应请求。 |
| 智能体设置 | 设置配置名称和记忆行为；在 `/discodex settings` 面板中选择受支持的模型、推理强度和权限。 |
| Discord 上下文与附件 | 查看消息、频道和用户，将图片及文件传入对话；仅在明确请求时单独发送消息。 |

## 快速开始

需要 Rust 1.94 或更高版本、Cargo 和操作系统对应的构建工具。

```sh
git clone https://github.com/CHOAM-CO/Discodex.git
cd Discodex
./install
```

Windows PowerShell 请运行 `./scripts/install.ps1`。安装程序可选择 English（默认）、한국어、简体中文或日本語。所选语言用于安装提示、Discodex 的 Discord 界面和智能体基础指令。预先设置 `DISCODEX_LANGUAGE=en`、`ko`、`zh` 或 `ja` 可跳过语言选择提示。

安装程序会连接 Codex 与 Discord，配置用户配置文件和登录后启动服务。迁移、更新和卸载请参阅[安装指南](docs/installation.md)。

## 更新

在跟踪文件没有本地修改的源码 checkout 中，使用现有配置文件更新指定配置：

```sh
./update --config "<现有-config.json-路径>"
```

Windows PowerShell：

```powershell
./scripts/update.ps1 -Config "<现有-config.json-路径>"
```

只有跟踪文件干净时，更新程序才会 fast-forward checkout；随后构建 release 二进制并更新 `--config` 指定的配置。它不会再次询问语言，也不会重写配置。所选语言、凭据和持久 Codex 对话状态仍属于该配置。若替换本产品拥有的二进制失败，程序会恢复旧二进制和服务状态。`./install --update --config ...` 与 `./scripts/install.ps1 -Update -Config ...` 也会调用相同更新流程。

## Discord 命令

设置面板显示当前选项，可通过按钮选择模型、推理强度、Fast 模式、权限配置、输入传递方式和语音。语音更改从下一次通话开始生效。

- `/discodex settings`：使用按钮选择设置
- `/discodex voice`：选择语音配置
- `/discodex status`：查看当前对话和设置
- `/discodex voice-join channel:<允许的语音频道>`：加入语音频道
- `/discodex voice-leave`：结束语音通话
- `/discodex interrupt`：请求停止当前响应
- `/discodex new`：开始新对话

退出语音通话只会断开语音；持续对话仍可继续。文档：[安装与卸载](docs/installation.md) · [产品设计](docs/design.md)
