# 安装与卸载

<p align="center"><a href="../README.md">English README</a> · <a href="../README.ko.md">한국어 README</a> · <a href="../README.zh-CN.md">简体中文 README</a> · <a href="../README.ja.md">日本語 README</a></p>
<p align="center"><a href="installation.md">English</a> · <a href="installation.ko.md">한국어</a> · 简体中文 · <a href="installation.ja.md">日本語</a></p>
<p align="center"><a href="installation.zh-CN.md">安装与卸载</a> · <a href="design.zh-CN.md">产品设计</a></p>

本指南介绍用户范围的安装和更新。安装脚本会从 GitHub 正式 Release 下载与操作系统和架构对应的预编译程序，并校验 SHA-256；无需本机 Rust、Cargo 或平台构建工具。Linux 预编译版本基于 Ubuntu 22.04（glibc 2.35），这是 Linux 兼容性基线。

Linux/macOS：

```sh
./install
```

Windows PowerShell：

```powershell
./scripts/install.ps1
```

## 选择语言

安装开始时可选择 English（默认）、한국어、简体中文或日本語。所选语言用于安装提示、Discodex Discord 界面文字和智能体基础指令。预先设置 `DISCODEX_LANGUAGE=en`、`ko`、`zh` 或 `ja`，bootstrap 就会跳过语言问题，并将该值传给安装程序。

macOS/Linux：

```sh
DISCODEX_LANGUAGE=zh ./install
```

Windows PowerShell：

```powershell
$env:DISCODEX_LANGUAGE = 'zh'
./scripts/install.ps1
```

## 迁移现有配置

若要从现有 Codex Worker 安装迁移单个配置，请对下载的 Discodex 程序运行迁移命令，并指定配置文件。Windows 请在 PowerShell 中使用实际配置文件路径。

```sh
./discodex migrate --config "<path-to-legacy-config.json>"
```

macOS、Linux 和 Windows 上，迁移会先通过所有权记录和服务确认指定配置，再把该配置、凭据、安装清单和状态复制到新的 `discodex` 路径。原路径会保留，以便回滚和保护数据。确认迁移后的配置可用前，不要删除原目录。

也可在 Linux/macOS 直接运行 `sh scripts/install.sh`。安装 bootstrap 会下载 GitHub Release 中对应平台的预编译程序并校验 SHA-256，使用 `discodex` 名称和用户数据默认路径。若目标文件已存在，不会覆盖。

## 首次安装

若没有保存的安装清单，`discodex install` 会交互式创建清单；`install-plan` 只创建相同清单。安装者需指定配置显示名称、Codex 执行程序（检测到的桌面应用/现有 CLI，或由安装器下载的官方 Codex core）、Discord 服务器、主页文字频道、转录频道、允许的语音频道、操作者 ID、接收策略和注册用户/频道 ID、已存在的项目目录及可选模型。接收策略为 `Everyone`（所有服务器和私信）、`RegisteredServer`（指定服务器）、`RegisteredChannels`（注册频道及子线程）或 `Paired`（注册用户 ID）。允许和拒绝列表独立于策略保存，拒绝列表优先。策略只决定消息接收范围；slash command 和管理权限遵循配置的操作者。首次安装计划中的策略和列表只在安装时写入运行时 `message-access.json`；之后重新安装或更新会保留当前值。

Discord token 通过安全输入提供。安装器会检查 Discord 访问和 native 连接。若 Codex 需要登录或 MFA，请在官方登录流程中完成。worker `config.json` 中的 `effort` 和 `serviceTier` 用于新建持久 thread；`serviceTier` 设为 `off` 时会向 native 传递清除 tier 的请求。未设置则遵循 native 默认值；恢复既有 thread 时，不会覆盖通过 `/discodex` 修改的当前 thread 设置。

仅保存安装选择清单并不代表服务已准备完成。安装器会检查 native 账号、catalog 和 Discord 配置，并注册用户服务。服务成功启动本身不能证明文字、问题/批准或语音功能均已验证。

## 更新

更新现有配置时，在安装脚本中明确指定该配置文件。即使设备上有多个配置，也不会猜测要更新哪一个。更新沿用保存的配置语言，不会再次询问语言。

macOS/Linux：

```sh
./update --config "<path-to-existing-config.json>"
```

Windows PowerShell：

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

更新脚本会从 GitHub 正式 Release 下载当前平台的预编译程序和 SHA-256 校验值。不修改源码 checkout，也不从源码构建。校验通过的程序会交给 `discodex update --source <new-binary> --config <existing-config>`，只更新指定且归产品所有的配置。`./install --update --config ...` 和 Windows 的 `./scripts/install.ps1 -Update -Config ...` 是同一更新流程的别名。

更新保留现有配置的语言、设置、凭据和持久 Codex 对话状态，不会创建新配置。如果替换程序或重启服务失败，会恢复此前归产品所有的二进制、所有权记录和服务状态。为回滚或数据保留而留下的旧目录不会自动删除。需要更改现有配置或对话状态时，应使用单独且明确的迁移流程；更新本身不会执行这些更改。

## 服务与卸载

用户服务安装为 Linux `systemd --user`、macOS 登录用户的 `~/Library/LaunchAgents` 或 Windows 当前用户的登录计划任务。重新安装和卸载会检查所有权记录，只处理本产品拥有的服务和文件。若使用的是外部 Codex 应用/CLI，该程序、账号、对话和项目都会保留。

从已安装的程序执行卸载：

```sh
discodex uninstall
```

卸载会停止归产品所有的服务，并清理归产品所有的配置、Discord 凭据、注册命令和安装器安装的配置。现有 Codex 登录、thread 状态和历史、项目及 Discord 服务器记录都会保留。

## Windows 管理员权限与自动启动

Windows 安装器先以当前用户准备配置和 Codex 登录，在注册计划任务时请求一次 UAC 批准。需要同一用户账号的管理员批准；若使用其他管理员账号认证，会停止而不更改任务。若取消 UAC，会保留已准备的文件，但不完成任务注册。

计划任务会在当前用户登录时以 Highest 权限启动 worker。worker 及其子 Codex 命令因此具有管理员权限，一般软件操作不需要再次提升。安装器不会更改 Windows 全局 UAC 策略。当前账号必须是管理员账号；服务不会转换为用户退出登录后运行的 SYSTEM 服务。

只有执行文件、配置路径、用户 SID、登录触发器和所有权记录均匹配时，才会迁移现有 Limited 任务。注册失败会尝试恢复先前任务定义和运行状态。重新安装和卸载会检查所有权记录中的权限级别，并在需要时请求 Windows 批准。计划任务没有常驻运行时限；失败时每隔一分钟最多重启三次。此设置不会检测仍在运行但无响应的 Codex turn。

参考：[Windows 计划任务的安全上下文](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks)、[UAC 与子进程权限](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)。

## macOS 完全磁盘访问权限

安装器无法自动检查或授予 macOS 完全磁盘访问权限（FDA）。如果新安装或重新安装导致执行文件路径变化，安装器会在 native 配置和启动 LaunchAgent 之前检测到路径变化，并显示经所有权验证的新执行文件准确路径，打开 Finder 和系统设置的 FDA 页面。需要该权限时，请把显示的执行文件加入列表并手动开启；若已授权或不需要，可按 Enter 继续，按 `q` 取消。继续并不能确认 macOS 已授予访问权限。没有交互式终端时，安装器会在启动服务前停止并提示执行文件路径。常规同签名、同路径更新不会再次打开 FDA 指引。macOS 对“文件与文件夹”和 AppleEvents 权限的请求由独立系统对话框处理，此指引不会代为授权。

使用 `--migrate-adhoc` 迁移现有 ad-hoc 安装时，也会先在原路径替换为新签名程序，再打开相同 FDA 指引，然后才重启原 LaunchAgent。查看设置后按 Enter 继续更新；按 `q` 会恢复先前执行文件、所有权记录和服务状态。同签名同路径的一般更新不会重新打开该页面。

安装或重置权限后，可运行下列命令直接打开指引。它会在 Finder 中选中所有权记录里的实际执行文件，并打开系统设置的 FDA 页面：

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

用户仍须在 Finder 中把选中的 worker 添加到权限列表并手动开启。此命令不会更改或检查授权状态；若实际安装路径需要权限，请在操作系统设置中处理。

## macOS 更新与权限保留

macOS 首次安装会在该 Mac 的登录钥匙串中创建本地代码签名密钥。已安装的 worker 和之后的更新共用同一证书与固定标识。私钥以不可导出形式保存在钥匙串中；创建过程中产生的临时私钥文件会删除。密钥创建和注册包含在安装流程内。远程安装时，会使用一次性安装 LaunchAgent 在登录用户会话中完成认证，并在操作结束后移除。无需单独打开钥匙串访问；macOS 显示认证对话框时，请选择允许。若钥匙串已锁定或密钥使用授权被拒绝，安装会在替换程序前停止。

签名元数据和公开证书保存在 `~/Library/Application Support/Discodex/signing/`。如果旧的 `Codex Worker/signing/` 目录存在且新位置不存在，会原子地整体迁移，以保留现有签名身份。保留签名身份并不意味着基于路径的 TCC 权限会自动应用到新执行文件路径。路径变化时，上述 FDA 指引会显示新路径；“文件与文件夹”和 AppleEvents 权限仍可能由 macOS 单独请求。若新位置为空但旧的签名 identity/certificate 仍存在，会继续使用原签名目录。卸载配置时会保留共享签名密钥和元数据，以便重新安装。若删除该目录或钥匙串中的签名密钥，现有代码身份将无法延续，因此不会自动生成新密钥。此本地签名不能替代公开分发所需的 Developer ID 签名和公证。

macOS 使用前述通用 `./update --config "<path-to-existing-config.json>"`。脚本会下载 GitHub 正式 Release 的 macOS 程序和 SHA-256 校验值，再调用应用更新流程。仅首次将旧 ad-hoc 签名安装迁移时才添加 `--migrate-adhoc`。`./install --update --config ...` 是相同更新流程的旧别名。更新保留原签名身份，只重启所选配置的服务；原本未运行的服务不会被启动。配置、凭据和 Codex 对话状态都会保留；更新失败时会恢复先前程序、所有权记录和服务状态。

FDA 权限是否仍有效由用户在 macOS 中确认。产品不会修改 TCC 数据库或自动授予权限。Apple 参考：[稳定签名与文件访问权限](https://developer.apple.com/forums/thread/678819)、[代码签名身份和自签名](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)。

## 配置身份与记忆

配置中的 `name` 同时是显示名称和默认对话身份。worker 会在新 thread 和服务重启后恢复既有 thread 时应用对应的智能体默认 prompt。语音 Realtime 使用独立于文字模型 `realtimeStartInstructions` 的 backend prompt。worker 只通过产品拥有的 Codex 子进程 `--config` 传递 `experimental_realtime_ws_backend_prompt`，不会更改 Codex 全局配置。官方 selector 优先使用此设置，其次使用请求 prompt；两者都没有时使用默认 backend prompt。当前 Codex 默认 backend prompt 会自称 Codex，因此需使用独立 prompt 保持已配置的语音身份。不会在连接时自动问候；它会传递通话边界及原对话连续性，并等待用户先说话。修改身份 prompt 无需删除历史或创建新 thread。依据：[prompt selector](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/realtime_prompt.rs)、[默认 realtime backend prompt](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/prompts/templates/realtime/backend_prompt.md)。

`config.json` 可选的 `memoryEnabled: true` 会为该 worker 启用官方 Codex 记忆功能、生成和使用、专用查询/搜索/笔记工具，以及既有 thread 的记忆生成。设置为 `false` 时会在该 worker 中关闭；省略时沿用现有 native 设置。重新安装会保留此字段。不增加安装问题，也不修改其他 Codex 应用的全局设置或账号文件。即使启用记忆，实际提取和整合也受 native 的等待时间和使用量条件影响。

## Discord 内更新

若尚无上次检查记录，则立即检查 GitHub 正式 Release；此后根据持久化的上次检查时间，在到期后再次检查。检查循环约每 15 秒轮询一次是否到期；成功检查的间隔为 6 小时，发现错误后 15 分钟重试。worker 重启时遵循已保存的上次检查时间，不会强制立即检查。通知另有空闲条件：原生连接必须在线，且没有活动 turn、语音通话或清理、待处理批准、未处理输入、消息处理或输出发送；满足这些条件并已空闲至少 5 分钟后，才在主页频道显示通知。若发现更新时已满足空闲条件，不会再额外等待 5 分钟。通知不 @提及，也不触发 push/桌面通知。

通知提供更新、跳过此版本和不再通知按钮。更新会固定通知中 Release 的版本；“跳过此版本”只跳过该版本；“不再通知”关闭此安装配置文件的通知，可在 `/discodex settings` 中重新开启。按钮文案遵循配置语言。

Discodex 会从获准的 GitHub Release 下载适合当前操作系统和架构的预编译程序，并校验 GitHub Release API 提供的 SHA-256 digest 和文件大小。客户端不下载源码或本地构建，因此不需要 Rust Cargo 或平台构建工具。独立 updater 会在替换程序前再次确认系统处于空闲状态。若 Release 没有当前平台的程序或 digest 校验失败，会停止准备并显示结果。更新保留会话、配置、自动启动和 Mac 签名身份，并造成短暂断线重连；失败不会自动重复应用，而会在同一通知中显示结果。

更新通知以正式 GitHub Release 为准，不以 main 分支的每次提交为准。发布时 Cargo 包版本须与 `vX.Y.Z` Release 标签一致。手动更新脚本也会下载对应平台的 Release 程序并校验 SHA-256。

发布者创建匹配版本的 Git 标签和 draft Release 后，在 GitHub Actions 的 `Build release binaries` 工作流中设置 `release_tag` 并运行。工作流会为 Windows、macOS、Linux 的 x64/ARM64 执行单元测试和 release 构建，并向 draft 附加六种程序、SHA-256 校验文件和 `discodex-installers.zip`。确认所有任务和附件后再公开 Release。不得只发布源码归档。

## 任务工具更新

更新后重启 worker，即可在保留现有 Codex thread 的情况下启用 native `update_plan`、CLI 任务和管理工作树工具。不需单独安装 MCP，也不修改用户全局 Codex 设置。任务查询、创建、分支和工作树创建都使用同一官方 app-server 连接；新任务继承发起任务的 native 设置，不会重置既有对话和配置。
