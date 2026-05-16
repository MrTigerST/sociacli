const IPC_URL = "ws://127.0.0.1:8923";
const RECONNECT_MS = 1500;

const stack = document.getElementById("stack");
const statusEl = document.getElementById("status");
const clearAllBtn = document.getElementById("clear-all");

let cfg = {
  notify_sound: null,
  notify_auto_dismiss: false,
  notify_auto_dismiss_ms: 6000,
  notify_show_list: true,
};
let ws = null;
// user_id → username, kept fresh from the "friends" sticky event.
const usernames = new Map();
// Most recent rendered game invite card — A / D shortcuts target this.
let focusedInvite = null;

document.documentElement.classList.add("hidden");
setWindowVisible(false);
connect();
wireGlobalKeys();
if (clearAllBtn) clearAllBtn.addEventListener("click", clearAll);

function setWindowVisible(visible) {
  try {
    const t = window.__TAURI__;
    const invoke = t?.core?.invoke || t?.invoke;
    invoke?.("set_visible", { visible });
  } catch {}
}

function connect() {
  ws = new WebSocket(IPC_URL);
  ws.onopen = () => {};
  ws.onclose = () => setTimeout(connect, RECONNECT_MS);
  ws.onerror = () => ws.close();
  ws.onmessage = (e) => {
    let ev;
    try { ev = JSON.parse(e.data); } catch { return; }
    handle(ev);
  };
}

function sendDaemon(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
  }
}

function handle(ev) {
  switch (ev.kind) {
    case "config":
      cfg = { ...cfg, ...ev.payload };
      break;
    case "friends": {
      const list = Array.isArray(ev.payload?.friends) ? ev.payload.friends : [];
      usernames.clear();
      for (const f of list) {
        if (f && f.id && f.username) usernames.set(f.id, f.username);
      }
      break;
    }
    case "hello":
      break;
    case "action":
      showAction(ev.payload);
      playSound();
      break;
    case "update":
      showUpdate(ev.payload);
      playSound();
      break;
    case "friend_request":
      showFriendRequest(ev.payload);
      playSound();
      break;
    case "presence":
      break;
    case "perm_changed":
      showAction({
        from: ev.payload.by,
        from_username: usernames.get(ev.payload.by) || ev.payload.by,
        action: "custom",
        title: "Permission changed",
        body: `${ev.payload.action}: ${ev.payload.allowed ? "allowed" : "blocked"}`,
        data: {},
      });
      break;
    case "focus":
      document.body.classList.add("flash");
      setTimeout(() => document.body.classList.remove("flash"), 250);
      revealWindow();
      break;
  }
}

const ACTION_LABEL = {
  message: "Message",
  game_invite: "Game invite",
  ptt: "PTT",
  custom: "Alert",
  plugin: "Plugin",
};

function showAction({ from, from_username, action, title, body, data }) {
  if (!cfg.notify_show_list) {
    while (stack.firstChild) stack.firstChild.remove();
  }

  const displayName = from_username || usernames.get(from) || from || "?";
  const kindKey = action || "custom";

  const card = document.createElement("div");
  card.className = `card action-${kindKey}`;

  // Header row with kind chip + sender + close button.
  const closeBtn = document.createElement("button");
  closeBtn.className = "close";
  closeBtn.type = "button";
  closeBtn.setAttribute("aria-label", "Dismiss");
  closeBtn.textContent = "×";
  closeBtn.addEventListener("click", () => dismissCard(card));

  const row = document.createElement("div");
  row.className = "row";
  const kindEl = document.createElement("span");
  kindEl.className = "kind";
  kindEl.textContent = ACTION_LABEL[kindKey] || kindKey;
  const fromEl = document.createElement("span");
  fromEl.className = "from";
  fromEl.textContent = displayName;
  row.append(kindEl, fromEl, closeBtn);

  const t = document.createElement("div");
  t.className = "title";
  t.textContent = title || "";

  const b = document.createElement("div");
  b.className = "body";
  if (kindKey === "game_invite" && data && data.url) {
    const a = document.createElement("a");
    a.href = data.url;
    a.target = "_blank";
    a.rel = "noopener";
    a.textContent = body || data.url;
    b.appendChild(a);
  } else {
    b.textContent = body || "";
  }

  card.append(row, t, b);

  // Game invites get richer affordances.
  if (kindKey === "game_invite") {
    const url = (data && data.url) || "";
    const actions = document.createElement("div");
    actions.className = "actions";
    const accept = document.createElement("button");
    accept.className = "btn btn-accept";
    accept.textContent = "Accept (A)";
    accept.addEventListener("click", () => acceptInvite(card, from, url));
    const decline = document.createElement("button");
    decline.className = "btn btn-decline";
    decline.textContent = "Decline (D)";
    decline.addEventListener("click", () => declineInvite(card, from));
    actions.append(accept, decline);
    card.append(actions);
    focusedInvite = { card, from, url };
  }

  // Generic custom buttons attached via `--button label=reply`.
  const buttons = Array.isArray(data?.buttons) ? data.buttons : [];
  if (buttons.length > 0 && kindKey !== "game_invite") {
    const actions = document.createElement("div");
    actions.className = "actions";
    for (const btn of buttons.slice(0, 4)) {
      const el = document.createElement("button");
      el.className = "btn";
      el.textContent = btn.label || "(reply)";
      el.addEventListener("click", () => {
        sendReply(from, btn.reply ?? btn.label ?? "");
        dismissCard(card);
      });
      actions.append(el);
    }
    card.append(actions);
  }

  stack.prepend(card);
  stack.scrollTop = 0;
  revealWindow();

  if (cfg.notify_auto_dismiss) {
    const ms = Math.max(1000, cfg.notify_auto_dismiss_ms || 6000);
    setTimeout(() => dismissCard(card), ms);
  }
}

// Update prompt card. Only one exists at a time — a new update event (prompt →
// "updating…" → done) replaces the previous. Mandatory cards have no dismiss.
function showUpdate(p) {
  document.querySelectorAll(".card.action-update").forEach((c) => c.remove());

  const card = document.createElement("div");
  card.className = "card action-update";

  const row = document.createElement("div");
  row.className = "row";
  const kindEl = document.createElement("span");
  kindEl.className = "kind";
  kindEl.textContent = p.mandatory ? "Update required" : "Update";
  row.appendChild(kindEl);
  // Mandatory + in-progress cards can't be dismissed.
  if (!p.mandatory && p.status !== "updating") {
    const closeBtn = document.createElement("button");
    closeBtn.className = "close";
    closeBtn.type = "button";
    closeBtn.setAttribute("aria-label", "Dismiss");
    closeBtn.textContent = "×";
    closeBtn.addEventListener("click", () => dismissCard(card));
    row.appendChild(closeBtn);
  }

  const t = document.createElement("div");
  t.className = "title";
  t.textContent = p.title || "Update";
  const b = document.createElement("div");
  b.className = "body";
  b.textContent = p.body || "";
  card.append(row, t, b);

  // The prompt (not the progress / done states) carries the action buttons.
  if (p.status !== "updating" && p.status !== "done") {
    const actions = document.createElement("div");
    actions.className = "actions";
    const up = document.createElement("button");
    up.className = "btn btn-accept";
    up.textContent = "Update now";
    up.addEventListener("click", () => {
      sendDaemon({ cmd: "self-update" });
      up.disabled = true;
      up.textContent = "Updating…";
    });
    actions.appendChild(up);
    if (!p.mandatory) {
      const later = document.createElement("button");
      later.className = "btn";
      later.textContent = "Later";
      later.addEventListener("click", () => dismissCard(card));
      actions.appendChild(later);
    }
    card.appendChild(actions);
  }

  stack.prepend(card);
  stack.scrollTop = 0;
  revealWindow();
}

// Incoming friend request card: Accept → friend-accept, Decline → friend-remove
// (both resolve by username server-side).
function showFriendRequest(p) {
  const username = p.username || "?";
  const card = document.createElement("div");
  card.className = "card action-friend_request";

  const row = document.createElement("div");
  row.className = "row";
  const kindEl = document.createElement("span");
  kindEl.className = "kind";
  kindEl.textContent = "Friend request";
  const fromEl = document.createElement("span");
  fromEl.className = "from";
  fromEl.textContent = username;
  const closeBtn = document.createElement("button");
  closeBtn.className = "close";
  closeBtn.type = "button";
  closeBtn.setAttribute("aria-label", "Dismiss");
  closeBtn.textContent = "×";
  closeBtn.addEventListener("click", () => dismissCard(card));
  row.append(kindEl, fromEl, closeBtn);

  const b = document.createElement("div");
  b.className = "body";
  b.textContent = `${username} wants to add you as a friend.`;

  const actions = document.createElement("div");
  actions.className = "actions";
  const accept = document.createElement("button");
  accept.className = "btn btn-accept";
  accept.textContent = "Accept";
  accept.addEventListener("click", () => {
    sendDaemon({ cmd: "friend-accept", username });
    dismissCard(card);
  });
  const decline = document.createElement("button");
  decline.className = "btn btn-decline";
  decline.textContent = "Decline";
  decline.addEventListener("click", () => {
    sendDaemon({ cmd: "friend-remove", username });
    dismissCard(card);
  });
  actions.append(accept, decline);

  card.append(row, b, actions);
  stack.prepend(card);
  stack.scrollTop = 0;
  revealWindow();
}

function acceptInvite(card, from, url) {
  if (url) {
    try {
      const t = window.__TAURI__;
      const invoke = t?.core?.invoke || t?.invoke;
      // Opens in the OS default browser via tauri-plugin-opener.
      invoke?.("plugin:opener|open_url", { url });
    } catch {}
  }
  sendReply(from, "accepted");
  if (focusedInvite && focusedInvite.card === card) focusedInvite = null;
  dismissCard(card);
}

function declineInvite(card, from) {
  sendReply(from, "declined");
  if (focusedInvite && focusedInvite.card === card) focusedInvite = null;
  dismissCard(card);
}

function sendReply(toUserId, replyText) {
  sendDaemon({
    cmd: "send-action",
    to: toUserId,
    action: "message",
    title: "Reply",
    body: String(replyText),
    data: { reply: true },
  });
}

function wireGlobalKeys() {
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      clearAll();
      return;
    }
    if (!focusedInvite) return;
    if (e.key === "a" || e.key === "A") {
      acceptInvite(focusedInvite.card, focusedInvite.from, focusedInvite.url);
    } else if (e.key === "d" || e.key === "D") {
      declineInvite(focusedInvite.card, focusedInvite.from);
    }
  });
}

function clearAll() {
  while (stack.firstChild) stack.firstChild.remove();
  focusedInvite = null;
  hideWindowIfEmpty();
}

function dismissCard(card) {
  if (!card.isConnected) return;
  card.classList.add("fade");
  card.addEventListener(
    "animationend",
    () => {
      card.remove();
      if (focusedInvite && focusedInvite.card === card) focusedInvite = null;
      if (!stack.firstChild) hideWindowIfEmpty();
    },
    { once: true }
  );
}

function revealWindow() {
  document.documentElement.classList.remove("hidden");
  setWindowVisible(true);
}

function hideWindowIfEmpty() {
  if (!stack.firstChild) {
    document.documentElement.classList.add("hidden");
    setWindowVisible(false);
  }
}

function playSound() {
  if (!cfg.notify_sound) return;
  try {
    const url = toAssetUrl(cfg.notify_sound);
    const audio = new Audio(url);
    audio.volume = 0.7;
    audio.play().catch(() => beep());
  } catch {
    beep();
  }
}

function toAssetUrl(path) {
  const normalized = path.replace(/\\/g, "/");
  return `asset://localhost/${encodeURI(normalized)}`;
}

let audioCtx = null;
function beep() {
  try {
    audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
    const o = audioCtx.createOscillator();
    const g = audioCtx.createGain();
    o.frequency.value = 880;
    g.gain.value = 0.05;
    o.connect(g).connect(audioCtx.destination);
    o.start();
    o.stop(audioCtx.currentTime + 0.08);
  } catch {}
}
