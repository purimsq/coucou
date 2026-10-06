// Settings window — the place where anything that writes to disk is confirmed.
// Stage 2 covers the Claude Code hooks and the general preferences; API keys and
// integrations land here too in a later stage.

import "./settings.css";
import { Bridge, onEvent, type HookStatus, type UserProfile } from "../core/bridge";
import { DEFAULT_SETTINGS, type Settings } from "../core/state";
import { h, clear, svg, dot } from "../views/dom";
import { ICONS } from "../views/icons";

let settings: Settings = { ...DEFAULT_SETTINGS };
let version = "";

const root = document.getElementById("settings-root")!;

async function save() {
  await Bridge.saveSettings(settings);
}

// ── Reusable bits ─────────────────────────────────────────────────────────────

function toggle(on: boolean, onChange: (v: boolean) => void): HTMLElement {
  const el = h("button", { class: on ? "switch on" : "switch", "aria-pressed": on });
  el.addEventListener("click", () => {
    const next = !el.classList.contains("on");
    el.classList.toggle("on", next);
    onChange(next);
  });
  return el;
}

function statusDot(ok: boolean): HTMLElement {
  return h("i", { class: "dot", style: `background:${ok ? "#22c55e" : "#f4505e"}` });
}

function renderDiff(text: string): HTMLElement {
  const box = h("div", { class: "diff" });
  for (const line of text.split("\n")) {
    const cls = line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : "ctx";
    box.append(h("div", { class: cls, text: line }));
  }
  return box;
}

// ── Claude Code section ───────────────────────────────────────────────────────

function claudeSection(status: HookStatus): HTMLElement {
  const body = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  const section = h(
    "section",
    {},
    h("h2", {}, statusDot(status.installed), h("span", { text: "Claude Code" })),
    body,
  );

  const rebuild = async () => {
    const fresh = await Bridge.hooksStatus();
    if (fresh) Object.assign(status, fresh);
    clear(body);
    draw();
    const head = section.querySelector("h2")!;
    clear(head);
    head.append(statusDot(status.installed), h("span", { text: "Claude Code" }));
  };

  function draw() {
    body.append(
      h("div", {
        class: "hint",
        text: status.installed
          ? "Coucou is hooked into your Claude Code sessions. Tool calls, questions and permission requests show up in the island, and you can answer them there."
          : "Install the hooks to see your Claude Code sessions in the island and approve permissions without leaving what you are doing.",
      }),
      h("div", { class: "row" },
        h("label", { text: "settings.json" }),
        h("span", { class: "path", text: status.settingsPath }),
      ),
      h("div", { class: "row" },
        h("label", { text: "Relay" }),
        h("span", { class: "path", text: status.hookPath }),
        statusDot(status.hookReady),
      ),
    );

    if (!status.hookReady) {
      body.append(h("div", {
        class: "notice warn",
        text: "coucou-hook.exe is not in place yet. Restart Coucou; if it still fails, build it with `cargo build -p coucou-hook`.",
      }));
    }

    const actions = h("div", { class: "row" });
    const install = h("button", {
      class: "primary",
      text: status.installed ? "Reinstall hooks…" : "Install hooks…",
      onclick: () => showPreview(true),
    });
    // Writing hook commands that point at a relay which isn't there would give
    // every Claude Code session a broken hook and nothing to show for it.
    if (!status.hookReady) {
      install.disabled = true;
      install.title = "The relay isn't installed yet.";
    }
    actions.append(install);
    if (status.installed) {
      actions.append(h("button", {
        class: "danger",
        text: "Uninstall hooks…",
        onclick: () => showPreview(false),
      }));
    }
    body.append(actions);
  }

  async function showPreview(install: boolean) {
    let preview;
    try {
      preview = await Bridge.hooksPreview(install);
    } catch (err) {
      // An unreadable or invalid settings.json stops here rather than being
      // treated as empty and written over.
      clear(body);
      body.append(
        h("div", { class: "notice err", text: String(err).replace(/^Error:\s*/, "") }),
        h("div", { class: "row" }, h("button", {
          text: "Back",
          onclick: () => { clear(body); draw(); },
        })),
      );
      return;
    }
    if (!preview) return;
    clear(body);
    body.append(
      h("div", {
        class: "hint",
        text: install
          ? "This is exactly what will change in your settings.json. Your own hooks are left untouched."
          : "This removes Coucou's entries only. Your own hooks are left untouched.",
      }),
      renderDiff(preview.diff),
      h("div", { class: "row" },
        h("span", { class: "path", text: `Backup → ${preview.backup}` }),
      ),
    );
    const confirm = h("button", {
      class: install ? "primary" : "danger",
      text: install ? "Back up and write" : "Back up and remove",
    });
    confirm.addEventListener("click", async () => {
      confirm.disabled = true;
      try {
        const backup = await Bridge.hooksApply(install, preview.fingerprint);
        clear(body);
        body.append(h("div", {
          class: "notice ok",
          text: `Done. Previous settings saved as ${backup}. Open a new Claude Code session to pick the hooks up.`,
        }));
        window.setTimeout(() => void rebuild(), 2600);
      } catch (err) {
        confirm.disabled = false;
        body.append(h("div", { class: "notice err", text: `Could not write: ${String(err)}` }));
      }
    });
    body.append(h("div", { class: "row" }, confirm, h("button", {
      text: "Cancel",
      onclick: () => { clear(body); draw(); },
    })));
  }

  draw();
  return section;
}

// ── Claude API section ────────────────────────────────────────────────────────

const MODELS: [string, string][] = [
  ["claude-opus-5", "Claude Opus 5"],
  ["claude-sonnet-5", "Claude Sonnet 5"],
  ["claude-haiku-4-5", "Claude Haiku 4.5"],
];

let updateProviderBadges: () => void = () => {};

function apiSection(hasKey: boolean): HTMLElement {
  const dot = statusDot(hasKey);
  const state = h("span", { class: "hint", text: hasKey ? "Key saved in the Windows Credential Manager." : "No key yet — the chat needs one." });
  const activeBadge = h("span", { class: "badge-active", text: "Active" });
  const activateBtn = h("button", { class: "btn-activate", text: "Use for chat" });

  const field = h("input", {
    type: "password",
    placeholder: hasKey ? "••••••••••••  (stored)" : "sk-ant-...",
    style: "flex:1 1 auto;min-width:0",
    autocomplete: "off",
    spellcheck: "false",
  }) as HTMLInputElement;

  const saveBtn = h("button", { class: "primary", text: "Save key" });
  const clearBtn = h("button", { class: "danger", text: "Remove" });
  const feedback = h("div", {});

  async function refresh() {
    const present = (await Bridge.secretPresent("anthropic-api-key")) ?? false;
    dot.style.background = present ? "#22c55e" : "#f4505e";
    state.textContent = present
      ? "Key saved in the Windows Credential Manager."
      : "No key yet — the chat needs one.";
    field.placeholder = present ? "••••••••••••  (stored)" : "sk-ant-...";
    clearBtn.style.display = present ? "" : "none";
  }

  const model = h("select", {}) as HTMLSelectElement;
  for (const [id, label] of MODELS) model.append(h("option", { value: id, text: label }));
  if (!MODELS.some(([id]) => id === settings.model)) {
    model.append(h("option", { value: settings.model, text: settings.model }));
  }
  model.value = !settings.model.startsWith("gemini") ? settings.model : "claude-opus-5";
  model.addEventListener("change", () => {
    settings.model = model.value;
    void save();
    updateProviderBadges();
  });

  activateBtn.addEventListener("click", () => {
    settings.model = model.value || "claude-opus-5";
    void save();
    updateProviderBadges();
  });

  saveBtn.addEventListener("click", async () => {
    const value = field.value.trim();
    if (!value) return;
    clear(feedback);
    try {
      await Bridge.secretSet("anthropic-api-key", value);
      field.value = "";
      settings.model = model.value || "claude-opus-5";
      await save();
      feedback.append(h("div", { class: "notice ok", text: "Saved. Claude is active for chat." }));
      await refresh();
      updateProviderBadges();
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Could not save: ${String(err)}` }));
    }
  });

  clearBtn.addEventListener("click", async () => {
    clear(feedback);
    try {
      await Bridge.secretClear("anthropic-api-key");
      feedback.append(h("div", { class: "notice ok", text: "Key removed." }));
      await refresh();
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Could not remove: ${String(err)}` }));
    }
  });

  clearBtn.style.display = hasKey ? "" : "none";

  const prevUpdate = updateProviderBadges;
  updateProviderBadges = () => {
    prevUpdate();
    const isGemini = settings.model.startsWith("gemini");
    activeBadge.style.display = !isGemini ? "" : "none";
    activateBtn.style.display = isGemini ? "" : "none";
    if (!isGemini && MODELS.some(([id]) => id === settings.model)) {
      model.value = settings.model;
    }
  };

  return h(
    "section",
    {},
    h("h2", {}, dot, h("span", { text: "Claude" }), activeBadge, activateBtn),
    state,
    h("div", { class: "row" }, h("label", { text: "API key" }), field, saveBtn, clearBtn),
    h("div", { class: "row" }, h("label", { text: "Model" }), model),
    feedback,
  );
}

// ── Google AI (Gemini) section ────────────────────────────────────────────────

const GEMINI_MODELS: [string, string][] = [
  ["gemini-2.0-flash", "Gemini 2.0 Flash (Recommended)"],
  ["gemini-1.5-flash", "Gemini 1.5 Flash"],
  ["gemini-1.5-pro", "Gemini 1.5 Pro"],
  ["gemini-3.8-flash", "Gemini 3.8 Flash (Preview / Billing)"],
  ["gemini-3.5-flash", "Gemini 3.5 Flash (Preview / Billing)"],
  ["gemini-2.5-flash", "Gemini 2.5 Flash (Legacy)"],
];

function geminiSection(hasKey: boolean): HTMLElement {
  const dot = statusDot(hasKey);
  const state = h("span", { class: "hint", text: hasKey ? "Key saved in the Windows Credential Manager." : "No key yet — enter your Google AI key to chat with Gemini." });
  const activeBadge = h("span", { class: "badge-active", text: "Active" });
  const activateBtn = h("button", { class: "btn-activate", text: "Use for chat" });

  const field = h("input", {
    type: "password",
    placeholder: hasKey ? "••••••••••••  (stored)" : "AIzaSy...",
    style: "flex:1 1 auto;min-width:0",
    autocomplete: "off",
    spellcheck: "false",
  }) as HTMLInputElement;

  const saveBtn = h("button", { class: "primary", text: "Save key" });
  const clearBtn = h("button", { class: "danger", text: "Remove" });
  const feedback = h("div", {});

  async function refresh() {
    const present = (await Bridge.secretPresent("gemini-api-key")) ?? false;
    dot.style.background = present ? "#22c55e" : "#f4505e";
    state.textContent = present
      ? "Key saved in the Windows Credential Manager."
      : "No key yet — enter your Google AI key to chat with Gemini.";
    field.placeholder = present ? "••••••••••••  (stored)" : "AIzaSy...";
    clearBtn.style.display = present ? "" : "none";
  }

  const model = h("select", {}) as HTMLSelectElement;
  for (const [id, label] of GEMINI_MODELS) model.append(h("option", { value: id, text: label }));
  if (!GEMINI_MODELS.some(([id]) => id === settings.model)) {
    model.append(h("option", { value: settings.model, text: settings.model }));
  }
  model.value = settings.model.startsWith("gemini") ? settings.model : "gemini-2.0-flash";
  model.addEventListener("change", () => {
    settings.model = model.value;
    void save();
    updateProviderBadges();
  });

  activateBtn.addEventListener("click", () => {
    settings.model = model.value || "gemini-2.0-flash";
    void save();
    updateProviderBadges();
  });

  saveBtn.addEventListener("click", async () => {
    const value = field.value.trim();
    if (!value) return;
    clear(feedback);
    try {
      await Bridge.secretSet("gemini-api-key", value);
      field.value = "";
      settings.model = model.value || "gemini-2.0-flash";
      await save();
      const modelLabel = model.selectedOptions[0]?.text || "Gemini";
      feedback.append(h("div", { class: "notice ok", text: `Saved. ${modelLabel} is active for chat.` }));
      await refresh();
      updateProviderBadges();
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Could not save: ${String(err)}` }));
    }
  });

  clearBtn.addEventListener("click", async () => {
    clear(feedback);
    try {
      await Bridge.secretClear("gemini-api-key");
      feedback.append(h("div", { class: "notice ok", text: "Key removed." }));
      await refresh();
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Could not remove: ${String(err)}` }));
    }
  });

  clearBtn.style.display = hasKey ? "" : "none";

  const prevUpdate = updateProviderBadges;
  updateProviderBadges = () => {
    prevUpdate();
    const isGemini = settings.model.startsWith("gemini");
    activeBadge.style.display = isGemini ? "" : "none";
    activateBtn.style.display = !isGemini ? "" : "none";
    if (isGemini && GEMINI_MODELS.some(([id]) => id === settings.model)) {
      model.value = settings.model;
    }
  };

  return h(
    "section",
    {},
    h("h2", {}, dot, h("span", { text: "Google AI (Gemini)" }), activeBadge, activateBtn),
    state,
    h("div", { class: "row" }, h("label", { text: "API key" }), field, saveBtn, clearBtn),
    h("div", { class: "row" }, h("label", { text: "Model" }), model),
    feedback,
  );
}

// ── Mochi's Memory & User Profile section ────────────────────────────────────

function formatDate(ts: string): string {
  const num = Number(ts);
  if (!num || isNaN(num)) return "Recently";
  const date = new Date(num > 1e11 ? num : num * 1000);
  const now = Date.now();
  const diffSec = Math.floor((now - date.getTime()) / 1000);
  if (diffSec < 60) return "Just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

function memorySection(profile: UserProfile): HTMLElement {
  let currentProfile: UserProfile = { ...profile };

  const nameVal = h("span", { class: "memory-stat-val", text: currentProfile.name || "Not configured" });
  const countVal = h("span", {
    class: "memory-stat-val",
    text: `${currentProfile.facts.length} ${currentProfile.facts.length === 1 ? "fact" : "facts"}`,
  });
  const updatedVal = h("span", {
    class: "memory-stat-val",
    text: formatDate(currentProfile.updatedAt),
  });

  const countBadge = h("span", {
    class: "badge-active",
    style: "background:rgba(168,85,247,0.15);color:#d8b4fe;border-color:rgba(168,85,247,0.3)",
    text: `${currentProfile.facts.length} memories`,
  });

  const openBtn = h("button", {
    class: "primary",
    style: "display:inline-flex;align-items:center;gap:6px",
  }, svg(ICONS.sparkles, 13), h("span", { text: "Open Memory Manager…" }));

  const section = h(
    "section",
    {},
    h("h2", {}, dot("#a855f7", 8), h("span", { text: "Mochi's Memory & User Profile" }), countBadge),
    h("div", {
      class: "hint",
      text: "Mochi continuously learns your identity, preferences, and interests to personalize answers. This long-term memory stays saved on your PC even when you clear chat history.",
    }),
    h("div", { class: "memory-summary-row" },
      h("div", { class: "memory-stat" }, h("span", { class: "memory-stat-label", text: "User Name" }), nameVal),
      h("div", { class: "memory-stat" }, h("span", { class: "memory-stat-label", text: "Learned Facts" }), countVal),
      h("div", { class: "memory-stat" }, h("span", { class: "memory-stat-label", text: "Last Updated" }), updatedVal),
      openBtn,
    ),
  );

  openBtn.addEventListener("click", () => {
    openMemoryModal(currentProfile, (updated) => {
      currentProfile = updated;
      nameVal.textContent = currentProfile.name || "Not configured";
      countVal.textContent = `${currentProfile.facts.length} ${currentProfile.facts.length === 1 ? "fact" : "facts"}`;
      updatedVal.textContent = formatDate(currentProfile.updatedAt);
      countBadge.textContent = `${currentProfile.facts.length} memories`;
    });
  });

  return section;
}

function openMemoryModal(
  profile: UserProfile,
  onUpdate: (updated: UserProfile) => void,
) {
  let workingProfile: UserProfile = { ...profile, facts: [...profile.facts] };
  let selectedCategory = "all";
  let searchQuery = "";

  const backdrop = h("div", { class: "modal-backdrop" });
  const closeBtn = h("button", { class: "modal-close-btn", title: "Close (Esc)" }, svg(ICONS.xmark, 14));
  const feedback = h("div", { style: "display:flex;flex-direction:column;gap:6px;padding:0 18px" });

  const modal = h(
    "div",
    { class: "modal-window" },
    h(
      "div",
      { class: "modal-header" },
      h("h2", {}, dot("#a855f7", 8), svg(ICONS.sparkles, 14), h("span", { text: "Mochi's Memory & User Profile" })),
      closeBtn,
    ),
    feedback,
  );

  const body = h("div", { class: "modal-body" });
  modal.append(body);
  backdrop.append(modal);
  document.body.append(backdrop);

  function close() {
    backdrop.classList.remove("open");
    window.removeEventListener("keydown", onKeyDown);
    setTimeout(() => backdrop.remove(), 200);
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  }

  window.addEventListener("keydown", onKeyDown);
  closeBtn.addEventListener("click", close);
  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) close();
  });

  requestAnimationFrame(() => backdrop.classList.add("open"));

  function notifyUpdated() {
    onUpdate(workingProfile);
  }

  // 1. Profile Info Card (Name & Instructions/Notes)
  const nameInput = h("input", {
    type: "text",
    placeholder: "Your name (e.g. Dylen)",
    value: workingProfile.name || "",
  }) as HTMLInputElement;

  const notesInput = h("textarea", {
    class: "memory-textarea",
    placeholder: "General notes or instructions Mochi should always remember (e.g. Keep answers concise, my PC cooling fan is broken so avoid heavy tasks, favorite team is Lakers)...",
  }) as HTMLTextAreaElement;
  notesInput.value = workingProfile.notes || "";

  const saveProfileBtn = h("button", { class: "primary", text: "Save Profile Details" });
  saveProfileBtn.addEventListener("click", async () => {
    saveProfileBtn.disabled = true;
    clear(feedback);
    try {
      workingProfile.name = nameInput.value.trim();
      workingProfile.notes = notesInput.value.trim();
      workingProfile.updatedAt = String(Math.floor(Date.now() / 1000));
      await Bridge.profileSave(workingProfile);
      feedback.append(h("div", { class: "notice ok", text: "Profile details saved successfully!" }));
      notifyUpdated();
      setTimeout(() => clear(feedback), 3000);
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Failed to save profile: ${String(err)}` }));
    } finally {
      saveProfileBtn.disabled = false;
    }
  });

  const profileCard = h(
    "div",
    { class: "profile-card" },
    h("h3", {}, h("span", { text: "User Profile & Custom Notes" })),
    h("div", { class: "row" }, h("label", { text: "Your Name" }), nameInput),
    h("div", { style: "display:flex;flex-direction:column;gap:6px" },
      h("label", { style: "color:var(--dim);font-size:12px", text: "Permanent Notes & Context" }),
      notesInput,
    ),
    h("div", { class: "row", style: "justify-content:flex-end" }, saveProfileBtn),
  );

  // 2. Learned Facts Explorer Card
  const factsListContainer = h("div", { class: "facts-container" });
  const searchInput = h("input", {
    type: "text",
    class: "search-input",
    placeholder: "Search memories…",
  }) as HTMLInputElement;

  searchInput.addEventListener("input", () => {
    searchQuery = searchInput.value.toLowerCase().trim();
    renderFacts();
  });

  const categories = [
    { id: "all", label: "All" },
    { id: "identity", label: "Identity" },
    { id: "preference", label: "Preferences" },
    { id: "interest", label: "Interests" },
    { id: "note", label: "Notes" },
  ];

  const filterBar = h("div", { class: "filter-bar" });

  function renderFilterTabs() {
    clear(filterBar);
    for (const cat of categories) {
      const count = cat.id === "all"
        ? workingProfile.facts.length
        : workingProfile.facts.filter((f) => f.category === cat.id).length;
      const pill = h("button", {
        class: selectedCategory === cat.id ? "filter-pill active" : "filter-pill",
        text: `${cat.label} (${count})`,
      });
      pill.addEventListener("click", () => {
        selectedCategory = cat.id;
        renderFilterTabs();
        renderFacts();
      });
      filterBar.append(pill);
    }
    filterBar.append(searchInput);
  }

  function renderFacts() {
    clear(factsListContainer);
    let filtered = workingProfile.facts;
    if (selectedCategory !== "all") {
      filtered = filtered.filter((f) => f.category === selectedCategory);
    }
    if (searchQuery) {
      filtered = filtered.filter((f) => f.text.toLowerCase().includes(searchQuery));
    }

    if (filtered.length === 0) {
      factsListContainer.append(h("div", {
        class: "empty-state",
        text: workingProfile.facts.length === 0
          ? "Mochi has not learned any facts yet. She stores details automatically as you chat, or you can add one manually below!"
          : "No memories match your filter search.",
      }));
      return;
    }

    for (const fact of filtered) {
      const badgeCls = `fact-badge ${fact.category || "preference"}`;
      const delBtn = h("button", { class: "fact-del-btn", title: "Forget this fact" }, svg(ICONS.trash, 12));

      delBtn.addEventListener("click", async () => {
        delBtn.disabled = true;
        try {
          const updated = await Bridge.profileFactDelete(fact.id);
          if (updated) {
            workingProfile = updated;
          } else {
            workingProfile.facts = workingProfile.facts.filter((f) => f.id !== fact.id);
          }
          notifyUpdated();
          renderFilterTabs();
          renderFacts();
        } catch (err) {
          feedback.append(h("div", { class: "notice err", text: `Could not delete memory: ${String(err)}` }));
        }
      });

      const row = h(
        "div",
        { class: "fact-row" },
        h("div", { class: "fact-content" },
          h("div", { style: "display:flex;align-items:center;gap:8px" },
            h("span", { class: badgeCls, text: fact.category }),
            h("span", { class: "fact-date", text: formatDate(fact.updatedAt) }),
          ),
          h("span", { class: "fact-text", text: fact.text }),
        ),
        delBtn,
      );
      factsListContainer.append(row);
    }
  }

  // 3. Add Fact Row
  const newCatSelect = h("select", {},
    h("option", { value: "preference", text: "Preference" }),
    h("option", { value: "identity", text: "Identity" }),
    h("option", { value: "interest", text: "Interest" }),
    h("option", { value: "note", text: "Note" }),
  ) as HTMLSelectElement;

  const newFactInput = h("input", {
    type: "text",
    style: "flex:1 1 180px",
    placeholder: "Teach Mochi a new fact (e.g. Favorite team is Lakers)…",
  }) as HTMLInputElement;

  const addFactBtn = h("button", { text: "+ Add Memory" });

  async function submitNewFact() {
    const text = newFactInput.value.trim();
    if (!text) return;
    addFactBtn.disabled = true;
    clear(feedback);
    try {
      const updated = await Bridge.profileFactAdd(newCatSelect.value, text);
      if (updated) workingProfile = updated;
      newFactInput.value = "";
      notifyUpdated();
      renderFilterTabs();
      renderFacts();
      feedback.append(h("div", { class: "notice ok", text: "New memory stored!" }));
      setTimeout(() => clear(feedback), 2500);
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Failed to add memory: ${String(err)}` }));
    } finally {
      addFactBtn.disabled = false;
    }
  }

  addFactBtn.addEventListener("click", () => void submitNewFact());
  newFactInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      void submitNewFact();
    }
  });

  const addRow = h(
    "div",
    { class: "row", style: "margin-top:6px" },
    newCatSelect,
    newFactInput,
    addFactBtn,
  );

  const factsCard = h(
    "div",
    { class: "profile-card" },
    h("h3", {}, h("span", { text: "Stored Facts & Preferences" })),
    filterBar,
    factsListContainer,
    addRow,
  );

  // 4. Reset Memory Danger Row
  const wipeBtn = h("button", { class: "danger", text: "Wipe Mochi's Memory…" });
  wipeBtn.addEventListener("click", async () => {
    const confirmed = confirm(
      "Are you sure you want Mochi to forget all learned facts and user profile details? (Your chat history will stay untouched).",
    );
    if (!confirmed) return;
    wipeBtn.disabled = true;
    try {
      await Bridge.profileClear();
      workingProfile = {
        name: "",
        notes: "",
        facts: [],
        updatedAt: String(Math.floor(Date.now() / 1000)),
      };
      nameInput.value = "";
      notesInput.value = "";
      notifyUpdated();
      renderFilterTabs();
      renderFacts();
      feedback.append(h("div", { class: "notice ok", text: "Mochi's memory has been wiped clean." }));
      setTimeout(() => clear(feedback), 3000);
    } catch (err) {
      feedback.append(h("div", { class: "notice err", text: `Failed to wipe memory: ${String(err)}` }));
    } finally {
      wipeBtn.disabled = false;
    }
  });

  const dangerCard = h(
    "div",
    { class: "row", style: "justify-content:space-between;border-top:1px solid var(--hairline);padding-top:12px;margin-top:4px" },
    h("span", { class: "hint", text: "Reset Mochi's memory back to initial state." }),
    wipeBtn,
  );

  renderFilterTabs();
  renderFacts();

  body.append(profileCard, factsCard, dangerCard);
}

// ── Integrations section ──────────────────────────────────────────────────────

interface IntegrationDef {
  id: string;
  name: string;
  color: string;
  /** Credential Manager keys, in the order they are shown. */
  fields: { key: string; label: string; placeholder: string; secret: boolean }[];
}

const INTEGRATIONS: IntegrationDef[] = [
  { id: "integration_stripe", name: "Stripe", color: "#0570DE",
    fields: [{ key: "stripe-api-key", label: "Secret key", placeholder: "sk_live_…", secret: true }] },
  { id: "integration_github", name: "GitHub", color: "#F4505E",
    fields: [{ key: "github-token", label: "Token", placeholder: "ghp_…", secret: true }] },
  { id: "integration_vercel", name: "Vercel", color: "#7C5CFF",
    fields: [{ key: "vercel-token", label: "Token", placeholder: "…", secret: true }] },
  { id: "integration_n8n", name: "n8n", color: "#F29B38",
    fields: [
      { key: "n8n-url", label: "Instance URL", placeholder: "https://n8n.example.com", secret: false },
      { key: "n8n-api-key", label: "API key", placeholder: "…", secret: true },
    ] },
  { id: "integration_resend", name: "Resend", color: "#22C55E",
    fields: [{ key: "resend-api-key", label: "API key", placeholder: "re_…", secret: true }] },
  { id: "integration_notion", name: "Notion", color: "#8C8C8C",
    fields: [{ key: "notion-api-key", label: "Integration token", placeholder: "ntn_…", secret: true }] },
  { id: "integration_calcom", name: "Cal.com", color: "#C9956A",
    fields: [{ key: "calcom-api-key", label: "API key", placeholder: "cal_…", secret: true }] },
];

const MAX_ACTIVE = 4;

function integrationsSection(present: Record<string, boolean>): HTMLElement {
  const note = h("div", { class: "hint" });
  const list = h("div", { style: "display:flex;flex-direction:column;gap:14px" });

  function updateNote() {
    const used = settings.activeIntegrations.length;
    note.textContent = `Pick up to ${MAX_ACTIVE} pills to show next to Mochi — ${used}/${MAX_ACTIVE} in use. Keys are stored in the Windows Credential Manager, never on disk.`;
  }

  for (const def of INTEGRATIONS) {
    const active = settings.activeIntegrations.includes(def.id);
    const sw = h("button", { class: active ? "switch on" : "switch" });
    sw.addEventListener("click", () => {
      const on = settings.activeIntegrations.includes(def.id);
      if (on) {
        settings.activeIntegrations = settings.activeIntegrations.filter((x) => x !== def.id);
      } else {
        if (settings.activeIntegrations.length >= MAX_ACTIVE) return;
        settings.activeIntegrations = [...settings.activeIntegrations, def.id];
      }
      sw.classList.toggle("on", !on);
      updateNote();
      void save();
    });

    const rows = h("div", { style: "display:flex;flex-direction:column;gap:6px;flex:1 1 auto;min-width:0" });
    for (const field of def.fields) {
      const input = h("input", {
        type: field.secret ? "password" : "text",
        placeholder: present[field.key] ? "••••••••  (stored)" : field.placeholder,
        autocomplete: "off",
        spellcheck: "false",
        style: "flex:1 1 auto;min-width:0",
      }) as HTMLInputElement;
      const saveBtn = h("button", { text: "Save" });
      const dotEl = statusDot(present[field.key] ?? false);
      saveBtn.addEventListener("click", async () => {
        const value = input.value.trim();
        try {
          await Bridge.secretSet(field.key, value);
          present[field.key] = value.length > 0;
          input.value = "";
          input.placeholder = value ? "••••••••  (stored)" : field.placeholder;
          dotEl.style.background = value ? "#22c55e" : "#f4505e";
        } catch {
          dotEl.style.background = "#f5a524";
        }
      });
      rows.append(
        h("div", { class: "row" },
          h("label", { style: "min-width:104px", text: field.label }),
          input, saveBtn, dotEl,
        ),
      );
    }

    list.append(
      h("div", { style: "display:flex;gap:12px;align-items:flex-start" },
        h("div", { style: "display:flex;align-items:center;gap:8px;min-width:132px;padding-top:4px" },
          sw,
          h("i", { class: "dot", style: `background:${def.color}` }),
          h("span", { style: "font-size:12.5px", text: def.name }),
        ),
        rows,
      ),
    );
  }

  updateNote();
  return h("section", {}, h("h2", {}, h("span", { text: "Integrations" })), note, list);
}

// ── General section ───────────────────────────────────────────────────────────

function generalSection(): HTMLElement {
  const volume = h("input", {
    type: "range", min: "0", max: "0.2", step: "0.005",
    value: String(settings.soundVolume),
  }) as HTMLInputElement;
  volume.addEventListener("input", () => {
    settings.soundVolume = Number(volume.value);
    void save();
  });

  const autoClose = h("input", {
    type: "number", min: "5", max: "120", step: "1",
    value: String(Math.round(settings.autoCloseInterval)),
    style: "width:72px",
  }) as HTMLInputElement;
  autoClose.addEventListener("change", () => {
    settings.autoCloseInterval = Math.max(5, Math.min(120, Number(autoClose.value) || 15));
    autoClose.value = String(settings.autoCloseInterval);
    void save();
  });

  const screen = h("select", {}) as HTMLSelectElement;
  screen.append(
    h("option", { value: "primary", text: "Main display" }),
    h("option", { value: "cursor", text: "Display under the cursor" }),
  );
  screen.value = settings.screen;
  screen.addEventListener("change", () => {
    settings.screen = screen.value as Settings["screen"];
    void save();
  });

  return h(
    "section",
    {},
    h("h2", {}, h("span", { text: "General" })),
    h("div", { class: "row" },
      h("label", { text: "Sound" }),
      toggle(settings.soundEnabled, (v) => { settings.soundEnabled = v; void save(); }),
      volume,
    ),
    h("div", { class: "row" },
      h("label", { text: "Auto-close" }),
      autoClose,
      h("span", { class: "hint", text: "seconds after you leave the island" }),
    ),
    h("div", { class: "row" },
      h("label", { text: "Island lives on" }),
      screen,
    ),
    h("div", { class: "row" },
      h("label", { text: "Launch at startup" }),
      toggle(settings.autostart, (v) => { settings.autostart = v; void save(); }),
    ),
  );
}

// ── Boot ──────────────────────────────────────────────────────────────────────

async function main() {
  const boot = await Bridge.boot();
  if (boot) {
    settings = { ...settings, ...boot.settings };
    version = boot.version;
  }
  const status = (await Bridge.hooksStatus()) ?? {
    installed: false, settingsPath: "", hookPath: "", hookReady: false,
  };

  const hasKey = (await Bridge.secretPresent("anthropic-api-key")) ?? false;
  const hasGeminiKey = (await Bridge.secretPresent("gemini-api-key")) ?? false;

  const keys = [
    "stripe-api-key", "github-token", "vercel-token",
    "n8n-url", "n8n-api-key", "resend-api-key", "notion-api-key", "calcom-api-key",
  ];
  const present: Record<string, boolean> = {};
  for (const k of keys) present[k] = (await Bridge.secretPresent(k)) ?? false;

  const userProfile = (await Bridge.profileGet()) ?? {
    name: "",
    notes: "",
    facts: [],
    updatedAt: "0",
  };

  clear(root);
  root.append(
    h("h1", {}, h("span", { text: "Coucou" }), h("span", { class: "version", text: version })),
    claudeSection(status),
    apiSection(hasKey),
    geminiSection(hasGeminiKey),
    memorySection(userProfile),
    integrationsSection(present),
    generalSection(),
    h("div", {
      class: "hint",
      text: "No telemetry. Network requests only go to the services you configure yourself.",
    }),
  );

  updateProviderBadges();

  void onEvent<Settings>("settings-changed", (s) => {
    settings = { ...settings, ...s };
    updateProviderBadges();
  });
}

void main();
