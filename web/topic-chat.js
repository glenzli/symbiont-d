export function messageTopicId(message) {
  return (message?.parts || []).find(
    (part) => part.type === "topic" && part.topic?.topicId,
  )?.topic?.topicId || null;
}

export function messageBelongsToTopic(message, topicId) {
  return Boolean(topicId) && messageTopicId(message) === topicId;
}

export function topicChatMessageKey(message, fallbackIndex = 0) {
  if (message?.revisionId) return message.revisionId;
  return [
    message?.role || "unknown",
    message?.at || "unknown",
    fallbackIndex,
    String(message?.content || "").slice(0, 80),
  ].join(":");
}
