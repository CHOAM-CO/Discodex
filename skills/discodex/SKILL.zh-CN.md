---
name: discodex
description: 帮助使用、配置、更新和排查运行此智能体的 Discord 桥接程序 Discodex。适用于 Discodex 设置、支持的行为、消息或文件传递、语音连接及安装问题。
---

# Discodex

本指南随 Discodex **{{version}}** 提供。回答桥接程序相关问题时，使用已安装版本的参考文档和当前工具结果。

只阅读相关章节：

- [安装与配置](references/installation.zh-CN.md)：配置文件、语言、提供商与模型、批准设置、更新、平台服务和卸载。
- [行为与设计](references/design.zh-CN.md)：主频道与来源频道的回复、附件与表格、语音收件箱与转录、持续上下文、任务工具、记忆和更新流程。

Discodex 将 Discord 连接到原生 Codex 运行时。排查问题时，区分 Discord 传递、worker 行为、原生模型或工具执行、提供商响应与主机操作系统行为。依据实际错误和当前设置定位问题；worker 正在运行并不能证明消息传递或语音正常。

当前工具 schema 和返回的目录说明本会话可用的功能。进行更改时使用这些信息；文档说明流程，但不能证明操作已成功。报告实际观察到的结果。

新版本请查阅[官方发布](https://github.com/CHOAM-CO/Discodex/releases)。所附参考文档来自此版本的[设计](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/design.zh-CN.md)和[安装](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/installation.zh-CN.md)文档。指向其他语言或仓库页面的相对链接可从这些源页面打开。
