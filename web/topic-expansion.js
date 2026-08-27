export function createTopicExpansion() {
  return { allExpanded: true, expanded: new Set(), collapsed: new Set() };
}

export function topicMessageKey(message, index = 0) {
  return (
    message.revisionId ||
    `${message.role || "message"}:${message.at || ""}:${index}:${String(message.content || "").slice(0, 48)}`
  );
}

export function isMessageExpanded(expansion, key) {
  if (expansion.expanded.has(key)) return true;
  if (expansion.collapsed.has(key)) return false;
  return expansion.allExpanded;
}

export function setMessageExpanded(expansion, key, expanded) {
  expansion.expanded.delete(key);
  expansion.collapsed.delete(key);
  if (expanded === expansion.allExpanded) return;
  (expanded ? expansion.expanded : expansion.collapsed).add(key);
}
