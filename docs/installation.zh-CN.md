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

若没有保存的安装清单，`discodex install` 会交互式创建清单；`install-plan` 只创建相同清单。安装者需指定配置显示名称、Codex 数据目录（共享现有目录或 Discodex 独立目录）、Discord 服务器、主页文字频道、转录频道、允许的语音频道、操作者 ID、接收策略和注册用户/频道 ID、已存在的项目目录及可选模型。接收策略为 `Everyone`（所有服务器和私信）、`RegisteredServer`（指定服务器）、`RegisteredChannels`（注册频道及子线程）或 `Paired`（注册用户 ID）。允许和拒绝列表独立于策略保存，拒绝列表优先。策略只决定消息接收范围；slash command 和管理权限遵循配置的操作者。首次安装计划中的策略和列表只在安装时写入运行时 `message-access.json`；之后重新安装或更新会保留当前值。

配对用户和允许列表中的用户可直接发送私信，无须提及机器人，也不受所选服务器或频道接收策略限制。拒绝列表和忽略自身消息的规则仍优先；`Everyone` 还允许其他私信发送者。普通自动输出、问题、批准请求和进度仍发在主页频道。回复实时收到的私信时，智能体通过 `discord_send_message` 的 `dm_id` 选择输入中的 `source.dm_id`，发回原私信。

Discord token 通过安全输入提供。安装器会检查 Discord 访问和 native 连接。若 Codex 需要登录或 MFA，请在官方登录流程中完成。worker `config.json` 中的 `effort` 和 `serviceTier` 用于新建持久 thread；`serviceTier` 设为 `off` 时会向 native 传递清除 tier 的请求。未设置则遵循 native 默认值；恢复既有 thread 时，不会覆盖通过 `/discodex` 修改的当前 thread 设置。

仅保存安装选择清单并不代表服务已准备完成。安装器会检查 native 账号、catalog 和 Discord 配置，并注册用户服务。服务成功启动本身不能证明文字、问题/批准或语音功能均已验证。

## 受管理的运行时与 Codex 数据

Discodex 1.0 使用 `third_party/codex` 固定的预编译 Codex 分支，不再选择已安装桌面应用或 CLI 的 core。沿用官方包布局，包含 `bin/codex`、`bin/codex-code-mode-host`、`rg`，以及平台需要的 Linux `bwrap`、Windows command-runner 和 sandbox-setup helper。Windows 可执行文件带 `.exe` 后缀。

新安装可共享已有 Codex 数据目录，沿用登录、记忆、配置和会话历史；检测到现有目录时推荐此选项。也可在 Discodex 配置数据下创建独立目录。重装和更新保留当前目录。功能偏好由 Discodex 保存并传给其子进程或原生会话设置，不覆盖共享 `config.toml`。不同客户端可以使用共享目录中的不同会话，不应同时操作同一个会话。

每个版本提供原有 `discodex-<target>[.exe]`、`discodex-runtime-<target>.tar.gz` 和 `discodex-runtime-<target>.json`。清单只包含 worker 版本、准确分支提交、平台、归档校验和及包内相对路径校验和，不包含用户配置、账户、凭据、主机地址或本地安装路径。更新先准备版本化运行时，再切换拥有的 worker、配置和所有权记录。运行时准备失败时，响应可能已中断；已安装文件保持不变，也不自动恢复响应。更新不复制或删除原生数据目录。替换或重启失败时按下文所述顺序尝试恢复；清理时保留未登记的用户文件。

本指南描述 1.0 源码。下载版本以公开 Release 标签为准；平台构建与用户验证结果另行报告。

配置项目包含默认的 `.agents/skills/discodex/` 技能。简短的 `SKILL.md` 使用配置语言，指向同一构建的四语言安装与设计文档，让智能体按需读取设置、更新或诊断所需的参考内容。安装、更新激活和新的原生 app-server 连接准备阶段会同步这些帮助文件。已有的用户同名技能保持不变；受管理技能中被用户修改的文件逐个保留。帮助同步失败时仅报告警告，不阻止机器人连接。基本提示词和共享 `CODEX_HOME` 保持不变。

## 自定义与本地 Codex provider

请在所选 Codex 进程实际使用的同一个 `CODEX_HOME/config.toml` 中配置 provider 和模型。Codex 内置 `ollama`、`lmstudio` provider，也支持自定义 Responses API provider。Provider 认证由 Codex 处理。`env_key` 指定的凭据必须能在 worker 服务环境中读取；请勿将密钥写入 Discodex 配置文件。使用 native 非 OpenAI provider 本身不要求 ChatGPT 账号或 OpenAI API key。

对于 Ollama，请填写已安装的模型名称：

```toml
model_provider = "ollama"
model = "example-local-model"
```

自定义 Responses API provider 示例：

```toml
model_provider = "demo"
model = "example-model"

[model_providers.demo]
name = "Example provider"
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
wire_api = "responses"
# 仅在提供商提供输入 token 计数 API 时启用：
# input_tokens_endpoint = "/responses/input_tokens"
```

请将占位符模型名称和 endpoint 替换为 provider 支持的值。实时 `model/list` catalog 只由当前 active provider 提供。原生发现支持 Codex 的 `{ "models": [...] }` 和兼容的 `{ "data": [...] }` 目录。兼容条目提供模型 ID，并可通过 `context_window`、`context_length`、`max_model_len` 或 `meta.n_ctx` 提供上下文容量；未声明的能力使用原生模型默认值。这不保证每个提供商都提供可发现的目录。Discodex 也使用 worker 有效配置中的默认、subagent、安装及当前 thread 模型。未激活的 profile 文件不会自动枚举。

要启用输入 token 计数，请取消提供商表中 `input_tokens_endpoint` 的注释，并填写提供商给出的端点。路径会附加到 `base_url` 后，以上 `/responses/input_tokens` 对应 `https://provider.example/v1/responses/input_tokens`；也可用绝对 URL 指定其他端点。请求复用现有提供商的认证和传输。

提供商需实现 [OpenAI 输入 token 计数协议](https://developers.openai.com/api/docs/guides/token-counting)，针对生成所用的相同模型输入、指令和工具返回 `{ "object": "response.input_tokens", "input_tokens": 123 }`。分词由提供商负责。省略此设置时保留原生用量估算，不会根据提供商或模型名称推断 API 支持。

实时 `model/list` 结果来自当前 active native catalog。Discodex 也会显示在 native 配置的 `[profiles.NAME]` 表中同时设置 `model` 和 `model_provider` 而明确注册的模型。仅定义 provider 不会注册模型，独立的 `*.config.toml` profile 文件也不会自动加载或扫描。例如：

```toml
[model_providers.demo]
name = "Example provider"
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
wire_api = "responses"

[profiles.example]
model = "example-model"
model_provider = "demo"
```

当前 catalog 和已注册 profile 会提供模型选项。选择模型时，会在同一个持久 thread 中切换 provider 并保留历史；同一 provider 内的更改可立即应用，跨提供商或执行器的更改需要在当前回复或语音结束后重新选择，不会排入队列。能力和推理强度选项遵循原生元数据；兼容目录所用的默认值不代表已验证提供商的实际能力。请参阅 [Codex provider 配置指南](https://developers.openai.com/codex/config-advanced/)和[官方模型元数据 schema](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs)。

主模型和默认子模型的选择同时保存执行器、提供商与模型；子会话可使用其他已注册提供商而不切换父会话。`[model_providers.<id>.capabilities]` 下的 `encrypted_content` 和 `agent_messages` 是独立的可选布尔值。省略时沿用原生默认分类：OpenAI 与 Azure Responses 默认支持加密历史重放，只有原生 OpenAI 分类默认支持 `AgentMessage`。显式值应符合提供商的实际协议支持。即使 `encrypted_content = true`，不支持原生 agent-message 格式时，其不透明正文也会在普通 assistant 消息中标明不可用。原始历史保持不变；详见[历史兼容性](design.zh-CN.md#提供商切换与加密历史)。

## 批准策略、高级设置与 Antigravity

`/discodex settings` 将**批准策略**与**权限配置**分开。配置默认值是 `on-request`；`never` 只在用户明确选择时使用。页面区分保存的策略与当前会话的实际值。活动会话通过原生设置 API 更新，开始或恢复会话时也传入所选策略。Windows 管理员权限及模型自身安全行为与此策略独立。 审批选择按配置文件保存在各自的 `native-features.json` 中，不覆盖共享 Codex 配置。

主设置只显示一次**默认子智能体模型**；**高级设置**包含详细功能和 **Antigravity**。原生进程功能开关在 worker 重启后生效；Antigravity 设置在下次模型选择或 AGY 会话启动时生效。保存的子模型默认值在下次创建或恢复线程时读取，包括 worker 重启后的恢复。当前线程在恢复前保持有效设置，现有子会话也保留各自配置。

Antigravity 默认关闭。可在安装时选择，或在 Discord 的**高级设置 → Antigravity**中启用。安装程序下载并验证官方预编译 CLI，放入 Discodex 配置专属位置，不复用或覆盖外部 AGY 程序。选择**共享现有设置**或**独立设置**；后者通过原生 `--gemini_dir` 和 `--app_data_dir` 使用配置内的设置目录。两者仍使用 AGY 原生操作系统账户密钥环，因此独立设置不代表账户隔离。

准备过程检查登录状态，需要时启动 AGY 原生交互登录。Discord 展示 CLI 的实际提示和浏览器登录链接，并提供选项按钮和所需授权码输入。浏览器授权由用户完成。可用**取消**或**退出**停止准备；安装器准备过程中使用 Ctrl+C 或 Ctrl+D，选项阶段可输入 `q`。取消不会退出 AGY 账户或撤销已完成的授权。只有登录和模型列表验证成功后才启用 AGY。补充的准确模型 ID 可每行输入一个。设置在下一次模型选择或执行时生效，无需重启 worker；订阅凭据不通过 SDK 传递。

启用后，Discodex 从已安装 CLI 的 `agy models` 读取准确模型 ID 和名称，从 `agy --help` 读取推理强度选项，不发送模型提示。两处模型列表显示发现结果，手动填写的 ID 用于补充。CLI 帮助列出的是参数选项，不是已验证的各模型能力元数据。不会按模型名称猜测 ID 或映射强度。列表读取失败会显示在设置中。明确指定的不受支持强度会原样传给 CLI，并可能返回错误，不会自动替换为其他值。

AGY 文字任务使用其重叠的内置工具，额外的 Discord 和任务工具通过项目 MCP 连接，并保留其他 MCP 条目。子智能体生命周期由 Codex 分支管理。每次响应启动新的 AGY CLI，重新输入原生对话和可见工具结果。原生历史与记忆留在 Codex，不恢复 AGY 私有推理或隐藏状态。即使由 AGY 执行任务，语音仍通过所选 Codex 目录的账户连接 Codex Realtime；仅有 AGY 登录无法认证语音连接。也不保证能关闭 AGY 的全部内置团队功能；不会用测试强制模型选择某个工具。

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

更新脚本会从 GitHub 正式 Release 下载当前平台的预编译程序和 SHA-256 校验值。不修改源码 checkout，也不从源码构建。校验通过的程序会交给 `discodex update --source <new-binary> --config <existing-config>`。新版本 updater 准备同一 Release 的 Codex 运行时后，只更新指定且归产品所有的配置。`./install --update --config ...` 和 Windows 的 `./scripts/install.ps1 -Update -Config ...` 是同一更新流程的别名。

更新保留现有配置的语言、设置、凭据和持久 Codex 对话状态，不会创建新配置。替换或重启失败时，更新程序会尝试恢复原 worker、配置、运行时引用、所有权记录和服务状态。必要的恢复步骤失败时，停止依赖该步骤的后续操作并报告原因，不会重启新旧文件混用的安装。未使用的暂存运行时会在必要的服务恢复后清理，清理失败也会报告。为回滚或数据保留而留下的旧目录不会自动删除。需要更改现有配置或对话状态时，应使用单独且明确的迁移流程；更新本身不会执行这些更改。

仅操作者可用的`/discodex update`会立即检查最新正式 Release，不受 6 小时检查间隔限制。若已有固定目标正在安装，会保留该目标并报告当前状态。否则会使用配置语言显示“更新”或“取消”确认，并明确提示“更新”会中断当前任务（包括 subagent 任务）并结束已连接的语音会话。取消不会影响当前工作。“更新”先下载并验证 Release；worker 下载或验证失败时，当前工作会继续运行。只有准备成功后才中断工作、结束语音并立即重启安装。重启后会在主页频道另发完成或失败消息，不受通知设置影响。对话历史、配置和设置会保留；不会自动重新启动已中断的响应，也不会发送恢复 prompt 或空输入。

## 服务与卸载

用户服务安装为 Linux `systemd --user`、macOS 登录用户的 `~/Library/LaunchAgents` 或 Windows 当前用户的登录计划任务。重新安装和卸载会检查所有权记录，只处理本产品拥有的服务和文件。若使用的是外部 Codex 应用/CLI，该程序、账号、对话和项目都会保留。

从已安装的程序执行卸载：

```sh
discodex uninstall
```

卸载会停止归产品所有的服务，并清理归产品所有的配置、Discord 凭据、注册命令和安装器安装的配置。现有 Codex 登录、thread 状态和历史、项目及 Discord 服务器记录都会保留。

同时删除此配置拥有的 AGY 程序。外部 AGY 安装、共享设置和原生操作系统账户密钥环保持不变，不执行 AGY 退出登录。

## Windows 管理员权限与自动启动

Windows 安装器先以当前用户准备配置和 Codex 登录，在注册计划任务时请求一次 UAC 批准。需要同一用户账号的管理员批准；若使用其他管理员账号认证，会停止而不更改任务。若取消 UAC，会保留已准备的文件，但不完成任务注册。

计划任务会在当前用户登录时以 Highest 权限启动 worker。worker 及其子 Codex 命令因此具有管理员权限，一般软件操作不需要再次提升。安装器不会更改 Windows 全局 UAC 策略。当前账号必须是管理员账号；服务不会转换为用户退出登录后运行的 SYSTEM 服务。

只有执行文件、配置路径、用户 SID、登录触发器和所有权记录均匹配时，才会迁移现有 Limited 任务。注册失败会尝试恢复先前任务定义和运行状态。重新安装和卸载会检查所有权记录中的权限级别，并在需要时请求 Windows 批准。计划任务没有常驻运行时限；失败时每隔一分钟最多重启三次。此设置不会检测仍在运行但无响应的 Codex turn。

参考：[Windows 计划任务的安全上下文](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks)、[UAC 与子进程权限](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)。

## macOS 完全磁盘访问权限

安装器无法自动检查或授予 macOS 完全磁盘访问权限（FDA）。如果新安装或重新安装导致执行文件路径变化，安装器会在 native 配置和启动 LaunchAgent 之前检测到路径变化，并显示经所有权验证的新执行文件准确路径，打开 Finder 和系统设置的 FDA 页面。需要该权限时，请把显示的执行文件加入列表并手动开启；若已授权或不需要，可按 Enter 继续，按 `q` 取消。继续并不能确认 macOS 已授予访问权限。没有交互式终端时，安装器会在启动服务前停止并提示执行文件路径。常规同签名、同路径更新不会再次打开 FDA 指引。macOS 对“文件与文件夹”和 AppleEvents 权限的请求由独立系统对话框处理，此指引不会代为授权。

使用 `--migrate-adhoc` 迁移现有 ad-hoc 安装时，也会先在原路径替换为新签名程序，再打开相同 FDA 指引，然后才重启原 LaunchAgent。查看设置后按 Enter 继续更新；按 `q` 会取消，并按上述恢复顺序尝试恢复先前执行文件、所有权记录和服务状态。同签名同路径的一般更新不会重新打开该页面。

安装或重置权限后，可运行下列命令直接打开指引。它会在 Finder 中选中所有权记录里的实际执行文件，并打开系统设置的 FDA 页面：

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

用户仍须在 Finder 中把选中的 worker 添加到权限列表并手动开启。此命令不会更改或检查授权状态；若实际安装路径需要权限，请在操作系统设置中处理。

## macOS 更新与权限保留

macOS 首次安装会在该 Mac 的登录钥匙串中创建本地代码签名密钥。已安装的 worker 和之后的更新共用同一证书与固定标识。私钥以不可导出形式保存在钥匙串中；创建过程中产生的临时私钥文件会删除。密钥创建和注册包含在安装流程内。远程安装时，会使用一次性安装 LaunchAgent 在登录用户会话中完成认证，并在操作结束后移除。无需单独打开钥匙串访问；macOS 显示认证对话框时，请选择允许。若钥匙串已锁定或密钥使用授权被拒绝，安装会在替换程序前停止。

签名元数据和公开证书保存在 `~/Library/Application Support/Discodex/signing/`。如果旧的 `Codex Worker/signing/` 目录存在且新位置不存在，会原子地整体迁移，以保留现有签名身份。保留签名身份并不意味着基于路径的 TCC 权限会自动应用到新执行文件路径。路径变化时，上述 FDA 指引会显示新路径；“文件与文件夹”和 AppleEvents 权限仍可能由 macOS 单独请求。若新位置为空但旧的签名 identity/certificate 仍存在，会继续使用原签名目录。卸载配置时会保留共享签名密钥和元数据，以便重新安装。若删除该目录或钥匙串中的签名密钥，现有代码身份将无法延续，因此不会自动生成新密钥。此本地签名不能替代公开分发所需的 Developer ID 签名和公证。

macOS 使用前述通用 `./update --config "<path-to-existing-config.json>"`。脚本会下载 GitHub 正式 Release 的 macOS 程序和 SHA-256 校验值，再调用应用更新流程。仅首次将旧 ad-hoc 签名安装迁移时才添加 `--migrate-adhoc`。`./install --update --config ...` 是相同更新流程的旧别名。更新保留原签名身份，只重启所选配置的服务；原本未运行的服务不会被启动。配置、凭据和 Codex 对话状态都会保留；更新失败时按上述顺序尝试恢复原安装配置和服务状态。

FDA 权限是否仍有效由用户在 macOS 中确认。产品不会修改 TCC 数据库或自动授予权限。Apple 参考：[稳定签名与文件访问权限](https://developer.apple.com/forums/thread/678819)、[代码签名身份和自签名](https://developer.apple.com/library/archive/technotes/tn2206/_index.html)。

## 配置身份与记忆

配置中的 `name` 同时是显示名称和默认对话身份。worker 会在新 thread 和服务重启后恢复既有 thread 时应用对应的智能体默认 prompt。语音 Realtime 使用独立于文字模型 `realtimeStartInstructions` 的 backend prompt。worker 只通过产品拥有的 Codex 子进程 `--config` 传递 `experimental_realtime_ws_backend_prompt`，不会更改 Codex 全局配置。官方 selector 优先使用此设置，其次使用请求 prompt；两者都没有时使用默认 backend prompt。当前 Codex 默认 backend prompt 会自称 Codex，因此需使用独立 prompt 保持已配置的语音身份。不会在连接时自动问候；它会传递通话边界及原对话连续性，并等待用户先说话。修改身份 prompt 无需删除历史或创建新 thread。依据：[prompt selector](https://github.com/CHOAM-CO/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/core/src/realtime_prompt.rs#L5-L24)、[默认 realtime backend prompt](https://github.com/CHOAM-CO/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/prompts/templates/realtime/backend_prompt.md#L1-L3)。

`config.json` 可选的 `memoryEnabled: true` 会为该 worker 启用官方 Codex 记忆功能、生成和使用、专用查询/搜索/笔记工具，以及既有 thread 的记忆生成。设置为 `false` 时会在该 worker 中关闭；省略时沿用现有 native 设置。重新安装会保留此字段。不增加安装问题，也不修改其他 Codex 应用的全局设置或账号文件。即使启用记忆，实际提取和整合也受 native 的等待时间和使用量条件影响。

## Discord 内更新

自动更新通知按持久化的上次检查时间检查 GitHub 正式 Release：成功后每 6 小时检查，失败后 15 分钟重试；worker 重启会遵循已保存的时间。自动通知仅在原生连接在线，且没有活跃 turn、语音连接或清理、待处理批准、未处理输入、消息处理或输出发送，并已空闲 5 分钟后，才显示在主页频道。若发现更新时已满足空闲条件，不会再额外等待 5 分钟。检查循环约每 15 秒运行一次。不会 @提及，也不会触发 push/桌面通知。

通知提供“更新”“跳过此版本”和“不再通知”按钮。点击“更新”会先下载并验证所选 Release；worker 下载或验证失败时，当前工作会继续运行。准备成功后才中断当前工作（包括 subagent 任务）、结束已连接的语音会话并立即重启安装，不等待通知的 5 分钟计时器。重启后会在主页频道另发完成或失败消息，不受通知设置影响。对话历史、配置和设置会保留；不会自动重新启动已中断的响应，也不会发送恢复 prompt 或空输入。“跳过此版本”只跳过该 Release；“不再通知”关闭此安装配置文件的通知，可在 `/discodex settings` 中重新开启。按钮文案遵循配置语言。

Discodex 会从获准的 GitHub Release 下载适合当前操作系统和架构的预编译程序，并校验 GitHub Release API 提供的 SHA-256 digest 和文件大小。客户端不下载源码或本地构建，因此不需要 Rust Cargo 或平台构建工具。只有准备成功后，独立 updater 才会替换程序并立即重启服务。下载或校验失败会保留当前程序和运行中的 worker，并报告结果。成功更新会保留对话历史、配置、设置、自动启动和 Mac 签名身份，并造成短暂断线重连。

更新通知以正式 GitHub Release 为准，不以 main 分支的每次提交为准。发布时 Cargo 包版本须与 `vX.Y.Z` Release 标签一致。手动更新脚本也会下载对应平台的 Release 程序并校验 SHA-256。

发布者创建匹配版本的 Git 标签和 draft Release 后，在 GitHub Actions 的 `Build release binaries` 工作流中设置 `release_tag` 并运行。工作流会为 Windows、macOS、Linux 的 x64/ARM64 执行单元测试和 release 构建，并向 draft 附加六种 worker 程序、SHA-256 校验文件、各平台 Codex 运行时归档和清单以及 `discodex-installers.zip`。确认所有任务和附件后再公开 Release。不得只发布源码归档。

## 任务工具更新

更新后重启 worker，即可在保留现有 Codex thread 的情况下使用 native `update_plan`、CLI 任务和管理工作树工具。每次计划变化都会显示为新的主页 Embed 卡片，绝不编辑旧卡片；相同事件回放会被抑制，完整计划文本仅在超出 Discord 限制时分页。不需单独安装 MCP，也不修改用户全局 Codex 设置。任务查询、创建、分支和工作树创建都使用同一官方 app-server 连接；新任务继承发起任务的 native 设置，不会重置既有对话和配置。

## 高级设置、插件与运行日志

Native app-server 功能按配置文件保存在各自的 `native-features.json` 中。功能包括 CodeMode、host、multiagent v2 与 `wait_agent`、普通模式问题、clock、token budget/history notes 和异步通知。设置界面会分别显示实际生效的 native 状态与保存值，并标出需要重启 worker 的更改。

使用 `/discodex plugins` 浏览 native 插件目录并通过 Discord 按钮调用支持的读取、安装和卸载操作。推荐由 native elicitation 提供。插件认证、批准、账户可用性和 entitlement 仍由官方 native 流程处理，具体取决于账户和目录。

可在 Discord 中通过按钮或自由文本回答异步 native 问题，并保留原问题 ID 与 native thread 关联。异步通知会发送到主页频道，不会结束当前活动 turn。

主控制事件队列由受监督的 worker 消费，并按配置文件持久保存日志。即使 Windows GUI worker 没有附加控制台，也能检查运行记录。更新使用预编译 Release 资产，客户端无需从源码构建。
