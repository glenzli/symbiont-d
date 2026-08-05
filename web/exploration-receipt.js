export function manualCompletionSince(exploration, requestId) {
  const run = exploration?.manualRun;
  if (
    !requestId ||
    run?.id !== requestId ||
    !["messaged", "silent", "failed"].includes(run.status) ||
    !run.completedAt
  ) {
    return null;
  }
  return {
    id: run.id,
    completedAt: run.completedAt,
    outcome: run.outcome || run.status,
    reason: run.reason || null,
    resultRevisionId: run.resultRevisionId || null,
  };
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
