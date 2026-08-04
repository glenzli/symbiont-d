export function manualCompletionSince(exploration, priorRunAt) {
  const runAt = exploration?.lastRunAt;
  if (
    !runAt ||
    runAt === priorRunAt ||
    exploration.phase === "exploring" ||
    exploration.lastTrigger !== "manual"
  ) {
    return null;
  }
  return {
    id: runAt,
    completedAt: runAt,
    outcome: exploration.lastOutcome || "unknown",
  };
}
