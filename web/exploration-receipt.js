export function manualCompletionSince(exploration, requestId) {
  const run = exploration?.manualRun;
  if (
    !requestId ||
    run?.id !== requestId ||
    run.presentedAt ||
    !["messaged", "silent", "failed"].includes(run.status) ||
    !run.completedAt
  ) {
    return null;
  }
  return completionFrom(run);
}

export function unpresentedManualCompletions(exploration) {
  const seen = new Set();
  const completions = [];
  for (const run of exploration?.manualReceipts || []) {
    if (
      !run?.id ||
      seen.has(run.id) ||
      run.presentedAt ||
      !["messaged", "silent", "failed"].includes(run.status) ||
      !run.completedAt
    ) {
      continue;
    }
    seen.add(run.id);
    completions.push(completionFrom(run));
  }
  return completions;
}

export function manualRunPending(exploration) {
  return ["queued", "exploring"].includes(exploration?.manualRun?.status);
}

export function manualRunLabel(run) {
  if (run?.status === "exploring") return "正在进行手动探索";
  return (
    {
      codex_busy: "等待后台工作完成后继续探索",
      conversation_active: "等待当前对话结束后继续探索",
      newer_user_input: "已优先处理新消息，稍后继续探索",
      background_interrupted: "探索暂时让路，稍后继续",
    }[run?.reason] || "手动探索已加入队列"
  );
}

export function manualCompletionNotice(receipt) {
  const notExecuted =
    receipt?.outcome === "failed" ||
    ["no_input_channel", "input_cooldown", "mailbox_empty", "channel_failed"].includes(
      receipt?.outcome,
    );
  const message =
    {
      no_input_channel: "没有已配置的广域输入通道，因此没有开始实际探索。",
      input_cooldown: "广域输入通道已启用，正在等待其下一次观察时间。",
      mailbox_empty: "已查收私有研究收件箱，但没有新的白名单输入。",
      channel_failed: "广域输入通道未能完成，本次没有产生可查看的探索内容。",
      failed: "运行中出现异常，可以稍后重新探索。",
      input_signals_broadcast:
        "已带回新的广域输入，可以直接回复；它们仍保持外部输入身份。",
      input_signals_published:
        "已带回新的广域输入，可以直接回复；它们仍保持外部输入身份。",
    }[receipt?.outcome] ||
    (receipt?.outcome?.startsWith("messaged")
      ? "已带回一条值得讨论的情报。"
      : "本次没有发现值得打扰你的新情报。");
  return {
    label: notExecuted ? "探索未实际执行" : "探索完成",
    message,
  };
}

function completionFrom(run) {
  return {
    id: run.id,
    completedAt: run.completedAt,
    outcome: run.outcome || run.status,
    reason: run.reason || null,
    resultRevisionId: run.resultRevisionId || null,
  };
}
