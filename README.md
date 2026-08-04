# symbiont-d

![A quiet flow from outside signals through conversation into durable context](assets/symbiont-d-banner.png)

<p align="center">
  <a href="#zh">中文</a> · <a href="#en">English</a>
</p>

> A local information companion for sustained thinking — not a task executor.

<a id="zh"></a>

## 中文

**symbiont-d** 是一个本地运行的信息陪伴层：它帮助人把外部信息、正在进行的讨论和长期问题放进同一个可继续思考的空间。

它不是 Jarvis，也不试图成为另一个任务系统。具体执行、审批、进度和任务历史仍属于 Codex；Symbiont-d 的职责是帮助你理解信息、发现真正值得进入对话的信号，并在需要时把恰当的上下文带给 Codex。

### 设计

- **信息入口，而非信息流。** 它可以主动感知研究、工具、项目生态、产业与文化的变化，但不会把结果做成需要不停消费的 feed。
- **自然对话，而非强制轮次。** 用户可以停止、撤回、编辑并从任意位置重新发送；模型也可以在没有增量时不回答。主动探索没有新情报时会给出清楚的结果提示，不会假装仍在工作。
- **记忆是可复核的工作，而不是自动归档。** 临时候选池不等于记忆；只有经过更强判断、保留来源和替代解释的信息才会进入长期上下文。假设和主题都会被定期复核、过期或降级。
- **连续性比完美聚类更重要。** 系统允许主题重叠、修正和渐进收束，不把每一句自然语言拆成无穷的 component，也不承诺“绝对正确”的关联。
- **人始终保有边界。** 权限、探索节奏、打扰频率和可见工作状态都可检查、可调整；后台行为默认克制。

### 它如何协作

```text
外部世界 / 你的对话 / 你选择的 Codex 上下文
                    ↓
              symbiont-d
    感知 · 讨论 · 当前地图 · 未决问题 · 反思
                    ↓
       值得保留的 Pages、来源与关系
                    ↓
               PCP Runtime
```

- **与 Codex：** Symbiont-d 可以把选定的 Codex 对话作为一次性、只读的自然上下文，也可以导出一份上下文包供你带到 Codex。它不替你派发、接管或伪造 Codex 任务。
- **与 PCP：** Symbiont-d 是 Host，负责判断何时写入、复核、修订或撤回记忆；[Paged Context Protocol (PCP)](https://github.com/glenzli/paged-context-protocol) Runtime 提供持久的 Pages、不可变 Revision、来源、关系和检索能力。
- **与本地数据：** 数据默认留在本机。PCP Console 是独立的只读观察界面；Symbiont-d 自身只呈现与当前协作有关的工作状态。

### 运行

此仓库目前是早期原型，需要：

- Rust 1.88+；
- 已登录的 Codex CLI；
- [Paged Context Protocol](https://github.com/glenzli/paged-context-protocol) 仓库的同级 checkout 与 PCP Runtime。

本地开发：

```bash
cargo run
```

然后打开 <http://127.0.0.1:4317>。

若希望作为常驻本地服务运行：

```bash
./scripts/service-install.sh
```

安装脚本会配置身份绑定的 PCP Runtime、只读 PCP Console 与 symbiont-d。重新运行该脚本可更新本地服务；卸载不会删除你的本地数据。

### 现状

Symbiont-d 正在以真实的长期使用场景校准探索、记忆整理和主动交流的边界。它宁可遗漏弱信号，也不应把噪声、猜测或内部工作过程伪装成值得打扰你的信息。

<p align="right"><a href="#en">English ↓</a></p>

---

<a id="en"></a>

## English

**symbiont-d** is a local information companion: a place where outside signals, an ongoing conversation, and long-running questions can remain available for thought.

It is not Jarvis, and it is not another task system. Execution, approvals, progress, and task history remain with Codex. Symbiont-d helps you understand information, notice signals that genuinely belong in the conversation, and carry the right context into Codex when useful.

### Design

- **An information doorway, not a feed.** It can attend to changes in research, tools, project ecosystems, industry, and culture without turning your attention into an endless stream to consume.
- **Natural conversation, not forced turn-taking.** A message can be stopped, retracted, edited, and resent from that point. The model may also remain silent when there is no meaningful addition. A manual exploration that finds nothing notable reports that outcome clearly instead of appearing stuck.
- **Memory is reviewable work, not automatic filing.** A temporary candidate pool is not memory. Only information that survives stronger judgment with sources and alternatives enters durable context. Topics and hypotheses are revisited, aged, or retired over time.
- **Continuity over perfect clustering.** Topics may overlap, be corrected, and converge gradually. The system does not try to split every sentence into infinite components or promise perfectly correct associations.
- **The person keeps the boundary.** Permissions, exploration pace, interruption limits, and visible working state remain inspectable and adjustable. Background behavior is intentionally restrained.

### How the pieces work together

```text
outside world / your conversation / selected Codex context
                          ↓
                    symbiont-d
  sensing · discussion · current map · open loops · reflection
                          ↓
       Pages, sources, and relations worth retaining
                          ↓
                     PCP Runtime
```

- **With Codex:** Symbiont-d can attach a selected Codex conversation as bounded, read-only context for one turn, or export a context packet for you to bring into Codex. It does not dispatch, take over, or impersonate Codex tasks.
- **With PCP:** Symbiont-d is the Host. It decides when memory is written, reviewed, revised, or retracted. The [Paged Context Protocol (PCP)](https://github.com/glenzli/paged-context-protocol) Runtime provides durable Pages, immutable Revisions, provenance, relations, and retrieval.
- **With local data:** Data stays local by default. PCP Console is a separate read-only observation surface; Symbiont-d presents only the working state relevant to the current collaboration.

### Run it

This early prototype needs:

- Rust 1.88+;
- a logged-in Codex CLI;
- a sibling checkout of the [Paged Context Protocol repository](https://github.com/glenzli/paged-context-protocol) and its PCP Runtime.

For local development:

```bash
cargo run
```

Then open <http://127.0.0.1:4317>.

To keep it running as a local service:

```bash
./scripts/service-install.sh
```

The installer configures an identity-bound PCP Runtime, a read-only PCP Console, and symbiont-d. Re-run it to update local services; uninstalling preserves local data.

### Status

Symbiont-d is an early prototype being calibrated through real long-term use. It should prefer missing a weak signal over presenting noise, speculation, or invisible internal work as something worth interrupting you for.

<p align="right"><a href="#zh">中文 ↑</a></p>
