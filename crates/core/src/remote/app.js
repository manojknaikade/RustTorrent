"use strict";
// The remote-control page. Everything shown that comes from a torrent (its name, an
// error) is put in with textContent, never as HTML.

const $ = (id) => document.getElementById(id);
const rows = new Map(); // torrent key -> the parts of its list entry
let timer = null;
let limitsShown = "";
let rulesShown = "";
let slowOn = false;
const RULES = ["slow_down_kib", "slow_up_kib", "ratio_limit_pct", "seed_time_min", "seed_idle_min",
  "max_downloads", "max_seeding", "max_active"];
let categories = [];
const MAX_UPLOAD = 4 * 1024 * 1024; // the same as the app's

async function call(path, body) {
  const options = { credentials: "same-origin", cache: "no-store" };
  if (body !== undefined) {
    options.method = "POST";
    options.headers = { "Content-Type": "application/json", "X-RustTorrent": "1" };
    options.body = JSON.stringify(body);
  }
  const response = await fetch(path, options);
  let data = {};
  try { data = await response.json(); } catch (e) { /* an empty answer */ }
  return { status: response.status, data };
}

function size(bytes) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let n = bytes, i = 0;
  while (n >= 1000 && i < units.length - 1) { n /= 1000; i += 1; }
  return (i === 0 ? n : n.toFixed(n < 10 ? 2 : 1)) + " " + units[i];
}

function show(signedIn) {
  $("login").hidden = signedIn;
  $("app").hidden = !signedIn;
  if (!signedIn) {
    clearTimeout(timer);
    $("password").focus();
  }
}

function say(text) {
  $("message").textContent = text;
}

async function command(body) {
  const { status, data } = await call("/api/command", body);
  if (status === 401) { show(false); return false; }
  if (status !== 200) { say(data.error || "That did not work."); return false; }
  refresh();
  return true;
}

function makeRow(key) {
  const li = document.createElement("li");
  li.className = "card";
  const name = document.createElement("div");
  name.className = "name";
  const bar = document.createElement("progress");
  bar.max = 1;
  const facts = document.createElement("div");
  facts.className = "facts muted";
  const actions = document.createElement("div");
  actions.className = "actions";
  const toggle = document.createElement("button");
  const files = document.createElement("button");
  files.textContent = "Files";
  const category = document.createElement("button");
  category.textContent = "Category";
  const remove = document.createElement("button");
  remove.textContent = "Remove";
  actions.append(toggle, files, category, remove);
  li.append(name, bar, facts, actions);

  const row = { li, name, bar, facts, toggle, files, paused: false, category: "" };
  files.addEventListener("click", () => openFiles(key));
  category.addEventListener("click", () => openCategory(key, row));
  toggle.addEventListener("click", () => {
    command({ do: row.paused ? "resume" : "pause", keys: [key] });
  });
  remove.addEventListener("click", () => {
    const dialog = $("remove-dialog");
    $("remove-name").textContent = row.name.textContent;
    $("remove-files").checked = false;
    dialog.returnValue = "cancel";
    dialog.onclose = () => {
      if (dialog.returnValue === "remove") {
        command({ do: "remove", keys: [key], delete_files: $("remove-files").checked });
      }
    };
    dialog.showModal();
  });
  return row;
}

function draw(state) {
  $("speeds").textContent = "↓ " + size(state.down_bps) + "/s   ↑ " + size(state.up_bps) + "/s";
  const banner = $("banner");
  const blocked = state.blocked || (state.schedule_off ? "The schedule has stopped all transfers for now." : "");
  banner.hidden = !blocked;
  banner.textContent = blocked;

  const list = $("list");
  const seen = new Set();
  for (const t of state.torrents) {
    seen.add(t.key);
    let row = rows.get(t.key);
    if (!row) {
      row = makeRow(t.key);
      rows.set(t.key, row);
    }
    list.append(row.li); // also puts it in the right place
    row.name.textContent = t.name;
    row.bar.value = t.total > 0 ? t.done / t.total : 0;
    row.paused = t.state === "Paused";
    row.toggle.textContent = row.paused ? "Resume" : "Pause";
    row.files.hidden = t.files < 2;
    row.category = t.category || "";

    const percent = t.total > 0 ? (100 * t.done / t.total).toFixed(1) + "%" : "";
    const parts = [t.state, percent, t.total > 0 ? size(t.total) : ""];
    if (t.down_bps > 0) parts.push("↓ " + size(t.down_bps) + "/s");
    if (t.up_bps > 0) parts.push("↑ " + size(t.up_bps) + "/s");
    if (t.down_bps > 0 && t.eta !== "–") parts.push(t.eta + " left");
    if (t.peers > 0 || t.peers_seen > 0) parts.push(t.peers + " of " + t.peers_seen + " peers");
    if (t.ratio !== "–") parts.push("ratio " + t.ratio);
    if (t.category) parts.push(t.category);
    row.facts.textContent = parts.filter(Boolean).join(" · ");
    if (t.error) {
      const bad = document.createElement("div");
      bad.className = "bad";
      bad.textContent = t.error;
      row.facts.append(bad);
    }
  }
  for (const [key, row] of rows) {
    if (!seen.has(key)) {
      row.li.remove();
      rows.delete(key);
    }
  }
  $("empty").hidden = state.torrents.length > 0;

  // Fill the limit boxes when the limits change, not while somebody is typing in them.
  const limits = state.down_limit_kib + "/" + state.up_limit_kib;
  if (limits !== limitsShown) {
    limitsShown = limits;
    $("limit-down").value = state.down_limit_kib;
    $("limit-up").value = state.up_limit_kib;
  }
  const rules = JSON.stringify(state.rules || {});
  if (state.rules && rules !== rulesShown) {
    rulesShown = rules;
    for (const name of RULES) $("rule-" + name).value = state.rules[name];
  }
  slowOn = !!(state.slow && state.slow.on);
  const slow = $("slow");
  slow.setAttribute("aria-pressed", String(slowOn));
  slow.textContent = slowOn ? (state.slow.by_schedule ? "Slow mode on (schedule)" : "Slow mode on") : "Slow mode";
  $("footer").textContent = "RustTorrent " + state.version;
  categories = state.categories || [];
}

async function openFiles(key) {
  const { status, data } = await call("/api/files/" + encodeURIComponent(key));
  if (status === 401) { show(false); return; }
  if (status !== 200) { say(data.error || "That did not work."); return; }
  const dialog = $("files-dialog");
  const list = $("files-list");
  list.replaceChildren();
  $("files-name").textContent = data.name;
  const boxes = [];
  data.files.forEach((f, i) => {
    const label = document.createElement("label");
    label.className = "check";
    const box = document.createElement("input");
    box.type = "checkbox";
    box.checked = f.chosen;
    boxes.push([i, box]);
    const text = document.createElement("span");
    text.className = "file";
    text.textContent = f.name;
    const facts = document.createElement("span");
    facts.className = "muted";
    facts.textContent = size(f.len) + (f.len > 0 ? " · " + Math.floor(100 * f.done / f.len) + "%" : "");
    label.append(box, text, facts);
    list.append(label);
  });
  dialog.returnValue = "cancel";
  dialog.onclose = () => {
    if (dialog.returnValue !== "save") return;
    const chosen = boxes.filter(([, box]) => box.checked).map(([i]) => i);
    if (chosen.length === 0) { say("Choose at least one file."); return; }
    command({ do: "files", key, files: chosen }).then((ok) => { if (ok) say("Files chosen."); });
  };
  dialog.showModal();
}

function openCategory(key, row) {
  const dialog = $("category-dialog");
  $("category-name").textContent = row.name.textContent;
  const known = $("category-known");
  known.replaceChildren();
  for (const name of categories) {
    const option = document.createElement("option");
    option.value = name;
    known.append(option);
  }
  $("category-text").value = row.category;
  dialog.returnValue = "cancel";
  dialog.onclose = () => {
    if (dialog.returnValue !== "save") return;
    const text = $("category-text").value.trim();
    command({ do: "category", keys: [key], category: text || null });
  };
  dialog.showModal();
}

async function refresh() {
  clearTimeout(timer);
  try {
    const { status, data } = await call("/api/state");
    if (status === 401) { show(false); return; }
    if (status === 200) {
      show(true);
      draw(data);
    }
  } catch (e) {
    say("RustTorrent is not answering.");
  }
  timer = setTimeout(refresh, document.hidden ? 10000 : 2000);
}

$("login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const error = $("login-error");
  error.textContent = "";
  try {
    const { status, data } = await call("/api/login", { password: $("password").value });
    if (status === 200) {
      $("password").value = "";
      say("");
      refresh();
    } else {
      error.textContent = data.error || "That did not work.";
    }
  } catch (e) {
    error.textContent = "RustTorrent is not answering.";
  }
});

$("sign-out").addEventListener("click", async () => {
  await call("/api/logout", {});
  show(false);
});

$("add-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const box = $("add-text");
  if (await command({ do: "add", text: box.value.trim() })) {
    box.value = "";
    say("Added.");
  }
});

$("upload").addEventListener("change", async () => {
  const input = $("upload");
  const file = input.files[0];
  input.value = "";
  if (!file) return;
  if (file.size > MAX_UPLOAD) { say("That file is too large to be a torrent."); return; }
  say("Sending " + file.name + "…");
  try {
    const response = await fetch("/api/upload", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      headers: { "Content-Type": "application/x-bittorrent", "X-RustTorrent": "1" },
      body: file,
    });
    let data = {};
    try { data = await response.json(); } catch (e) { /* an empty answer */ }
    if (response.status === 401) { show(false); return; }
    if (response.status !== 200) { say(data.error || "That did not work."); return; }
    say("Added " + data.name + ".");
    refresh();
  } catch (e) {
    say("RustTorrent is not answering.");
  }
});

// Cancel is not a submit button, so that Enter in the box saves.
$("category-cancel").addEventListener("click", () => $("category-dialog").close("cancel"));

$("limits-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const number = (id) => Math.max(0, Math.floor(Number($(id).value) || 0));
  if (await command({ do: "limits", down_kib: number("limit-down"), up_kib: number("limit-up") })) {
    say("Limits set.");
  }
});

$("rules-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const body = { do: "rules" };
  for (const name of RULES) body[name] = Math.max(0, Math.floor(Number($("rule-" + name).value) || 0));
  if (await command(body)) say("Rules set.");
});

$("slow").addEventListener("click", () => {
  command({ do: "slow_mode", on: !slowOn });
});

document.addEventListener("visibilitychange", () => {
  if (!document.hidden && !$("app").hidden) refresh();
});

refresh();
