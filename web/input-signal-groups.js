const DEFAULT_MAX_GAP_MS = 20 * 60 * 1000;
const MIN_GROUP_SIZE = 2;

export function inputSignalGroupRuns(items, maxGapMs = DEFAULT_MAX_GAP_MS) {
  const runs = [];
  let start = 0;
  while (start < items.length) {
    const first = items[start];
    if (!first.isSignal) {
      start += 1;
      continue;
    }
    let end = start + 1;
    let previousAt = Date.parse(first.observedAt);
    while (end < items.length) {
      const current = items[end];
      const currentAt = Date.parse(current.observedAt);
      if (
        !current.isSignal ||
        current.roleId !== first.roleId ||
        !Number.isFinite(previousAt) ||
        !Number.isFinite(currentAt) ||
        currentAt - previousAt > maxGapMs
      ) {
        break;
      }
      previousAt = currentAt;
      end += 1;
    }
    if (end - start >= MIN_GROUP_SIZE) runs.push({ start, end });
    start = end;
  }
  return runs;
}

function unwrapGroups(container) {
  for (const group of container.querySelectorAll(":scope > .input-signal-group")) {
    const items = group.querySelector(":scope > .input-signal-group-items");
    group.replaceWith(...items.children);
  }
}

function groupHeader(messages) {
  const header = document.createElement("header");
  header.className = "input-signal-group-header";
  const avatar = messages[0].querySelector(".message-avatar")?.cloneNode(true);
  if (avatar) {
    avatar.classList.add("input-signal-group-avatar");
    avatar.setAttribute("aria-hidden", "true");
    header.append(avatar);
  }
  const summary = document.createElement("div");
  const speaker = messages[0].querySelector(".speaker")?.textContent || "外部输入";
  const title = document.createElement("strong");
  title.textContent = `${speaker} · ${messages.length} 条连续输入`;
  const range = document.createElement("small");
  const firstAt = new Date(messages[0].dataset.signalObservedAt);
  const lastAt = new Date(messages.at(-1).dataset.signalObservedAt);
  range.textContent = `${firstAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}–${lastAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
  summary.append(title, range);

  header.append(summary);
  return header;
}

export function regroupInputSignals(container) {
  if (!container) return;
  unwrapGroups(container);
  const children = [...container.children];
  const items = children.map((element) => ({
    isSignal: element.matches(".input-signal"),
    roleId: `${element.dataset.inputRoleId || ""}:${element.dataset.signalKind || "external_input"}`,
    observedAt: element.dataset.signalObservedAt || "",
  }));
  const runs = inputSignalGroupRuns(items);
  for (const run of [...runs].reverse()) {
    const messages = children.slice(run.start, run.end);
    const group = document.createElement("section");
    group.className = "input-signal-group";
    const header = groupHeader(messages);
    const body = document.createElement("div");
    body.className = "input-signal-group-items";
    group.append(header, body);
    children[run.start].replaceWith(group);
    body.append(...messages);
  }
}
