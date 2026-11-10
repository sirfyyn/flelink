// The chat while FLE Durable answers: its one durable conversation, live, with approvals and stop. Everything goes through /api/p/agent/…;
// the switch next to the title goes to the direct chat with the model (chat.js).

import { get, hostUrl, post } from "./api.js";
import { engineUp, on, store, topbar } from "./app.js";
import { renderMarkdown } from "./md.js";
import { clear, copyText, h, icon, mark, toast } from "./ui.js";

const TOKEN = document.querySelector('meta[name="fle-token"]')?.content || "";
const API = "/api/p/agent";

const blocks = (m) => (Array.isArray(m?.content) ? m.content : typeof m?.content === "string" ? [{ type: "text", text: m.content }] : []);
const textOf = (m) => blocks(m).filter((b) => b.type === "text").map((b) => b.text).join("");

export function mountAgent(main, { seg }) {
  let alive = true;
  let entries = [];
  let liveMsg = null; // the assistant message being written
  let working = false;
  let pinned = true;
  let ctrl = null;
  let approvals = [];
  let remote = true; // until the status says otherwise: no buttons that could not work
  let ownAgent = false; // FLE Link started it, so it answers with the model of the Engine page

  const inner = h("div", { class: "messages-inner" });
  const messages = h("div", { class: "messages" }, inner);
  messages.addEventListener("scroll", () => { pinned = messages.scrollTop + messages.clientHeight >= messages.scrollHeight - 40; });
  const ta = h("textarea", { rows: 1, placeholder: "Write to your agent …", "aria-label": "Message to the agent" });
  const sendBtn = h("button", { class: "send", "aria-label": "Send", title: "Send (Enter)" }, icon("send"));
  const stopBtn = h("button", { class: "btn ghost sm", hidden: true, onclick: stop }, icon("stop", "sm"), "Stop");
  const status = h("span", { class: "pill" });
  const composer = h("div", { class: "composer" }, ta, h("div", { class: "bar-row" }, status, h("span", { class: "grow" }), stopBtn, sendBtn));
  const note = h("div", { class: "composer-note" }, "Your agent keeps one conversation. It also works from its agenda and from webhooks; those show up here.");
  const approvalBox = h("div", { style: { display: "grid", gap: "8px" } });
  const modelNote = h("div", { class: "callout warn", hidden: true }, h("div", null,
    "The agent answers with the model of the Engine page, and none is loaded. ",
    h("a", { href: "/engine", "data-link": true }, "Load a model"), ", then write to it."));

  main.replaceChildren(
    topbar(h("h1", null, "FLE Durable"), seg),
    h("div", { class: "chat" }, h("div", { class: "chat-col" }, messages, h("div", { class: "composer-wrap" }, approvalBox, modelNote, composer, note))));

  const autosize = () => { ta.style.height = "auto"; ta.style.height = `${Math.min(ta.scrollHeight, 240)}px`; };
  ta.addEventListener("input", autosize);
  ta.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
  });
  sendBtn.addEventListener("click", send);

  // FLE Link's own agent answers with the model of the Engine page: without one it takes the message and waits
  const noModel = () => ownAgent && !engineUp();
  const loadingModel = () => store.engine?.state === "starting";

  function setWorking(on) {
    working = on;
    stopBtn.hidden = !on;
    if (!on && noModel()) {
      clear(status, h("span", { class: "dot" }), loadingModel() ? "The model is loading" : "No model is loaded");
      status.className = "pill warn";
    } else {
      clear(status, h("span", { class: "dot" }), on ? "Working" : "Ready");
      status.className = on ? "pill info live" : "pill good";
    }
    modelNote.hidden = !noModel() || loadingModel();
  }

  // who decides approvals (the buttons work only for FLE Link's own agent) and whether the engine's model is ours
  async function readStatus() {
    try {
      const s = await get(`${API}/status`);
      if (!alive) return;
      const was = remote;
      remote = !!s.remote;
      ownAgent = !s.remote;
      setWorking(working);
      if (was !== remote) refreshApprovals();
    } catch { /* the next round asks again */ }
  }

  async function send() {
    const text = ta.value.trim();
    if (!text) return;
    // without a model the agent tries three times and gives up: the message would get no answer, now or later
    if (noModel()) {
      toast(loadingModel() ? "The model is still loading; send it once it is ready." : "No model is loaded. Load one on the Engine page first.", "bad");
      return;
    }
    ta.value = "";
    autosize();
    pinned = true;
    try {
      await post(`${API}/message`, { text });
    } catch (e) {
      ta.value = text;
      toast(e.message, "bad");
    }
  }

  async function stop() {
    try { await post(`${API}/abort`, {}); } catch (e) { toast(e.message, "bad"); }
  }

  function userEl(text) {
    return h("div", { class: "msg user" }, h("div", { class: "bubble" }, text));
  }

  function noteEl(text) {
    return h("div", { class: "small faint", style: { textAlign: "center" } }, text);
  }

  // a turn the model server refused or broke off: the reason, short (its JSON error body is unwrapped)
  function errorText(m) {
    let msg = String(m.errorMessage || "");
    const j = msg.match(/\{.*\}/s);
    if (j) { try { const v = JSON.parse(j[0]); msg = v.message || v.error?.message || msg; } catch { /* as it came */ } }
    return msg.slice(0, 300);
  }
  function errorEl(text) {
    return h("div", { class: "callout bad small" }, h("b", null, "The model did not answer. "), h("span", { "data-no-i18n": true }, text));
  }

  function assistantEl(m, isLive) {
    const body = h("div", { class: "body", style: { display: "grid", gap: "8px", minWidth: 0 } }, h("div", { class: "who" }, "Agent"));
    for (const b of blocks(m)) {
      if (b.type === "thinking" && b.thinking) {
        body.append(h("details", { class: "small faint" }, h("summary", null, "Thinking"), h("div", { style: { whiteSpace: "pre-wrap" } }, b.thinking)));
      } else if (b.type === "text" && b.text) {
        const md = h("div", { class: "md" });
        md.innerHTML = renderMarkdown(b.text) + (isLive ? '<span class="cursor"></span>' : "");
        body.append(md);
      } else if (b.type === "toolCall") {
        body.append(h("div", { class: "small faint mono", style: { overflowWrap: "anywhere" } }, `${b.name} ${JSON.stringify(b.arguments ?? {}).slice(0, 300)}`));
      }
    }
    const text = textOf(m);
    if (text && !isLive) {
      body.append(h("div", { class: "foot" }, h("div", { class: "actions" },
        h("button", { class: "btn ghost icon sm", title: "Copy", "aria-label": "Copy", onclick: (e) => copyText(text, e.currentTarget) }, icon("copy", "sm")))));
    }
    return h("div", { class: "msg assistant" }, h("div", { class: "avatar" }, mark(20)), body);
  }

  function render() {
    const out = [];
    let lastError = null;
    for (const e of entries) {
      if (e.kind === "fle.event") {
        const d = e.data ?? {};
        out.push(d.source === "chat" ? userEl(d.text) : noteEl(`${d.source}${d.from && d.from !== d.source ? ` · ${d.from}` : ""}: ${d.text}`));
      } else if (e.kind === "fyos.mensch") { // Ginny (the Runner) keeps its own entries
        out.push(userEl(e.data?.text ?? ""));
      } else if (e.kind === "fyos.eingang") {
        out.push(noteEl(e.data?.text ?? ""));
      } else if (e.kind === "fle.approval") {
        out.push(noteEl(`Asked: ${e.data?.action}`));
      } else if (e.kind === "fle.approval-decision") {
        out.push(noteEl(`Approval: ${e.data?.decision}${e.data?.note ? ` · ${e.data.note}` : ""}`));
      } else {
        for (const m of Array.isArray(e.model) ? e.model : e.model ? [e.model] : []) {
          if (m.role === "assistant" && blocks(m).length) out.push(assistantEl(m, false));
          // the agent tries a failed request again; the same reason is shown once
          else if (m.role === "assistant" && m.stopReason === "error" && m.errorMessage) {
            const text = errorText(m);
            if (lastError !== text) out.push(errorEl(text));
            lastError = text;
            continue;
          }
          lastError = null;
        }
      }
    }
    if (liveMsg && blocks(liveMsg).length) out.push(assistantEl(liveMsg, true));
    if (!out.length) {
      out.push(h("div", { class: "welcome" }, h("div", { class: "hero-mark", style: { width: "44px", height: "44px" } }, mark(44)),
        h("h2", { "data-i18n": "What should {your agent} work on?" }, "What should ", h("span", { class: "grad" }, "your agent"), " work on?"),
        h("p", { class: "lead" }, "It remembers this conversation across restarts and can plan its own follow-ups.")));
    }
    inner.replaceChildren(...out);
    if (pinned) messages.scrollTop = messages.scrollHeight;
  }

  async function reload() {
    try {
      entries = (await get(`${API}/conversation`)).entries ?? [];
      render();
    } catch { /* the stream reconnects and comes back here */ }
  }

  async function refreshApprovals() {
    if (!alive) return;
    const decide = (a, decision) => post(`${API}/approvals/${a.id}`, { decision }).then(refreshApprovals).catch((e) => toast(e.message, "bad"));
    try {
      approvals = ((await get(`${API}/approvals`)).approvals ?? []).filter((a) => a.status === "open");
    } catch { approvals = []; }
    // a connected Durable decides approvals itself (Ginny: in the FYOS app); here they are only shown
    clear(approvalBox, ...approvals.map((a) => h("div", { class: "callout warn", style: { display: "grid", gap: "6px" } },
      h("b", null, a.action), h("div", null, a.context), a.command ? h("pre", { class: "mono small", style: { whiteSpace: "pre-wrap" } }, a.command) : null,
      remote ? h("div", { class: "small" }, "Waiting for your decision in the agent's own app.")
        : h("div", { class: "row" }, h("button", { class: "btn sm", onclick: () => decide(a, "refused") }, "Refuse"),
          h("button", { class: "btn primary sm", onclick: () => decide(a, "granted") }, "Allow")))));
  }

  function onEvents(list) {
    let finished = false;
    for (const ev of list) {
      if (ev.type === "message_start" && ev.message?.role === "assistant") liveMsg = structuredClone(ev.message);
      if (ev.type === "message_update" && liveMsg) {
        for (const c of ev.changes ?? []) {
          if (c.type === "message") liveMsg = structuredClone(c.message);
          if (c.type === "text_start") (liveMsg.content ??= [])[c.contentIndex] = { type: "text", text: "" };
          if (c.type === "text_delta" && liveMsg.content?.[c.contentIndex]) liveMsg.content[c.contentIndex].text += c.delta ?? "";
        }
      }
      if (ev.type === "message_end" || ev.type === "entry_appended") {
        const entry = ev.entry;
        if (entry && !entries.some((x) => x.id === entry.id)) entries.push(entry);
        if (ev.type === "message_end" && entry?.model?.[0]?.role === "assistant") liveMsg = null;
      }
      if (ev.type === "run_start") setWorking(true);
      if (ev.type === "run_end") { setWorking(false); finished = true; }
    }
    render();
    if (finished) refreshApprovals();
  }

  function handle(event, data) {
    let v;
    try { v = JSON.parse(data); } catch { return; }
    if (event === "snapshot") { entries = v.entries ?? []; render(); reload(); }
    if (event === "events") onEvents(v);
  }

  async function stream() {
    while (alive) {
      ctrl = new AbortController();
      try {
        const res = await fetch(hostUrl(`${API}/stream`), { headers: { "X-FLE-Token": TOKEN, Accept: "text/event-stream" }, signal: ctrl.signal });
        if (!res.ok) throw new Error(String(res.status));
        const reader = res.body.getReader();
        const dec = new TextDecoder();
        let buf = "", event = "message", data = "";
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          buf += dec.decode(value, { stream: true });
          let cut;
          while ((cut = buf.indexOf("\n")) >= 0) {
            const line = buf.slice(0, cut).replace(/\r$/, "");
            buf = buf.slice(cut + 1);
            if (line === "") { if (data) handle(event, data); event = "message"; data = ""; }
            else if (line.startsWith("event:")) event = line.slice(6).trim();
            else if (line.startsWith("data:")) data += line.slice(5).trim();
          }
        }
      } catch { /* reconnect below */ }
      if (alive) await new Promise((r) => setTimeout(r, 1000));
    }
  }

  setWorking(false);
  render();
  reload();
  readStatus();
  refreshApprovals();
  stream();
  const poll = setInterval(refreshApprovals, 3000);
  const statusPoll = setInterval(readStatus, 15000);
  const offEngine = on("engine", () => setWorking(working));
  ta.focus();

  return {
    unmount() {
      alive = false;
      clearInterval(poll);
      clearInterval(statusPoll);
      offEngine();
      ctrl?.abort();
    },
  };
}
