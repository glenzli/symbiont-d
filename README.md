# symbiont-d

![symbiont-d conversation and external-input briefing](assets/symbiont-d-banner.png)

[中文](#中文) · [English](#english)

---

<a id="中文"></a>

## 中文

### 退役声明：通用助手能力交回平台

**日期：2026-09-30**

**项目状态：已退役。** symbiont-d 不再推荐用于新部署，也不再规划新功能。现有代码和文档保留，供已有部署查阅和迁移参考。

symbiont-d 原本把日常对话、外部输入、定时探索和长期上下文连接到一套本地服务中。在我的实际工作流里，日常对话、主动信息整理、工具协调，以及语音、移动端和持续任务，已经可以交给 dots 承接。我决定停止继续扩展这层通用助手服务。

这次退役的范围是 symbiont-d。PCP 管理的长期知识、来源和修订历史仍值得保留；PCP 与 infer-runtime 的独立用途不在此次退役范围内。已有本地数据应按需要备份和迁移，本声明不表示现有服务已经停止或数据已经迁移。

参见 [VASMC 的退役声明](https://github.com/glenzli/vasmc/blob/main/README.md#zh-cn)及[《AI 脚手架的半衰期》](https://glenzli.com/notes/half-life-of-ai-scaffolding/)。下面保留的功能、安装和配置说明记录退役前的实现，供历史参考。

### 退役前项目说明

> **开发预览。** symbiont-d 的接口、配置和本地数据格式仍可能调整。建议在测试环境中使用，并备份 `data/` 中需要保留的数据。

symbiont-d 是一个本地运行的对话与信息整理服务。日常对话通过 Codex app-server 完成；定时探索、IMAP 邮箱和 Google Drive 等外部输入可进入同一个时间线；长期上下文通过 [Paged Context Protocol (PCP)](https://github.com/glenzli/paged-context-protocol) 存储和检索。

项目目前面向单用户、本机部署。Codex 仍负责任务执行、审批和进度管理；symbiont-d 负责对话、输入整理和上下文衔接。

![使用虚构数据的 symbiont-d 对话与外部输入界面](docs/images/conversation-briefing-demo.svg)

*示例界面使用虚构数据。*

### 组件关系

```text
定时探索 / IMAP / Google Drive / 选定的 Codex 上下文
                          │
                          ▼
                    symbiont-d
       ├─ Codex app-server
       ├─ 本地 SQLite / 配置
       ├─ PCP Runtime
       └─ infer-runtime（可选）
```

- **Codex app-server** 提供普通对话、工具调用和定向调查。
- **本地存储** 保存对话、输入状态和运行配置。
- **PCP Runtime** 独立管理 Pages、Revision、来源、关系和检索；symbiont-d 通过 enrollment 连接。
- **infer-runtime** 是可选依赖，用于本地语音转写和部分无状态判断。

### 当前能力

- **对话界面**：支持流式回复、停止、编辑、撤回和重发，并在本地保存对话记录。
- **外部输入**：支持可选的 IMAP 收件箱和 Google Drive 只读接入，以及定时探索和手动调查。输入保留来源，未发布的候选不会写入 PCP。
- **X 账号关注**：可在探索通道设置或对话中管理账号、关注点、呈现偏好及暂停状态。点击“检查关注”或在对话中提出检查时，Symbiont 尝试使用已连接、已登录的浏览器读取可见动态，首次检查建立进度；无法访问时不会推进进度。不会调用 X API，也不会在后台定时检查。
- **长期上下文**：symbiont-d 按运行策略判断普通内容是否需要记录；PCP 负责持久化、版本和检索。
- **临时讨论**：在进程内保存独立对话。默认丢弃，也可由用户选择保留结论或完整记录。
- **语音输入**：通过可选的 infer-runtime 转写本地录音；音频文件本身不写入 PCP。
- **Codex 调用**：仓库附带只读的 `$symbiont` Codex Skill，用于把限定范围的上下文和来源带入当前任务。

### 当前边界

- 服务默认只监听 `127.0.0.1:4317`，没有公共托管或多用户部署配置。
- PCP Runtime 与 Console 由 PCP 仓库独立安装和运行；symbiont-d 的安装脚本不会管理或卸载它们。
- 邮箱、Drive、模型服务和 infer-runtime 均需单独配置。本仓库不包含账号凭据、API Key、邮箱地址、Drive 标识或模型文件。
- 外部输入通道是只读的；来源服务不可用时，相关功能会报告不可用或失败状态。
- 当前版本不承诺配置、数据库或 PCP 客户端接口向后兼容。

### 运行开发版

需要 Rust 1.88+、已登录的 Codex CLI，以及位于同级目录的 [PCP 仓库](https://github.com/glenzli/paged-context-protocol)：

```sh
cargo run
```

打开 <http://127.0.0.1:4317>。作为 macOS 本地常驻服务安装：

```sh
./scripts/service-install.sh
./scripts/service-status.sh
```

请先通过 PCP 自己的安装入口启动 Runtime 与 Console，再在 Console 中批准 symbiont-d 的 enrollment。

开发期修复旧 PCP Page 时，先运行只读预览；确认 `data/pcp-history-repair.json` 后再运行 apply。预览默认使用日常（Terra）计算通道完成审阅，只把来源冲突或证据不足的候选升级到关键（Sol）通道。apply 会申请独立的 `service:symbiont-pcp-repair` enrollment，必须在 PCP Console 单独批准，并且只获得读取与保留历史的 repair 权限。两个命令都是一次性维护进程，不会启动第二个 Web 服务：

```sh
SYMBIONT_RUN_PCP_HISTORY_REPAIR=preview cargo run
SYMBIONT_RUN_PCP_HISTORY_REPAIR=apply cargo run
```

若旧 Page 的直接用户来源以中文为主、当前内容却是英文，可使用独立的语言保真迁移。它只把现有 Page 语义恢复为自然中文，不重新总结或补写事实，并保留原有来源、facets 与 Revision 历史：

```sh
SYMBIONT_RUN_PCP_LANGUAGE_REPAIR=preview cargo run
SYMBIONT_RUN_PCP_LANGUAGE_REPAIR=apply cargo run
```

### 项目导航

- [外部输入角色与生命周期](docs/signal-input-roles.md)
- [临时讨论边界](docs/ephemeral-discussions.md)
- [Codex Skill](integrations/codex-skill/symbiont/SKILL.md)
- [安装 Codex Skill](scripts/install-codex-skill.sh)

---

<a id="english"></a>

## English

### Retirement notice: Handing general assistant capabilities back to the platform

**Date: 2026-09-30**

**Project status: retired.** symbiont-d is no longer recommended for new deployments, and no new features are planned. Existing code and documentation remain available as a reference for existing deployments and migration.

symbiont-d originally brought ordinary conversation, external input, scheduled exploration, and durable context together in a local service. In my actual workflow, dots can now handle ordinary conversation, proactive information gathering and organization, tool coordination, voice, mobile access, and ongoing tasks. I have decided to stop expanding this general-purpose assistant service.

This retirement applies to symbiont-d. The durable knowledge, sources, and revision history managed by PCP remain worth preserving; the independent uses of PCP and infer-runtime are outside the scope of this retirement. Existing local data should be backed up and migrated as needed. This notice does not mean that existing services have been stopped or data has already been migrated.

See [VASMC's retirement notice](https://github.com/glenzli/vasmc/blob/main/README.md#en) and [The Half-Life of AI Scaffolding](https://glenzli.com/en/notes/half-life-of-ai-scaffolding/). The feature, installation, and configuration documentation below describes the implementation before retirement and remains for historical reference.

### Project documentation before retirement

> **Development preview.** symbiont-d interfaces, configuration, and local data formats may change. Use it in a test environment and back up any data under `data/` that needs to be retained.

symbiont-d is a locally run conversation and information-management service. Ordinary conversation uses Codex app-server; scheduled exploration, IMAP mail, and Google Drive can add external input to the same timeline; durable context is stored and retrieved through [Paged Context Protocol (PCP)](https://github.com/glenzli/paged-context-protocol).

The project currently targets a single-user local deployment. Codex remains responsible for task execution, approvals, and progress tracking. symbiont-d handles conversation, input organization, and context transfer.

![A synthetic symbiont-d conversation and external-input briefing](docs/images/conversation-briefing-demo.svg)

*The example uses fictional data.*

### Components

```text
scheduled exploration / IMAP / Google Drive / selected Codex context
                                │
                                ▼
                          symbiont-d
             ├─ Codex app-server
             ├─ local SQLite/config
             ├─ PCP Runtime
             └─ infer-runtime (optional)
```

- **Codex app-server** provides ordinary conversation, tool calls, and directed investigation.
- **Local storage** holds transcripts, input state, and runtime configuration.
- **PCP Runtime** independently manages Pages, Revisions, sources, relations, and retrieval; symbiont-d connects through enrollment.
- **infer-runtime** is optional and provides local speech transcription and some stateless judgments.

### Available in the current build

- **Conversation UI**: streaming responses, stop, edit, retract, and resend, with a locally stored transcript.
- **External input**: optional read-only IMAP and Google Drive connections, scheduled exploration, and manual investigation. Inputs retain their sources; unpublished candidates do not enter PCP.
- **X account watches**: manage accounts, focus, presentation preference, pause and removal through Sources settings or conversation. An explicit check uses the connected, signed-in browser when available and advances progress only for posts actually observed. The first check establishes a baseline. No X API calls or scheduled X checks run.
- **Durable context**: symbiont-d applies its runtime policy to ordinary recording; PCP provides persistence, revision history, and retrieval.
- **Temporary discussions**: isolated in-process conversations that are discarded by default. The user may retain a conclusion or the full transcript.
- **Voice input**: local recordings can be transcribed through optional infer-runtime. Audio files are not written to PCP.
- **Codex recall**: the repository includes a read-only `$symbiont` Codex Skill for bringing bounded context and sources into the current task.

### Current boundaries

- The service listens on `127.0.0.1:4317` by default. There is no public hosting or multi-user deployment configuration.
- PCP Runtime and Console are installed and run independently from the PCP repository. The symbiont-d installer does not manage or remove them.
- Mail, Drive, model services, and infer-runtime require separate configuration. This repository contains no account credentials, API keys, mail addresses, Drive identifiers, or model files.
- External input connections are read-only. When a source service is unavailable, the corresponding feature reports an unavailable or failed state.
- The current release does not guarantee backward compatibility for configuration, databases, or PCP client interfaces.

### Run the development build

The build requires Rust 1.88+, a signed-in Codex CLI, and a sibling checkout of the [PCP repository](https://github.com/glenzli/paged-context-protocol):

```sh
cargo run
```

Open <http://127.0.0.1:4317>. To install it as a persistent local macOS service:

```sh
./scripts/service-install.sh
./scripts/service-status.sh
```

Start Runtime and Console through PCP's own installation path, then approve the symbiont-d enrollment in Console.

To repair older PCP Pages during development, run the read-only preview first and inspect `data/pcp-history-repair.json` before apply. Preview uses the conversation (Terra) compute lane by default and escalates only source-conflicted or genuinely ambiguous candidates to the critical (Sol) lane. Apply requests a separate `service:symbiont-pcp-repair` enrollment that must be approved independently in PCP Console and grants only read plus history-preserving repair. Both commands are one-shot maintenance processes and do not start a second web service:

```sh
SYMBIONT_RUN_PCP_HISTORY_REPAIR=preview cargo run
SYMBIONT_RUN_PCP_HISTORY_REPAIR=apply cargo run
```

For older Pages whose direct user sources are predominantly Chinese but whose current content is English, use the separate language-fidelity migration. It translates the existing Page semantics into natural Chinese without resummarizing or adding facts, while preserving sources, facets, and Revision history:

```sh
SYMBIONT_RUN_PCP_LANGUAGE_REPAIR=preview cargo run
SYMBIONT_RUN_PCP_LANGUAGE_REPAIR=apply cargo run
```

### Project navigation

- [External-input roles and lifecycle](docs/signal-input-roles.md)
- [Temporary-discussion boundary](docs/ephemeral-discussions.md)
- [Codex Skill](integrations/codex-skill/symbiont/SKILL.md)
- [Codex Skill installer](scripts/install-codex-skill.sh)

## License

symbiont-d is available under the [MIT License](LICENSE).
