const MAX_QUOTES = 6;
const MAX_QUOTE_CHARS = 6000;

export function initQuoteUi({
  conversation,
  entryFor,
  focusComposer,
  notify,
}) {
  const tray = document.querySelector("#quote-tray");
  const selectionButton = document.createElement("button");
  selectionButton.type = "button";
  selectionButton.className = "quote-selection-button";
  selectionButton.textContent = "引用所选";
  selectionButton.hidden = true;
  document.body.append(selectionButton);

  let quotes = [];
  let pendingSelection = null;

  conversation.addEventListener("mouseup", () => {
    window.setTimeout(() => showSelectionAction(), 0);
  });
  conversation.addEventListener("contextmenu", (event) => {
    const draft = quoteFromSelection();
    if (!draft) return;
    event.preventDefault();
    pendingSelection = draft;
    positionSelectionButton(event.clientX, event.clientY);
  });
  conversation.addEventListener("click", (event) => {
    const quote = event.target.closest(".message-quote");
    if (!quote) return;
    jumpToSource(quote.dataset.sourceRevisionId, quote);
  });
  conversation.addEventListener("scroll", hideSelectionAction, { passive: true });
  window.addEventListener("resize", hideSelectionAction);
  document.addEventListener("mousedown", (event) => {
    if (!selectionButton.contains(event.target)) hideSelectionAction();
  });

  selectionButton.addEventListener("mousedown", (event) => event.preventDefault());
  selectionButton.addEventListener("click", () => {
    if (pendingSelection) add(pendingSelection);
    window.getSelection()?.removeAllRanges();
    hideSelectionAction();
    focusComposer();
  });

  tray.addEventListener("click", (event) => {
    const remove = event.target.closest("[data-remove-quote]");
    if (!remove) return;
    quotes.splice(Number(remove.dataset.removeQuote), 1);
    render();
    focusComposer();
  });

  function addWhole(entry) {
    if (!entry?.revisionId) return;
    const source = String(entry.content || "").trim();
    const text = source
      ? truncate(source, MAX_QUOTE_CHARS)
      : entry.parts?.some((part) => part.type === "image")
        ? "[图片消息]"
        : "[空消息]";
    add({
      sourceRevisionId: entry.revisionId,
      selectedText: text,
      sourceRole: entry.role,
      sourceAt: entry.at,
      startOffset: null,
      endOffset: null,
      wholeMessage: true,
    });
    focusComposer();
  }

  function add(draft) {
    if (quotes.length >= MAX_QUOTES) {
      notify(`每条消息最多引用 ${MAX_QUOTES} 段内容`);
      return;
    }
    const normalized = normalizeQuote(draft);
    if (!normalized?.sourceRevisionId || !normalized.text) return;
    const duplicate = quotes.some(
      (quote) =>
        quote.sourceRevisionId === normalized.sourceRevisionId &&
        quote.text === normalized.text &&
        quote.startOffset === normalized.startOffset &&
        quote.endOffset === normalized.endOffset,
    );
    if (duplicate) return;
    quotes.push(normalized);
    render();
  }

  function set(nextQuotes) {
    quotes = (nextQuotes || [])
      .map(normalizeQuote)
      .filter(Boolean)
      .slice(0, MAX_QUOTES);
    render();
  }

  function clear() {
    quotes = [];
    render();
  }

  function drafts() {
    return quotes.map(quoteDraft).filter(Boolean);
  }

  function parts() {
    return quotes.map((quote) => ({
      type: "quote",
      quote: {
        sourceRevisionId: quote.sourceRevisionId,
        sourceRole: quote.sourceRole,
        sourceAt: quote.sourceAt,
        text: quote.text,
        sourceSha256: quote.sourceSha256 || "",
        startOffset: quote.startOffset,
        endOffset: quote.endOffset,
        wholeMessage: quote.wholeMessage,
        truncated: quote.truncated,
      },
    }));
  }

  function render() {
    tray.replaceChildren();
    tray.hidden = quotes.length === 0;
    for (const [index, quote] of quotes.entries()) {
      const item = document.createElement("div");
      item.className = "composer-quote";
      const content = document.createElement("div");
      const meta = document.createElement("span");
      meta.textContent = `${roleLabel(quote.sourceRole)} · ${formatTime(quote.sourceAt)}`;
      const excerpt = document.createElement("p");
      excerpt.textContent = quote.text;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.dataset.removeQuote = String(index);
      remove.title = "移除引用";
      remove.setAttribute("aria-label", "移除引用");
      remove.textContent = "×";
      content.append(meta, excerpt);
      item.append(content, remove);
      tray.append(item);
    }
  }

  function showSelectionAction() {
    const draft = quoteFromSelection();
    if (!draft) {
      hideSelectionAction();
      return;
    }
    pendingSelection = draft;
    const rect = window.getSelection().getRangeAt(0).getBoundingClientRect();
    positionSelectionButton(rect.right, rect.bottom + 6);
  }

  function quoteFromSelection() {
    const selection = window.getSelection();
    if (!selection || selection.isCollapsed || selection.rangeCount !== 1) return null;
    const range = selection.getRangeAt(0);
    const startElement =
      range.startContainer.nodeType === Node.ELEMENT_NODE
        ? range.startContainer
        : range.startContainer.parentElement;
    const endElement =
      range.endContainer.nodeType === Node.ELEMENT_NODE
        ? range.endContainer
        : range.endContainer.parentElement;
    const message = startElement?.closest(".message");
    const body = message?.querySelector(".message-body");
    if (
      !message ||
      !body ||
      !body.contains(range.startContainer) ||
      !body.contains(range.endContainer) ||
      endElement?.closest(".message") !== message
    ) {
      return null;
    }
    const entry = entryFor(message);
    if (!entry?.revisionId) return null;
    const raw = selection.toString();
    const selectedText = raw.trim();
    if (!selectedText || selectedText.length > MAX_QUOTE_CHARS) {
      if (selectedText.length > MAX_QUOTE_CHARS) {
        notify(`单段引用不能超过 ${MAX_QUOTE_CHARS} 个字符`);
      }
      return null;
    }
    const prefix = range.cloneRange();
    prefix.selectNodeContents(body);
    prefix.setEnd(range.startContainer, range.startOffset);
    const leadingWhitespace = raw.length - raw.trimStart().length;
    const startOffset = prefix.toString().length + leadingWhitespace;
    return {
      sourceRevisionId: entry.revisionId,
      selectedText,
      sourceRole: entry.role,
      sourceAt: entry.at,
      startOffset,
      endOffset: startOffset + selectedText.length,
      wholeMessage: false,
    };
  }

  function positionSelectionButton(x, y) {
    selectionButton.hidden = false;
    const width = selectionButton.offsetWidth;
    const height = selectionButton.offsetHeight;
    selectionButton.style.left = `${Math.max(8, Math.min(x - width, window.innerWidth - width - 8))}px`;
    selectionButton.style.top = `${Math.max(8, Math.min(y, window.innerHeight - height - 8))}px`;
  }

  function hideSelectionAction() {
    selectionButton.hidden = true;
    pendingSelection = null;
  }

  function jumpToSource(revisionId, quote) {
    const source = [...conversation.querySelectorAll(".message")].find(
      (message) => message.dataset.revisionId === revisionId,
    );
    if (!source) {
      quote.classList.add("source-missing");
      quote.title = "原消息不在当前对话窗口，引用快照仍然保留";
      notify("原消息不在当前对话窗口，引用快照仍然保留");
      return;
    }
    source.scrollIntoView({ behavior: "smooth", block: "center" });
    source.classList.remove("quote-source-highlight");
    window.requestAnimationFrame(() => source.classList.add("quote-source-highlight"));
    window.setTimeout(() => source.classList.remove("quote-source-highlight"), 1600);
  }

  return {
    addWhole,
    clear,
    drafts,
    parts,
    set,
  };
}

function normalizeQuote(value) {
  const quote = value?.quote || value;
  if (!quote) return null;
  const text = String(quote.text ?? quote.selectedText ?? "").trim();
  if (!text) return null;
  return {
    sourceRevisionId: quote.sourceRevisionId,
    sourceRole: quote.sourceRole || "assistant",
    sourceAt: quote.sourceAt || new Date().toISOString(),
    text: truncate(text, MAX_QUOTE_CHARS),
    sourceSha256: quote.sourceSha256 || "",
    startOffset: quote.startOffset ?? null,
    endOffset: quote.endOffset ?? null,
    wholeMessage: quote.wholeMessage === true,
    truncated: quote.truncated === true || text.length > MAX_QUOTE_CHARS,
  };
}

export function quoteDraft(value) {
  const quote = normalizeQuote(value);
  if (!quote?.sourceRevisionId || !quote.text) return null;
  return {
    sourceRevisionId: quote.sourceRevisionId,
    selectedText: quote.text,
    startOffset: quote.startOffset,
    endOffset: quote.endOffset,
    wholeMessage: quote.wholeMessage,
  };
}

function roleLabel(role) {
  return role === "user" ? "你" : "symbiont-d";
}

function formatTime(value) {
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function truncate(value, maxChars) {
  return value.length <= maxChars ? value : `${value.slice(0, maxChars - 1)}…`;
}
