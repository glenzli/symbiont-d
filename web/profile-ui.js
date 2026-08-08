import { formatDate, formatMemorySize, responseJson } from "/presentation.js";
import { renderRichText } from "/rich-text.js";
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
  const archiveLoadState = document.querySelector("#archive-load-state");
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
    archiveLoadState.textContent = "正在读取工作状态";
    try {
      archivePayload = await responseJson(
        await fetch("/api/archive"),
        "无法读取工作状态",
      );
      state.profile = archivePayload.profile;
      state.autonomyPermitted = archivePayload.autonomyPermitted;
      renderArchive();
      archiveLoadState.textContent = "";
    } catch (error) {
      archiveLoadState.textContent = error.message;
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
    renderContext(archivePayload.context);
    curiosityUi.render(archivePayload.curiosity);
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

  function renderInfo(payload) {
    const profile = payload.profile;
    const rows = [
      ["初始化", statusText(profile.status)],
      ["方式", modeText(profile.mode)],
      ["最近更新", formatDate(profile.updatedAt)],
      ["本地上下文", formatMemorySize(payload.memoryChars)],
      ["持久记忆", "请在 PCP Console 查看"],
      [
        "主动探索许可",
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
