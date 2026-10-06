// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView from
// IslandViewContent.swift.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { Bridge, onEvent, type ChatContext } from "../core/bridge";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import type { ViewHost } from "./views";

let nextId = 1;

interface ChatStatusPayload {
  status: "searching" | "searched" | "thinking";
  detail: string;
  siteCount?: number;
}

interface ChatTokenPayload {
  token: string;
  done: boolean;
}

function bubble(message: ChatMessage): HTMLElement {
  if (message.role === "user") {
    return h(
      "div",
      { class: "chat-row user" },
      h("div", { class: "bubble", text: message.content }),
    );
  }

  const replyEl = h("div", { class: "reply" });
  if (message.siteCount && message.siteCount > 0) {
    const badge = h(
      "div",
      { class: "chat-grounding-badge" },
      h("span", { class: "globe-icon" }, svg(ICONS.globe, 11)),
      h("span", { text: `Searched ${message.siteCount} websites` }),
    );
    replyEl.append(badge);
  }
  const textEl = h("div", { class: "reply-text", text: message.content });
  replyEl.append(textEl);

  if (message.memoryUpdated) {
    const pill = h(
      "div",
      { class: "chat-memory-pill" },
      h("span", { class: "sparkle-icon" }, svg(ICONS.sparkles, 11)),
      h("span", { text: "Memory updated" }),
    );
    replyEl.append(pill);
  }

  return h("div", { class: "chat-row" }, replyEl);
}

function statusRow(status: ChatStatusPayload): HTMLElement {
  const isThinking = status.status === "thinking";
  const row = h("div", { class: "chat-row status-row" });
  const bubble = h("div", { class: `chat-status-bubble ${isThinking ? "thinking" : ""}` });

  if (!isThinking) {
    const globe = h("span", { class: "chat-status-globe" }, svg(ICONS.globe, 13));
    bubble.append(globe);
  }

  const isPulse = status.status === "searching" || isThinking;
  const text = h("span", {
    class: `chat-status-text ${isPulse ? "status-pulse" : ""}`,
    text: status.detail,
  });
  bubble.append(text);
  row.append(bubble);
  return row;
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string, onRemove?: () => void): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  if (onRemove) {
    const xBtn = h(
      "button",
      {
        class: "chip-close",
        title: "Remove attached file",
        "aria-label": "Remove attachment",
        onclick: (e: Event) => {
          e.stopPropagation();
          onRemove();
        },
      },
      svg(ICONS.xmark, 8),
    );
    chip.append(xBtn);
  }
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const clearBtn = h(
    "button",
    {
      class: "chat-clear-btn",
      title: "Clear chat history (User profile & memory will stay saved)",
      "aria-label": "Clear chat",
    },
    svg(ICONS.trash, 11),
    h("span", { text: "Clear" }),
  );
  const headerRow = h("div", { class: "chat-header-row" }, chipRow, clearBtn);

  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: "Ask me anything…",
    spellcheck: "false",
  }) as HTMLInputElement;
  const send = h(
    "button",
    {
      class: "send-btn",
      title: "Send",
      "aria-label": "Send",
      onmousedown: (e: Event) => {
        // Prevent clicking the button from blurring the text input
        e.preventDefault();
      },
    },
    svg(ICONS.arrowUp, 11),
  );
  const bar = h("div", { class: "chat-bar" }, input, send);

  const el = h(
    "div",
    { class: "view" },
    h("div", { class: "card wash chat-card" }, h("div", { class: "chat-body" }, headerRow, log, bar)),
  );
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  bar.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).closest("button")) return;
    input.focus();
  });

  let sending = false;
  let renderedCount = -1;
  let activeStatus: ChatStatusPayload | null = null;
  let activeStatusEl: HTMLElement | null = null;
  let activeReplyEl: HTMLElement | null = null;
  let activeReplyRow: HTMLElement | null = null;
  let lastSearchedCount = 0;
  let streamedContent = "";

  // Subscribe to live backend status and token events
  void onEvent<ChatStatusPayload>("chat-status", (payload) => {
    if (!sending) return;
    activeStatus = payload;
    if (payload.siteCount && payload.siteCount > 0) {
      lastSearchedCount = payload.siteCount;
    }

    if (!activeReplyEl) {
      if (activeStatusEl && activeStatusEl.parentNode) {
        const newStatusEl = statusRow(payload);
        activeStatusEl.replaceWith(newStatusEl);
        activeStatusEl = newStatusEl;
      } else {
        activeStatusEl = statusRow(payload);
        log.append(activeStatusEl);
      }
      log.scrollTop = log.scrollHeight;
      onHeightChange();
    }
  });

  void onEvent<ChatTokenPayload>("chat-token", (payload) => {
    if (!sending) return;
    if (payload.token) {
      if (!activeReplyEl) {
        if (activeStatusEl && activeStatusEl.parentNode) {
          activeStatusEl.remove();
          activeStatusEl = null;
        }

        activeReplyEl = h("div", { class: "reply" });
        if (lastSearchedCount > 0) {
          const badge = h(
            "div",
            { class: "chat-grounding-badge" },
            h("span", { class: "globe-icon" }, svg(ICONS.globe, 11)),
            h("span", { text: `Searched ${lastSearchedCount} websites` }),
          );
          activeReplyEl.append(badge);
        }

        const textEl = h("div", { class: "reply-text" });
        activeReplyEl.append(textEl);

        activeReplyRow = h("div", { class: "chat-row" }, activeReplyEl);
        log.append(activeReplyRow);
      }

      // Append token smoothly into .reply-text container
      const textContainer = (activeReplyEl.querySelector(".reply-text") as HTMLElement) ?? activeReplyEl;
      const tokenSpan = h("span", { class: "token-fade", text: payload.token });
      textContainer.append(tokenSpan);
      streamedContent += payload.token;
      log.scrollTop = log.scrollHeight;
      onHeightChange();
    }
  });

  let memoryUpdatedInTurn = false;

  void onEvent<{ updated: boolean }>("memory-updated", (payload) => {
    if (!sending) return;
    if (payload.updated) {
      memoryUpdatedInTurn = true;
      if (activeReplyEl && !activeReplyEl.querySelector(".chat-memory-pill")) {
        const pill = h(
          "div",
          { class: "chat-memory-pill" },
          h("span", { class: "sparkle-icon" }, svg(ICONS.sparkles, 11)),
          h("span", { text: "Memory updated" }),
        );
        activeReplyEl.append(pill);
        log.scrollTop = log.scrollHeight;
        onHeightChange();
      }
    }
  });

  async function clearChat() {
    if (sending || State.chatHistory.length === 0) return;
    try {
      await Bridge.chatClear();
    } catch (err) {
      console.error("[coucou] failed to clear chat on disk", err);
    }
    State.chatHistory = [];
    State.droppedFile = null;
    renderedCount = -1;
    clear(log);
    Sound.play("pop");
    State.notify();
    onHeightChange();
    input.placeholder = "Ask me anything…";
    input.focus();
  }

  clearBtn.addEventListener("click", () => void clearChat());

  function updateSendButtonState(isSending: boolean) {
    clear(send);
    if (isSending) {
      send.append(svg(ICONS.stop, 10));
      send.title = "Stop generating";
      send.setAttribute("aria-label", "Stop generating");
      send.classList.add("stopping");
    } else {
      send.append(svg(ICONS.arrowUp, 11));
      send.title = "Send";
      send.setAttribute("aria-label", "Send");
      send.classList.remove("stopping");
    }
  }

  function stopGeneration() {
    if (!sending) return;
    void Bridge.chatStop();
    Sound.play("blip");
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending) return;
    input.value = "";
    sending = true;
    updateSendButtonState(true);
    input.focus();
    Sound.play("send");

    const maxId = State.chatHistory.reduce((max, m) => Math.max(max, m.id), 0);
    if (maxId >= nextId) nextId = maxId + 1;

    const userMsg: ChatMessage = { id: nextId++, role: "user", content: query };
    State.chatHistory.push(userMsg);
    State.stateOverride = "thinking";

    // Append user bubble to DOM immediately
    log.append(bubble(userMsg));

    // Initial status: "Thinking…" (NO 3 dots!)
    activeStatus = { status: "thinking", detail: "Thinking…" };
    lastSearchedCount = 0;
    streamedContent = "";
    memoryUpdatedInTurn = false;
    activeReplyEl = null;
    activeReplyRow = null;
    activeStatusEl = statusRow(activeStatus);
    log.append(activeStatusEl);
    log.scrollTop = log.scrollHeight;
    onHeightChange();

    const file = State.droppedFile;
    const context: ChatContext | null =
      file ? { kind: "file", name: file.name, path: file.path } : null;

    function cleanLiveNodes() {
      (activeStatusEl as HTMLElement | null)?.remove();
      (activeReplyRow as HTMLElement | null)?.remove();
      activeStatusEl = null;
      activeReplyEl = null;
      activeReplyRow = null;
    }

    try {
      const reply = await Bridge.chatSend(query, context);

      // Clean up active temporary live DOM nodes
      cleanLiveNodes();

      const finalContent = (reply.text || streamedContent).trim();
      if (!finalContent) {
        // Stopped before generating text: remove pending user turn to keep conversation clean
        State.chatHistory.pop();
      } else {
        const isMemoryUpdated = Boolean(reply.memoryUpdated || memoryUpdatedInTurn);
        const assistantMsg: ChatMessage = {
          id: nextId++,
          role: "assistant",
          content: finalContent,
          siteCount: lastSearchedCount > 0 ? lastSearchedCount : undefined,
          memoryUpdated: isMemoryUpdated ? true : undefined,
        };
        State.chatHistory.push(assistantMsg);
        State.droppedFile = null;
        Sound.play("finish");
      }
      State.stateOverride = null;
    } catch (err) {
      cleanLiveNodes();
      State.chatHistory.pop();
      input.value = query;
      State.stateOverride = null;
      State.noteMessage = String(err).replace(/^Error:\s*/, "");
      State.view = "note";
      Sound.play("error");
    } finally {
      sending = false;
      updateSendButtonState(false);
      cleanLiveNodes();
      activeStatus = null;
      lastSearchedCount = 0;
      streamedContent = "";
      memoryUpdatedInTurn = false;
      renderedCount = -1;
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  send.addEventListener("click", () => {
    if (sending) {
      stopGeneration();
    } else {
      void submit();
    }
    input.focus();
  });

  input.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") {
      e.preventDefault();
      if (!sending) {
        void submit();
      }
      input.focus();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const wantChip = file?.name ?? "";
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) {
          chipRow.append(
            contextChip(wantChip, () => {
              State.droppedFile = null;
              State.promptContext = null;
              chipRow.dataset.label = "";
              clear(chipRow);
              State.notify();
              onHeightChange();
              input.focus();
            }),
          );
        }
      }

      const hasHistory = State.chatHistory.length > 0;
      clearBtn.style.opacity = hasHistory ? "1" : "0";
      clearBtn.style.pointerEvents = hasHistory ? "auto" : "none";

      if (!sending) {
        const count = State.chatHistory.length;
        if (count !== renderedCount) {
          renderedCount = count;
          clear(log);
          for (const m of State.chatHistory) log.append(bubble(m));
          log.scrollTop = log.scrollHeight;
        }
      }

      input.placeholder = State.chatHistory.length === 0 ? "Ask me anything…" : "Continue…";
      // Keep input persistently enabled and focused so user never has to re-click
    },
    focus() {
      input.focus();
    },
  };
}
