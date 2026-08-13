# symbiont-d

![A quiet flow from outside signals through conversation into durable context](assets/symbiont-d-banner.png)

<p align="center">
  <a href="#zh">中文</a> · <a href="#en">English</a>
</p>

> A local companion for turning outside signals into better conversations — not another task executor.

<a id="zh"></a>

## 中文

**symbiont-d** 是一个本地运行的信息陪伴层。它把外部输入、正在进行的对话与长期问题放进同一个可继续思考的空间，帮助你决定什么值得讨论、什么值得保留，以及什么只该经过而不留下。

它不是 Jarvis，也不试图成为另一个任务系统。具体执行、审批、进度和任务历史仍属于 Codex；Symbiont-d 的职责是让信息抵达得更自然，让讨论拥有连续性，并在需要时把恰当的上下文带给 Codex。

### 一眼看见它如何工作

下面是完全虚构的数据：外部输入以轻量消息出现，保留原文、来源与可展开的说明；用户真正接住它时，才把它带入主对话与长期上下文。

![使用虚构数据的 symbiont-d 对话与外部输入简报界面](docs/images/conversation-briefing-demo.svg)

### 设计原则

- **信息入口，不是信息流。** Luna、邮箱、Drive 与其他输入通道可以带来研究、工具、项目生态、产业和文化中的变化；它们不是需要不停消费的 feed。
- **外部信息先可见，后决定。** 外部输入默认不写入 PCP。它保留摘要、原文入口与来源，允许回复、引用、删除，或在过期后静默离开；聚焦模式可暂时隐藏尚未接住的输入。
- **自然对话，而非强制轮次。** 可以停止、撤回、编辑并从任意位置重发；模型也可以没有增量时不回答。临时讨论沿用已有认识，却不会自动进入记忆，只有在你决定后才会沉淀。
- **记忆是可复核的工作。** 候选池不是记忆。只有保留来源、修正与替代解释的信息才会进入长期上下文；旧主题、假设和关联可以被复核、修订或撤回。
- **连续性优先于完美聚类。** 允许主题重叠、纠正和渐进收束，而不把每一句自然语言拆成无穷组件，也不承诺绝对正确的关联。
- **克制的主动性。** 广域探索可带回有趣的输入；对于值得认真质疑的说法，系统可以以派生的“异议”消息补充反证或边界，但是否说话始终比是否分析更严格。

### 协作边界

```text
外部世界 / 你的对话 / 你选择的 Codex 上下文
                    ↓
              symbiont-d
   感知 · 输入简报 · 讨论 · 临时探索 · 当前地图
                    ↓
      值得保留的 Pages、来源、修订与关系
                    ↓
               PCP Runtime
```

- **与 Codex：** 可以把相关的 Symbiont 认识、用户确认和来源证据带入 Codex，也可以把选定的 Codex 对话作为一次性只读上下文拉回。Symbiont-d 不替你派发、接管或伪造 Codex 任务。
- **与长期上下文：** Symbiont-d 作为 Host，决定何时写入、复核、修订或撤回，并通过 [Paged Context Protocol (PCP)](https://github.com/glenzli/paged-context-protocol) 使用 Pages、不可变 Revision、来源、关系和检索能力。
- **与本地数据：** 数据默认留在本机。PCP Console 是独立的只读观察界面；账号凭据、邮箱地址、文件夹标识和运行记录都不属于仓库内容。

### 输入与体验

- **广域输入与角色简报：** 每个启用的输入通道可拥有自己的昵称和头像。相邻输入会轻量组合，仍可逐条展开、回复或删除；原文与来源不被“摘要的摘要”替代。
- **研究收件箱与 Drive：** 可选的只读渠道，适合把 Scholar、Spark、GitHub 或人工转发的资料汇入同一个候选入口。已读游标与凭据只保留在本地。
- **语音输入：** 通过可选的本地 [infer-runtime](https://github.com/glenzli/infer-runtime) 完成授权转写，录音时显示实际音量波形；语音文件本身不会成为 PCP 记忆。
- **可见的状态：** 探索、转写、重连和失败都会在界面中留下简洁的状态与重试入口，而不是让聊天看起来像卡住了。

### 运行

当前原型需要：Rust 1.88+、已登录的 Codex CLI，以及同级 checkout 的 [PCP 仓库](https://github.com/glenzli/paged-context-protocol)。语音输入和部分无状态后台判断可选用 [infer-runtime](https://github.com/glenzli/infer-runtime)。

```bash
cargo run
```

然后打开 <http://127.0.0.1:4317>。若希望作为本地常驻服务运行：

```bash
./scripts/service-install.sh
```

安装脚本会配置身份绑定的 PCP Runtime、只读 PCP Console 与 symbiont-d；卸载不会删除本地数据。

<p align="right"><a href="#en">English ↓</a></p>

---

<a id="en"></a>

## English

**symbiont-d** is a local information companion. It keeps outside input, an ongoing conversation, and long-running questions in one place where they can remain available for thought — helping you decide what deserves a conversation, what deserves memory, and what should simply pass by.

It is not Jarvis, and it is not another task system. Execution, approvals, progress, and task history remain with Codex. Symbiont-d makes information arrive more naturally, gives discussion continuity, and carries the right context into Codex when useful.

### See the interaction model

The image below uses entirely fictional data. External input arrives as a lightweight message with source material still available; it becomes part of the main conversation and durable context only when a person actually takes it up.

![A synthetic symbiont-d conversation and external-input briefing](docs/images/conversation-briefing-demo.svg)

### Principles

- **An information doorway, not a feed.** Luna, mail, Drive, and other channels can bring in change across research, tools, project ecosystems, industry, and culture without becoming an endless stream to consume.
- **External input is visible before it is decided.** It does not enter PCP by default. A source card keeps its summary, original material, and provenance available to reply to, quote, dismiss, or let expire; focus mode can temporarily hide input that has not been taken up.
- **Natural conversation, not forced turns.** A message can be stopped, retracted, edited, and resent. The model may remain silent when there is no meaningful addition. Temporary discussion carries current understanding forward without automatically entering memory.
- **Memory is reviewable work.** A candidate pool is not memory. Only information that retains sources, corrections, and alternatives enters durable context; old topics, hypotheses, and links can be reviewed, revised, or retracted.
- **Continuity over perfect clustering.** Topics may overlap, be corrected, and converge gradually. The system does not split every sentence into infinite components or promise perfectly correct associations.
- **Deliberate initiative.** Broad exploration can surface interesting input. When a claim deserves careful challenge, a derived dissent message may add counter-evidence or a boundary — but speaking has a higher bar than analysing.

### Boundaries

```text
outside world / your conversation / selected Codex context
                          ↓
                    symbiont-d
  sensing · input briefing · discussion · temporary inquiry · current map
                          ↓
       Pages, sources, revisions, and relations worth retaining
                          ↓
                     PCP Runtime
```

- **With Codex:** Symbiont-d can carry relevant understanding, user confirmations, and source evidence into Codex, or bring a selected Codex conversation back as bounded read-only context. It does not dispatch, take over, or impersonate Codex tasks.
- **With durable context:** Symbiont-d is the Host. It decides when memory is written, reviewed, revised, or retracted, using [Paged Context Protocol (PCP)](https://github.com/glenzli/paged-context-protocol) for Pages, immutable Revisions, provenance, relations, and retrieval.
- **With local data:** Data stays local by default. PCP Console is a separate read-only observation surface; credentials, mail addresses, folder identifiers, and runtime records are never repository content.

### Inputs and experience

- **Broad input and role briefings:** Every enabled channel can have its own name and avatar. Nearby entries are grouped lightly but remain individually expandable, replyable, and dismissible; a summary never replaces access to source material.
- **Research inbox and Drive:** Optional read-only paths for bringing Scholar, Spark, GitHub, and human-forwarded material into one candidate inlet. Read cursors and credentials stay local.
- **Voice input:** User-authorized local transcription runs through optional [infer-runtime](https://github.com/glenzli/infer-runtime), with a live amplitude waveform while recording. The audio file itself is not PCP memory.
- **Visible state:** Exploration, transcription, reconnection, and failures have compact status and retry affordances rather than making the chat feel stalled.

### Run it

This prototype needs Rust 1.88+, a signed-in Codex CLI, and a sibling checkout of the [PCP repository](https://github.com/glenzli/paged-context-protocol). Voice input and some stateless background judgments can additionally use [infer-runtime](https://github.com/glenzli/infer-runtime).

```bash
cargo run
```

Then open <http://127.0.0.1:4317>. To keep it running as a local service:

```bash
./scripts/service-install.sh
```

The installer configures an identity-bound PCP Runtime, a read-only PCP Console, and symbiont-d. Removing the service preserves local data.

<p align="right"><a href="#zh">中文 ↑</a></p>
