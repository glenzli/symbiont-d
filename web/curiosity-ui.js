import { formatDate } from "/presentation.js";

const STATE_LABELS = {
  germinating: "萌发",
  watching: "观察中",
  dormant: "休眠",
  resolved: "已解决",
};

const ORIGIN_LABELS = {
  user: "用户明确提出",
  symbiont: "symbiont 推演",
  external: "外部信息触发",
};

const ATTENTION_LABELS = {
  ready: "可探索",
  awaiting_user: "等待回应",
  feedback_pending: "正在吸收回复",
  cooldown: "已纳入，暂缓",
};

export function initCuriosityUi() {
  const root = document.querySelector("#archive-curiosity");

  function render(snapshot) {
    root.replaceChildren();
    const header = document.createElement("header");
    header.className = "curiosity-heading";
    const heading = document.createElement("div");
    const title = document.createElement("h3");
    const description = document.createElement("p");
    const count = document.createElement("span");
    title.textContent = "Curiosity Map";
    description.textContent =
      "symbiont-d 自己仍在追的问题，不等同于你的兴趣或画像";
    count.textContent = `${snapshot?.activeCount || 0} 个活跃`;
    heading.append(title, description);
    header.append(heading, count);
    root.append(header);

    const hunches = snapshot?.hunches || [];
    if (!hunches.length) {
      const empty = document.createElement("p");
      empty.className = "archive-empty";
      empty.textContent = "还没有形成需要跨对话继续追踪的问题。";
      root.append(empty);
      return;
    }

    const list = document.createElement("div");
    list.className = "hunch-list";
    for (const hunch of hunches) {
      const details = document.createElement("details");
      details.className = `hunch-item hunch-${hunch.state}`;
      const summary = document.createElement("summary");
      const question = document.createElement("strong");
      const state = document.createElement("span");
      question.textContent = hunch.question;
      const semanticState = STATE_LABELS[hunch.state] || hunch.state;
      const attentionState =
        ATTENTION_LABELS[hunch.attention] || hunch.attention;
      state.textContent =
        hunch.attention && hunch.attention !== "ready"
          ? `${semanticState} · ${attentionState}`
          : semanticState;
      summary.append(question, state);
      details.append(summary);

      const body = document.createElement("div");
      body.className = "hunch-body";
      appendField(body, "为什么还活着", hunch.whyAlive);
      appendField(body, "什么会改变它", hunch.whatWouldChangeIt);
      if (hunch.resolution) {
        appendField(body, "结论", hunch.resolution);
      }
      if (hunch.attention === "cooldown" && hunch.feedbackAssessment) {
        appendField(body, "最近反馈处理", hunch.feedbackAssessment);
      }
      const metadata = document.createElement("dl");
      metadata.className = "hunch-metadata";
      appendDefinition(
        metadata,
        "来源",
        ORIGIN_LABELS[hunch.origin] || hunch.origin,
      );
      appendDefinition(metadata, "最近修订", formatDate(hunch.updatedAt));
      appendDefinition(
        metadata,
        "最近主动探索",
        hunch.lastExploredAt ? formatDate(hunch.lastExploredAt) : "尚未",
      );
      appendDefinition(
        metadata,
        "交互状态",
        ATTENTION_LABELS[hunch.attention] || hunch.attention || "可探索",
      );
      if (hunch.eligibleAfter) {
        appendDefinition(
          metadata,
          "可再次探索",
          formatDate(hunch.eligibleAfter),
        );
      }
      if (hunch.lastFeedbackRevisionId) {
        appendDefinition(metadata, "最近反馈", hunch.lastFeedbackRevisionId);
      }
      appendDefinition(
        metadata,
        "依据",
        hunch.sourceRevisionIds?.length
          ? `${hunch.sourceRevisionIds.length} 个 Revision`
          : "未附加",
      );
      appendDefinition(metadata, "Page", hunch.pageId);
      appendDefinition(metadata, "Revision", hunch.revisionId);
      body.append(metadata);
      details.append(body);
      list.append(details);
    }
    root.append(list);
  }

  return { render };
}

function appendField(parent, label, value) {
  const section = document.createElement("section");
  const heading = document.createElement("h4");
  const content = document.createElement("p");
  heading.textContent = label;
  content.textContent = value;
  section.append(heading, content);
  parent.append(section);
}

function appendDefinition(parent, term, detail) {
  const dt = document.createElement("dt");
  const dd = document.createElement("dd");
  dt.textContent = term;
  dd.textContent = detail;
  parent.append(dt, dd);
}
