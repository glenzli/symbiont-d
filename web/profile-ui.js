import { formatDate, formatMemorySize, responseJson } from "/presentation.js";
import { renderMessageContent, renderRichText } from "/rich-text.js";
import { initCuriosityUi } from "/curiosity-ui.js";

export function initProfileUi(state, sendMessage) {
  const onboarding = document.querySelector("#onboarding");
  const onboardingChoices = document.querySelector("#onboarding-choices");
  const descriptionForm = document.querySelector("#description-form");
  const initialDescription = document.querySelector("#initial-description");
  const onboardingState = document.querySelector("#onboarding-state");
  const guidedButton = document.querySelector("#start-guided");
  const composer = document.querySelector("#composer");
  const emptyState = document.querySelector("#empty-state");
  const archiveDialog = document.querySelector("#archive-dialog");
  const archiveMemory = document.querySelector("#archive-memory");
  const archivePages = document.querySelector("#archive-pages");
  const archiveInfo = document.querySelector("#archive-info");
  const orientationForm = document.querySelector("#orientation-form");
  const orientationText = document.querySelector("#orientation-text");
  const saveOrientation = document.querySelector("#save-orientation");
  const orientationSaveState = document.querySelector(
    "#orientation-save-state",
  );
  const contextForms = [
    ...archiveDialog.querySelectorAll("[data-context-kind]"),
  ];
  const curiosityUi = initCuriosityUi();
  const profileReviewStatus = document.querySelector(
    "#profile-review-status",
  );
  const profileReviewContent = document.querySelector(
    "#profile-review-content",
  );
  const tabButtons = [...archiveDialog.querySelectorAll("[data-archive-tab]")];
  const tabPanels = [
    ...archiveDialog.querySelectorAll("[data-archive-panel]"),
  ];
  let archivePayload = null;
  let starting = false;

  function render() {
    const unconfigured = state.profile.status === "unconfigured";
    onboarding.hidden = !unconfigured;
    composer.hidden = unconfigured;
    if (unconfigured) {
      emptyState.hidden = true;
      onboardingChoices.hidden = false;
      descriptionForm.hidden = true;
    } else {
      emptyState.hidden = Boolean(document.querySelector(".message"));
      const placeholder =
        state.profile.status === "calibrating"
          ? "继续这段认识彼此的对话…"
          : "说点什么…";
      document.querySelector("#message").placeholder = placeholder;
    }
  }

  async function begin(mode, firstMessage) {
    if (starting) return;
    starting = true;
    onboardingState.textContent = "正在开始";
    guidedButton.disabled = true;
    try {
      state.profile = await responseJson(
        await fetch("/api/onboarding/start", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ mode }),
        }),
        "无法开始初始化",
      );
      onboardingState.textContent = "";
      render();
      await sendMessage(firstMessage);
    } catch (error) {
      onboardingState.textContent = error.message;
    } finally {
      guidedButton.disabled = false;
      starting = false;
    }
  }

  async function loadArchive() {
    archiveMemory.textContent = "正在读取";
    try {
      archivePayload = await responseJson(
        await fetch("/api/archive"),
        "无法读取档案",
      );
      state.profile = archivePayload.profile;
      state.autonomyPermitted = archivePayload.autonomyPermitted;
      renderArchive();
    } catch (error) {
      archiveMemory.textContent = error.message;
    }
  }

  function renderArchive() {
    if (!archivePayload) return;
    const profile = archivePayload.profile;
    const ready = profile.status === "ready";
    orientationText.value = profile.orientation || "";
    orientationText.disabled = !ready;
    saveOrientation.disabled = !ready;
    orientationText.placeholder = ready
      ? ""
      : profile.status === "calibrating"
        ? "完成初始化对话后，画像会显示在这里。"
        : "尚未开始初始化。";
    renderMemory(archivePayload.memory);
    renderContext(archivePayload.context);
    curiosityUi.render(archivePayload.curiosity);
    renderPages(archivePayload.pcp);
    renderInfo(archivePayload);
  }

  function renderContext(context) {
    const documents = {
      "current-map": context?.currentMap,
      "open-loops": context?.openLoops,
    };
    for (const [kind, document] of Object.entries(documents)) {
      const textarea = archiveDialog.querySelector(
        `[data-context-text="${kind}"]`,
      );
      const updated = archiveDialog.querySelector(
        `[data-context-updated="${kind}"]`,
      );
      textarea.value = document?.content || "";
      updated.textContent = document?.updatedAt
        ? formatDate(document.updatedAt)
        : "尚未整理";
    }

    const review = context?.profileReview;
    const status = review?.facets?.reviewStatus;
    profileReviewStatus.textContent = review
      ? `${profileReviewStatusText(status)} · ${formatDate(review.updatedAt)}`
      : "尚未审查";
    profileReviewContent.replaceChildren();
    if (review?.content) {
      renderRichText(profileReviewContent, review.content);
    } else {
      const empty = document.createElement("p");
      empty.className = "archive-empty";
      empty.textContent = "长期画像还没有进入后台审查周期。";
      profileReviewContent.append(empty);
    }
  }

  function renderPages(pcp) {
    archivePages.replaceChildren();
    if (!pcp.pages.length) {
      const empty = document.createElement("p");
      empty.className = "archive-empty";
      empty.textContent = "还没有 PCP Page。";
      archivePages.append(empty);
      return;
    }
    const scopeByNamespace = new Map(
      pcp.scopes.map((scope) => [scope.namespace, scope]),
    );
    for (const page of pcp.pages) {
      const revision = page.revision;
      const imageAsset = imageAssetFromRevision(revision);
      const details = document.createElement("details");
      details.className = "pcp-page";
      const summary = document.createElement("summary");
      const heading = document.createElement("span");
      const title = document.createElement("strong");
      const namespace = document.createElement("small");
      const id = document.createElement("code");
      title.textContent =
        revision.facets?.kind === "user_orientation"
          ? "用户画像"
          : revision.facets?.kind === "image_asset"
            ? "图片资产"
          : revision.facets?.role === "user"
            ? "用户消息"
            : revision.facets?.role === "assistant"
              ? "symbiont 回复"
              : revision.facets?.kind || "Context Page";
      namespace.textContent =
        scopeByNamespace.get(revision.namespace)?.displayName ||
        revision.namespace;
      id.textContent = shortId(revision.pageId);
      heading.append(title, namespace);
      summary.append(heading, id);
      details.append(summary);

      const body = document.createElement("div");
      body.className = "pcp-page-body";
      if (imageAsset?.url) {
        const content = document.createElement("figure");
        content.className = "archive-image";
        const image = document.createElement("img");
        image.src = imageAsset.url;
        image.alt = imageAsset.filename || "Image asset";
        const caption = document.createElement("figcaption");
        caption.textContent = imageAsset.filename || "Image";
        content.append(image, caption);
        body.append(content);
      } else if (revision.payload?.content) {
        const content = document.createElement("div");
        content.className = "pcp-rich-content";
        renderRichText(content, revision.payload.content);
        body.append(content);
      }
      const metadata = document.createElement("dl");
      metadata.className = "page-metadata";
      appendDefinition(metadata, "Page", revision.pageId);
      appendDefinition(metadata, "Revision", revision.revisionId);
      appendDefinition(metadata, "Scope", revision.namespace);
      appendDefinition(metadata, "状态", revision.lifecycleStatus);
      appendDefinition(
        metadata,
        "观察时间",
        formatDate(revision.observedAt || revision.createdAt),
      );
      appendDefinition(
        metadata,
        "来源",
        revision.sourceRefs?.length
          ? revision.sourceRefs
              .map((source) => source.locator ? `${source.uri} · ${source.locator}` : source.uri)
              .join("\n")
          : "无外部来源",
      );
      appendDefinition(
        metadata,
        "生成",
        formatProvenance(revision.provenance),
      );
      appendDefinition(
        metadata,
        "关系",
        page.relations.length
          ? page.relations.map((relation) => relation.relationType).join(", ")
          : "无",
      );
      appendDefinition(metadata, "修订", `${page.history.length} 个版本`);
      body.append(metadata);
      details.append(body);
      archivePages.append(details);
    }
  }

  function renderMemory(entries) {
    archiveMemory.replaceChildren();
    if (!entries.length) {
      const empty = document.createElement("p");
      empty.className = "archive-empty";
      empty.textContent = "还没有本地记忆。";
      archiveMemory.append(empty);
      return;
    }
    for (const entry of [...entries].reverse()) {
      const article = document.createElement("article");
      article.className = "memory-entry";
      const header = document.createElement("header");
      const role = document.createElement("strong");
      const time = document.createElement("time");
      const body = document.createElement("div");
      body.className = "memory-entry-body";
      role.textContent =
        entry.role === "user"
          ? "你"
          : entry.role === "memory"
            ? "明确记忆"
            : "symbiont-d";
      time.textContent = formatDate(entry.at);
      renderMessageContent(body, entry);
      header.append(role, time);
      article.append(header, body);
      archiveMemory.append(article);
    }
  }

  function renderInfo(payload) {
    const profile = payload.profile;
    const rows = [
      ["初始化", statusText(profile.status)],
      ["方式", modeText(profile.mode)],
      ["最近更新", formatDate(profile.updatedAt)],
      ["本地记忆", formatMemorySize(payload.memoryChars)],
      ["PCP Pages", String(payload.pcp.pageCount)],
      ["可见 Scope", String(payload.pcp.scopes.length)],
      [
        "自主探索许可",
        payload.autonomyPermitted ? "已允许" : "未允许",
      ],
      ["外部任务访问", "未启用"],
    ];
    archiveInfo.replaceChildren();
    for (const [term, detail] of rows) {
      const dt = document.createElement("dt");
      const dd = document.createElement("dd");
      dt.textContent = term;
      dd.textContent = detail;
      archiveInfo.append(dt, dd);
    }
  }

  function activateTab(name) {
    for (const button of tabButtons) {
      button.setAttribute(
        "aria-selected",
        String(button.dataset.archiveTab === name),
      );
    }
    for (const panel of tabPanels) {
      panel.hidden = panel.dataset.archivePanel !== name;
    }
  }

  document
    .querySelector("#choose-description")
    .addEventListener("click", () => {
      onboardingChoices.hidden = true;
      descriptionForm.hidden = false;
      initialDescription.focus();
    });
  document
    .querySelector("#cancel-description")
    .addEventListener("click", () => {
      descriptionForm.hidden = true;
      onboardingChoices.hidden = false;
      onboardingState.textContent = "";
    });
  guidedButton.addEventListener("click", () =>
    begin("guided", "我们从认识我开始吧。请问我第一个问题。"),
  );
  descriptionForm.addEventListener("submit", (event) => {
    event.preventDefault();
    const description = initialDescription.value.trim();
    if (description) begin("description", description);
  });
  document.querySelector("#open-archive").addEventListener("click", () => {
    orientationSaveState.textContent = "";
    activateTab("orientation");
    archiveDialog.showModal();
    loadArchive();
  });
  for (const button of tabButtons) {
    button.addEventListener("click", () =>
      activateTab(button.dataset.archiveTab),
    );
  }
  orientationForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    orientationSaveState.textContent = "保存中";
    try {
      state.profile = await responseJson(
        await fetch("/api/profile/orientation", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ orientation: orientationText.value }),
        }),
        "保存失败",
      );
      archivePayload.profile = state.profile;
      orientationSaveState.textContent = "已保存";
      renderArchive();
    } catch (error) {
      orientationSaveState.textContent = error.message;
    }
  });
  for (const form of contextForms) {
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const kind = form.dataset.contextKind;
      const textarea = form.querySelector(`[data-context-text="${kind}"]`);
      const saveState = form.querySelector(
        `[data-context-save-state="${kind}"]`,
      );
      saveState.textContent = "保存中";
      try {
        archivePayload.context = await responseJson(
          await fetch(`/api/context/${kind}`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ content: textarea.value }),
          }),
          "保存失败",
        );
        saveState.textContent = "已保存";
        renderContext(archivePayload.context);
      } catch (error) {
        saveState.textContent = error.message;
      }
    });
  }

  return { render };
}

function profileReviewStatusText(status) {
  return (
    {
      no_change: "暂不调整",
      clarification: "等待确认",
      proposal: "有修订建议",
    }[status] || "已审查"
  );
}

function appendDefinition(parent, term, detail) {
  const dt = document.createElement("dt");
  const dd = document.createElement("dd");
  dt.textContent = term;
  dd.textContent = detail;
  parent.append(dt, dd);
}

function imageAssetFromRevision(revision) {
  if (revision.facets?.kind !== "image_asset") return null;
  try {
    return JSON.parse(revision.payload?.content || "null");
  } catch {
    return null;
  }
}

function formatProvenance(events) {
  if (!events?.length) return "未读取";
  return events
    .map((event) => {
      const producer =
        event.toolOrModel ||
        event.actor?.actorId ||
        event.actor?.actorType ||
        "unknown";
      const inputCount = event.inputRevisionIds?.length || 0;
      return `${event.operation} · ${producer} · ${inputCount} 个输入`;
    })
    .join("\n");
}

function shortId(value) {
  if (!value || value.length < 20) return value || "";
  return `${value.slice(0, 11)}…${value.slice(-6)}`;
}

function statusText(status) {
  return (
    {
      unconfigured: "尚未开始",
      calibrating: "正在认识",
      ready: "已完成",
    }[status] || status
  );
}

function modeText(mode) {
  return (
    {
      description: "初始描述",
      guided: "引导对话",
    }[mode] || "尚无"
  );
}
