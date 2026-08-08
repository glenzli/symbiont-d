export function dispositionState(disposition) {
  return String(disposition?.reaction || "").trim() ? "reacted" : "settled";
}

export function initTurnDispositionUi({
  conversation,
  messageActions,
  appendEphemeralReaction,
  notify,
}) {
  function apply(disposition, options = {}) {
    const revisionId = disposition?.revisionId;
    if (!revisionId) return false;
    const message = conversation.querySelector(
      `.message[data-role="user"][data-revision-id="${CSS.escape(revisionId)}"]`,
    );
    if (!message) return false;

    const reaction = String(disposition.reaction || "").trim();
    if (reaction) appendEphemeralReaction?.({ revisionId, reaction });
    messageActions.update(message, null, {
      deliveryState: dispositionState(disposition),
    });
    if (options.announce) {
      notify(
        reaction
          ? `symbiont-d 用 ${reaction} 回应了`
          : "已读 · 对话在这里自然收束",
      );
    }
    return true;
  }

  function applyAll(dispositions) {
    for (const disposition of dispositions || []) apply(disposition);
  }

  return { apply, applyAll };
}
