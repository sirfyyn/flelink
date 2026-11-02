// Agent: FLE Durable next to the model (crates/flelink-api/src/agent.rs). The agent runs all the time and works from events:
// what you plan here, what it plans for itself, webhooks. This page starts and stops it, shows its agenda by day, what
// waits for it and what it asks you to approve, and plans new events. Everything goes through /api/p/agent/…; the runner's
// own token never reaches the page.

const KINDS = [
  ["in", "In … minutes"],
  ["at", "At a time"],
  ["daily", "Every day"],
  ["weekly", "On certain days"],
  ["interval", "Every … minutes"],
];
const DAYS = [[1, "Mon"], [2, "Tue"], [3, "Wed"], [4, "Thu"], [5, "Fri"], [6, "Sat"], [7, "Sun"]];
const POLL_MS = 4000;

export function mount(main, kit) {
  const { h, clear, icon, toast, confirmDialog, topbar, api } = kit;
  if (!document.getElementById("agent-css")) {
    document.head.append(h("link", { id: "agent-css", rel: "stylesheet", href: kit.asset("agent.css") }));
  }

  let status = null;
  let agenda = null;
  let queue = null;
  let approvals = [];
  let timer = 0;
  let gone = false;
  const plan = { kind: "in", text: "", title: "", minutes: 30, at: "", time: "08:00", days: [1, 2, 3, 4, 5], every: 60, owner: false };

  const statusBox = h("div", { class: "card stack" });
  const setupBox = h("div");
  const planBox = h("div", { class: "card stack" });
  const agendaBox = h("div", { class: "card stack" });
  const sideBox = h("div", { class: "stack" });

  main.replaceChildren(topbar("Agent"), h("div", { class: "page" }, h("div", { class: "page-inner" },
    h("div", { class: "page-head" }, h("div", null, h("h2", null, "Agent"),
      h("p", null, "FLE Durable runs next to your model all the time and works from events: what you plan here, what it plans for "
        + "itself to follow up, and webhooks from your tools. It keeps everything in one file, so a restart neither loses nor "
        + "repeats anything."))),
    statusBox, setupBox,
    h("div", { class: "ag-cols" }, h("div", { class: "stack" }, planBox, agendaBox), sideBox))));

  // ------------------------------------------------------------------------------------------- helpers

  async function busy(btn, fn) {
    if (btn) btn.disabled = true;
    try {
      return await fn();
    } catch (e) {
      toast(e.message, "bad", 8000);
      return null;
    } finally {
      if (btn) btn.disabled = false;
    }
  }

  const field = (label, input, hint) => h("label", { class: "field" }, h("span", null, label), input, hint ? h("span", { class: "hint" }, hint) : null);
  const callout = (kind, ...body) => h("div", { class: `callout ${kind}` }, icon(kind === "info" ? "info" : "alert", "sm"), h("div", null, ...body));
  const pad = (n) => String(n).padStart(2, "0");
  const timeOf = (ms) => { const d = new Date(ms); return `${pad(d.getHours())}:${pad(d.getMinutes())}`; };

  function dayLabel(ms) {
    const d = new Date(ms);
    const start = new Date(); start.setHours(0, 0, 0, 0);
    const diff = Math.round((new Date(d).setHours(0, 0, 0, 0) - start) / 86400000);
    if (diff === 0) return "Today";
    if (diff === 1) return "Tomorrow";
    return d.toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });
  }

  function rule(it) {
    if (it.kind === "once") return "once";
    if (it.kind === "daily") return `every day ${it.time}`;
    if (it.kind === "weekly") return `${it.days.map((n) => DAYS[n - 1][1]).join(", ")} ${it.time}`;
    return `every ${it.every_min} min`;
  }

  const ago = (ms) => {
    const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
    if (s < 60) return `${s} s ago`;
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    return `${Math.round(s / 3600)} h ago`;
  };

  // ------------------------------------------------------------------------------------------- status and setup

  function renderStatus() {
    const s = status;
    if (!s) return clear(statusBox, h("p", { class: "muted" }, "Loading …"));
    const pill = s.running
      ? h("span", { class: "pill good live" }, h("span", { class: "dot" }), "Running")
      : s.installed && s.node.ok ? h("span", { class: "pill" }, h("span", { class: "dot" }), "Stopped")
      : h("span", { class: "pill warn" }, h("span", { class: "dot" }), "Not set up");
    const start = h("button", { class: "btn primary", disabled: !s.installed || !s.node.ok,
      onclick: (e) => busy(e.currentTarget, async () => { status = await api.post("/start"); toast("The agent is running", "good"); refresh(); }) },
      icon("play", "sm"), "Start");
    const stop = h("button", { class: "btn", onclick: (e) => busy(e.currentTarget, async () => { status = await api.post("/stop"); refresh(); }) },
      icon("stop", "sm"), "Stop");
    const restart = h("button", { class: "btn", title: "Restart with the model the engine runs now",
      onclick: (e) => busy(e.currentTarget, async () => { status = await api.post("/restart"); refresh(); }) }, icon("refresh", "sm"), "Restart");
    const chat = h("button", { class: "btn", onclick: (e) => busy(e.currentTarget, async () => {
      const r = await api.post("/chat");
      window.open(r.url, "_blank", "noopener");
    }) }, icon("external", "sm"), "Open its chat");
    const auto = h("label", { class: "check" }, h("span", { class: "switch" },
      h("input", { type: "checkbox", checked: !!s.autostart, onchange: (e) => busy(null, async () => {
        status = await api.post("/setup", { autostart: e.target.checked });
        renderStatus();
      }) }), h("i")), h("span", null, "Start with FLE Link"));
    clear(statusBox,
      h("div", { class: "card-head" }, icon("spark"), h("div", { class: "grow" }, h("h3", null, "FLE Durable"),
        h("p", null, s.running
          ? `Uses the model on the Engine page through FLE Link's own /v1. Port ${s.port}, pid ${s.pid}.`
          : "Uses the model on the Engine page. Start the engine first, then the agent.")), pill),
      h("div", { class: "row wrap" }, s.running ? [stop, restart, chat] : [start], h("span", { class: "grow" }), auto),
      s.error ? callout("bad", s.error) : null,
      s.log?.length ? h("details", { class: "ag-log" }, h("summary", null, "Log"), h("pre", { class: "mono" }, s.log.join("\n"))) : null);
  }

  function renderSetup() {
    const s = status;
    if (!s || (s.installed && s.node.ok)) return clear(setupBox);
    const form = { runner_dir: s.runner_dir || "", node: "", language: s.language || "en" };
    const dir = h("input", { class: "input mono", value: form.runner_dir, spellcheck: "false", placeholder: "~/fle-durable",
      oninput: (e) => { form.runner_dir = e.target.value; } });
    const node = h("input", { class: "input mono", value: "", spellcheck: "false", placeholder: s.node.ok ? s.node.path : "empty: node from the PATH",
      oninput: (e) => { form.node = e.target.value; } });
    const lang = h("select", { class: "select", onchange: (e) => { form.language = e.target.value; } },
      [["en", "English"], ["de", "Deutsch"]].map(([v, l]) => h("option", { value: v, selected: v === form.language }, l)));
    const save = h("button", { class: "btn primary", onclick: (e) => busy(e.currentTarget, async () => {
      status = await api.post("/setup", form);
      renderStatus();
      renderSetup();
    }) }, icon("check", "sm"), "Save");
    clear(setupBox, h("div", { class: "card stack" },
      h("div", { class: "card-head" }, icon("wrench"), h("div", { class: "grow" }, h("h3", null, "Set up"),
        h("p", null, "The agent is FLE Durable, a Node program. FLE Link starts it and gives it your model."))),
      s.node.ok ? callout("info", `Node ${s.node.version} found at ${s.node.path}.`) : callout("warn", s.node.error),
      s.problem ? callout("warn", s.problem) : null,
      h("div", { class: "grid two" },
        field("FLE Durable folder", dir, "The folder with src/durable.js; inside it once: npm ci --ignore-scripts"),
        field("Node (optional)", node, "Node 22.19 or newer")),
      h("div", { class: "grid two" }, field("Language of the agent", lang)),
      h("div", { class: "row" }, save)));
  }

  // ------------------------------------------------------------------------------------------- plan

  function renderPlan() {
    const off = !status?.running;
    const text = h("textarea", { class: "textarea", rows: 3, placeholder: "What should the agent do? E.g. “Look at the nightly build and tell me what broke.”",
      oninput: (e) => { plan.text = e.target.value; } });
    text.value = plan.text;
    const kindSel = field("When", h("select", { class: "select", onchange: (e) => { plan.kind = e.target.value; renderPlan(); } },
      KINDS.map(([v, l]) => h("option", { value: v, selected: v === plan.kind }, l))));
    const num = (key, min) => h("input", { class: "input", type: "number", min, value: plan[key], oninput: (e) => { plan[key] = Number(e.target.value); } });
    let when = null;
    if (plan.kind === "in") when = field("Minutes from now", num("minutes", 1));
    if (plan.kind === "at") when = field("Date and time", h("input", { class: "input", type: "datetime-local", value: plan.at, oninput: (e) => { plan.at = e.target.value; } }));
    if (plan.kind === "daily" || plan.kind === "weekly") {
      when = field("Time", h("input", { class: "input", type: "time", value: plan.time, oninput: (e) => { plan.time = e.target.value; } }));
    }
    if (plan.kind === "interval") when = field("Every … minutes (5 or more)", num("every", 5));
    const days = plan.kind === "weekly" ? h("div", { class: "row wrap" }, DAYS.map(([n, l]) => h("button", {
      class: `chip${plan.days.includes(n) ? " on" : ""}`, type: "button",
      onclick: () => { plan.days = plan.days.includes(n) ? plan.days.filter((x) => x !== n) : [...plan.days, n].sort(); renderPlan(); } }, l))) : null;
    const owner = h("label", { class: "check" }, h("span", { class: "switch" },
      h("input", { type: "checkbox", checked: plan.owner, onchange: (e) => { plan.owner = e.target.checked; } }), h("i")),
      h("span", null, "Right of way (may interrupt background work)"));
    const go = h("button", { class: "btn primary", disabled: off, onclick: (e) => busy(e.currentTarget, async () => {
      const body = { text: plan.text, title: plan.title || undefined, priority: plan.owner ? "owner" : undefined };
      if (plan.kind === "in") Object.assign(body, { kind: "once", in_minutes: plan.minutes });
      if (plan.kind === "at") Object.assign(body, { kind: "once", at: plan.at ? new Date(plan.at).toISOString() : "" });
      if (plan.kind === "daily") Object.assign(body, { kind: "daily", time: plan.time });
      if (plan.kind === "weekly") Object.assign(body, { kind: "weekly", time: plan.time, days: plan.days });
      if (plan.kind === "interval") Object.assign(body, { kind: "interval", every_min: plan.every });
      const r = await api.post("/agenda", body);
      toast(`Planned for ${dayLabel(r.item.next).toLowerCase()} ${timeOf(r.item.next)}`, "good");
      plan.text = "";
      plan.title = "";
      await refresh();
      renderPlan();
    }) }, icon("plus", "sm"), "Plan");
    clear(planBox,
      h("div", { class: "card-head" }, icon("plus"), h("div", { class: "grow" }, h("h3", null, "Plan something"),
        h("p", null, "The agent gets it as an event when it is due, also after a restart. It plans its own follow-ups the same way."))),
      text, h("div", { class: "grid two" }, kindSel, when), days,
      field("Title (optional)", h("input", { class: "input", value: plan.title, oninput: (e) => { plan.title = e.target.value; } })), h("div", { class: "row wrap" }, owner, h("span", { class: "grow" }), go),
      off ? h("p", { class: "hint" }, "Start the agent to plan.") : null);
  }

  // ------------------------------------------------------------------------------------------- agenda

  function itemRow(it) {
    const who = it.by === "agent" ? h("span", { class: "pill info" }, icon("spark", "xs"), "planned by the agent") : h("span", { class: "pill" }, "you");
    const pause = h("button", { class: "btn icon", title: it.paused ? "Resume" : "Pause",
      onclick: (e) => busy(e.currentTarget, async () => { await api.post(`/agenda/${it.id}/pause`, { paused: !it.paused }); refresh(); }) },
      icon(it.paused ? "play" : "pause", "sm"));
    const remove = h("button", { class: "btn icon", title: "Remove", onclick: (e) => busy(e.currentTarget, async () => {
      if (!(await confirmDialog({ title: "Remove this item?", body: it.title, ok: "Remove", danger: true }))) return;
      await api.del(`/agenda/${it.id}`);
      refresh();
    }) }, icon("trash", "sm"));
    return h("div", { class: `ag-item${it.paused ? " paused" : ""}` },
      h("div", { class: "ag-time mono" }, it.next ? timeOf(it.next) : "–"),
      h("div", { class: "grow ag-body" }, h("div", { class: "ag-title" }, it.title),
        h("div", { class: "row wrap ag-meta" }, h("span", { class: "tag" }, rule(it)), who, it.paused ? h("span", { class: "pill warn" }, "paused") : null),
        it.text !== it.title ? h("div", { class: "ag-text faint" }, it.text) : null),
      h("div", { class: "row" }, pause, remove));
  }

  function renderAgenda() {
    const head = h("div", { class: "card-head" }, icon("grid"), h("div", { class: "grow" }, h("h3", null, "Agenda"),
      h("p", null, agenda ? `Times in ${agenda.zone}.` : "What is planned, by day.")));
    if (!status?.running) return clear(agendaBox, head, h("div", { class: "empty" }, "The agenda shows while the agent runs."));
    if (!agenda) return clear(agendaBox, head, h("p", { class: "muted" }, "Loading …"));
    const active = agenda.items.filter((x) => !x.paused && x.next);
    const paused = agenda.items.filter((x) => x.paused || !x.next);
    const groups = new Map();
    for (const it of active) {
      const k = dayLabel(it.next);
      if (!groups.has(k)) groups.set(k, []);
      groups.get(k).push(it);
    }
    clear(agendaBox, head,
      agenda.items.length ? null : h("div", { class: "empty" }, "Nothing planned yet. Plan something above, or ask the agent to follow up on a task."),
      [...groups].map(([day, items]) => h("div", { class: "ag-day" }, h("div", { class: "ag-day-head" }, day), items.map(itemRow))),
      paused.length ? h("div", { class: "ag-day" }, h("div", { class: "ag-day-head" }, "Paused"), paused.map(itemRow)) : null);
  }

  // ------------------------------------------------------------------------------------------- queue, approvals, history, webhook

  function renderSide() {
    if (!status?.running) return clear(sideBox);
    const waiting = queue?.eintraege ?? [];
    const open = approvals.filter((f) => f.status === "offen");
    const decide = (f, entscheidung) => (e) => busy(e.currentTarget, async () => {
      await api.post(`/approvals/${encodeURIComponent(f.id)}`, { entscheidung });
      toast(entscheidung === "erteilt" ? "Allowed" : "Refused", "good");
      refresh();
    });
    const hookUrl = `${location.origin}/api/p/agent/events`;
    clear(sideBox,
      open.length ? h("div", { class: "card stack ag-approvals" },
        h("div", { class: "card-head" }, icon("alert"), h("div", { class: "grow" }, h("h3", null, "Asks you"),
          h("p", null, "Only destructive steps wait for you."))),
        open.map((f) => h("div", { class: "ag-approval stack" },
          h("div", { class: "ag-title" }, f.aktion),
          f.kontext ? h("p", { class: "faint" }, f.kontext) : null,
          f.befehl ? h("pre", { class: "mono ag-cmd" }, f.befehl) : null,
          h("div", { class: "row" }, h("button", { class: "btn primary", onclick: decide(f, "erteilt") }, "Allow"),
            h("button", { class: "btn", onclick: decide(f, "abgelehnt") }, "Refuse"))))) : null,
      h("div", { class: "card stack" },
        h("div", { class: "card-head" }, icon("layers"), h("div", { class: "grow" }, h("h3", null, "Waiting"),
          h("p", null, waiting.length ? `${waiting.length} event${waiting.length === 1 ? "" : "s"} for the agent` : "Nothing waits; the agent is up to date."))),
        waiting.map((e) => h("div", { class: "ag-event" }, h("div", { class: "row" }, h("span", { class: "tag" }, e.quelle),
          e.klasse === "max" ? h("span", { class: "pill info" }, "right of way") : null, h("span", { class: "grow" }), h("span", { class: "faint" }, ago(e.angenommen))),
          h("div", { class: "faint ag-text" }, e.text)))),
      h("div", { class: "card stack" },
        h("div", { class: "card-head" }, icon("refresh"), h("div", { class: "grow" }, h("h3", null, "Fired recently"))),
        (agenda?.history ?? []).length
          ? agenda.history.slice(0, 8).map((x) => h("div", { class: "row ag-hist" }, h("span", { class: "mono faint" }, timeOf(x.fired)),
            h("span", { class: "grow" }, x.title), x.by === "agent" ? h("span", { class: "tag" }, "agent") : null))
          : h("p", { class: "faint" }, "Nothing yet.")),
      h("div", { class: "card stack" },
        h("div", { class: "card-head" }, icon("link"), h("div", { class: "grow" }, h("h3", null, "Webhook"),
          h("p", null, "Any tool can hand the agent an event. The same id twice counts once."))),
        h("pre", { class: "mono ag-cmd" }, `curl -X POST ${hookUrl} \\\n  -H 'X-Agent-Key: ${status.webhook_key}' \\\n  -H 'Content-Type: application/json' \\\n  -d '{"id":"ci-42","von":"ci","text":"Build 42 failed on main"}'`),
        h("p", { class: "hint" }, "The key is this agent's own. From another computer add the access key as Bearer token.")));
  }

  // ------------------------------------------------------------------------------------------- data

  async function refresh() {
    try {
      status = await api.get("/status");
      if (status.running) {
        [agenda, queue, approvals] = await Promise.all([
          api.get("/agenda").catch(() => agenda),
          api.get("/queue").catch(() => queue),
          api.get("/approvals").then((r) => r.freigaben ?? []).catch(() => approvals),
        ]);
      } else {
        agenda = null;
        queue = null;
        approvals = [];
      }
    } catch (e) {
      toast(e.message, "bad");
    }
    if (gone) return;
    renderStatus();
    renderSetup();
    renderAgenda();
    renderSide();
  }

  async function loop() {
    await refresh();
    if (!gone) timer = setTimeout(loop, POLL_MS);
  }

  renderStatus();
  renderPlan();
  loop().then(() => renderPlan());
  return { unmount() { gone = true; clearTimeout(timer); } };
}
