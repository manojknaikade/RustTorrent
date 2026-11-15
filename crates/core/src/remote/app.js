"use strict";
// The remote-control page. Everything shown that comes from a torrent (its name, an
// error) is put in with textContent, never as HTML.

const $ = (id) => document.getElementById(id);
const rows = new Map(); // torrent key -> the parts of its list entry
let timer = null;
let limitsShown = "";

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
  const remove = document.createElement("button");
  remove.textContent = "Remove";
  actions.append(toggle, remove);
  li.append(name, bar, facts, actions);

  const row = { li, name, bar, facts, toggle, paused: false };
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

    const percent = t.total > 0 ? (100 * t.done / t.total).toFixed(1) + "%" : "";
    const parts = [t.state, percent, t.total > 0 ? size(t.total) : ""];
    if (t.down_bps > 0) parts.push("↓ " + size(t.down_bps) + "/s");
    if (t.up_bps > 0) parts.push("↑ " + size(t.up_bps) + "/s");
    if (t.peers > 0) parts.push(t.peers + (t.peers === 1 ? " peer" : " peers"));
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
  $("footer").textContent = "RustTorrent " + state.version;
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

$("limits-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const number = (id) => Math.max(0, Math.floor(Number($(id).value) || 0));
  if (await command({ do: "limits", down_kib: number("limit-down"), up_kib: number("limit-up") })) {
    say("Limits set.");
  }
});

document.addEventListener("visibilitychange", () => {
  if (!document.hidden && !$("app").hidden) refresh();
});

refresh();
