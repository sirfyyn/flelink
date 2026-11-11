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
const TICKET_STATES = [["new", "New"], ["triage", "To sort"], ["in_progress", "In progress"], ["waiting_approval", "Waiting for you"], ["resolved", "Resolved"], ["closed", "Closed"]];

export function mount(main, kit) {
  const { h, clear, icon, toast, confirmDialog, topbar, api } = kit;
  if (!document.getElementById("agent-css")) {
    document.head.append(h("link", { id: "agent-css", rel: "stylesheet", href: kit.asset("agent.css") }));
  }

  let status = null;
  let agenda = null;
  let queue = null;
  let approvals = [];
  let agendaOff = false; // a connected agent can run without one (503)
  let timer = 0;
  let gone = false;
  const plan = { kind: "in", text: "", title: "", minutes: 30, at: "", time: "08:00", days: [1, 2, 3, 4, 5], every: 60, owner: false };

  const statusBox = h("div", { class: "card stack" });
  const setupBox = h("div");
  const planBox = h("div", { class: "card stack" });
  const agendaBox = h("div", { class: "card stack" });
  const sideBox = h("div", { class: "stack" });
  const workBox = h("div", { class: "card stack" });
  const work = { tab: "tickets", tickets: [], calendar: [], unreachable: [], events: [], off: false };
  let workSig = "";
  let planRunning = false;
  const deskBox = h("div", { class: "card stack" });
  let desk = null;

  main.replaceChildren(topbar("Agent"), h("div", { class: "page" }, h("div", { class: "page-inner" },
    h("div", { class: "page-head" }, h("div", null, h("h2", null, "Agent"),
      h("p", null, "FLE Durable runs next to your model all the time and works from events: what you plan here, what it plans for "
        + "itself to follow up, and webhooks from your tools. It keeps everything in one file, so a restart neither loses nor "
        + "repeats anything."))),
    statusBox, setupBox, deskBox,
    h("div", { class: "ag-cols" }, h("div", { class: "stack" }, planBox, agendaBox), sideBox), workBox)));

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
    const chat = h("a", { class: "btn", href: "/" }, icon("external", "sm"), "Open the chat");
    const auto = h("label", { class: "check" }, h("span", { class: "switch" },
      h("input", { type: "checkbox", checked: !!s.autostart, onchange: (e) => busy(null, async () => {
        status = await api.post("/setup", { autostart: e.target.checked });
        renderStatus();
      }) }), h("i")), h("span", null, "Start with FLE Link"));
    const always = h("label", { class: "check", title: "FLE Link starts when you log in, with the last model and the agent, and comes back if it stops. Takes effect at the next login." },
      h("span", { class: "switch" }, h("input", { type: "checkbox", checked: !!s.always_on, onchange: (e) => busy(null, async () => {
        status = await api.post("/always", { on: e.target.checked });
        renderStatus();
      }) }), h("i")), h("span", null, "Always on"));
    clear(statusBox,
      h("div", { class: "card-head" }, icon("spark"), h("div", { class: "grow" }, h("h3", null, "FLE Durable"),
        h("p", null, s.running
          ? (s.remote ? "Your agent, reached over its own interface." : `Uses the model on the Engine page through FLE Link's own /v1. Port ${s.port}, pid ${s.pid}.`)
          : "Uses the model on the Engine page. Start the engine first, then the agent.")), pill),
      s.remote ? h("div", { class: "row wrap" }, s.running ? chat : null, h("span", { class: "small faint" }, `Connected to ${s.url}. It runs on its own; FLE Link only talks to it.`))
        : h("div", { class: "row wrap" }, s.running ? [stop, restart, chat] : [start], h("span", { class: "grow" }), always, auto),
      s.update && !s.running ? h("div", { class: "row wrap" }, callout("info", "A newer FLE Durable comes with this FLE Link."), install(s)) : null,
      s.error ? callout("bad", s.error) : null,
      s.log?.length ? h("details", { class: "ag-log" }, h("summary", null, "Log"), h("pre", { class: "mono" }, s.log.join("\n"))) : null);
  }

  let setupSig = "";
  function renderSetup() {
    const s = status;
    if (!s) return clear(setupBox);
    // the page refreshes every few seconds: drawing again only when something changed keeps what you are typing and an open section
    const sig = JSON.stringify([s.remote, s.url, s.token_set, s.installed, s.node.ok, s.node.error, s.problem, s.runner_dir, s.managed, s.installing, s.installed_pin]);
    if (sig === setupSig) return;
    setupSig = sig;
    const form = { url: s.url || "", token: "", runner_dir: s.runner_dir || "", node: "" };
    const input = (key, attrs) => h("input", { class: "input mono", spellcheck: "false", ...attrs, oninput: (e) => { form[key] = e.target.value; } });
    const save = h("button", { class: "btn primary", onclick: (e) => busy(e.currentTarget, async () => {
      status = await api.post("/setup", form);
      refresh();
    }) }, icon("check", "sm"), "Save");
    const body = [
      s.remote || s.node.ok ? null : callout("warn", s.node.error),
      s.problem ? callout("warn", s.problem) : null,
      !s.remote && s.managed && !s.installed ? h("div", { class: "row" }, install(s)) : null,
      h("details", null, h("summary", null, "Connect to one that runs, or use your own folder"),
        h("div", { class: "grid two" },
          field("Address of a running FLE Durable", input("url", { value: form.url, placeholder: "http://127.0.0.1:8787" }), "FLE Durable is its own program. Empty: FLE Link runs it here."),
          field("Its token", input("token", { type: "password", autocomplete: "off", placeholder: s.token_set ? "A token is saved" : "FLE_DURABLE_TOKEN" }), "Stays on this computer; the page never sees it.")),
        s.remote ? null : h("div", { class: "grid two" },
          field("FLE Durable folder (optional)", input("runner_dir", { value: form.runner_dir, placeholder: "empty: installed by FLE Link" }), "Empty: the copy FLE Link installs."),
          field("Node (optional)", input("node", { placeholder: s.node.ok ? s.node.path : "empty: node from the PATH" }), "Node 22.19 or newer")),
        h("div", { class: "row" }, save))];
    const ready = s.installed && s.node.ok;
    clear(setupBox, ready
      ? h("details", { class: "card stack" }, h("summary", null, "Connection"), ...body)
      : h("div", { class: "card stack" }, h("div", { class: "card-head" }, icon("wrench"), h("div", { class: "grow" }, h("h3", null, "Set up the agent"),
          h("p", null, "The agent is FLE Durable, its own program. Once it is set up, the chat talks to it, and a switch there takes you straight to the model. "
            + "It reads and changes files and runs commands in its working folder on this computer, and asks you before steps that cannot be undone. "
            + "Everything stays on this computer. Install fetches it into FLE Link's data folder; it needs git and Node 22.19 or newer (nodejs.org)."))), ...body));
  }

  // FLE Link fetches FLE Durable itself (the pinned version, into its data folder) and starts it when a model is loaded
  function install(s) {
    return h("button", { class: "btn primary", disabled: !s.node.ok || s.installing,
      onclick: (e) => busy(e.currentTarget, async () => {
        status = await api.post("/install");
        try {
          status = await api.post("/start");
          toast("FLE Durable is installed and running; the chat now talks to it", "good");
        } catch {
          toast("FLE Durable is installed. Load a model on the Engine page, then press Start here.", "good", 8000);
        }
        refresh();
      }) }, icon("download", "sm"), s.installing ? "Installing …" : s.installed_pin ? "Update FLE Durable" : "Install FLE Durable");
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
    planBox.hidden = agendaOff;
    if (agendaOff) return clear(agendaBox, head, h("div", { class: "empty" }, "The agenda is off in this agent."));
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
    const waiting = queue?.entries ?? [];
    const open = approvals.filter((f) => f.status === "open");
    const decide = (f, decision) => (e) => busy(e.currentTarget, async () => {
      await api.post(`/approvals/${encodeURIComponent(f.id)}`, { decision });
      toast(decision === "granted" ? "Allowed" : "Refused", "good");
      refresh();
    });
    const hookUrl = `${location.origin}/api/p/agent/events`;
    clear(sideBox,
      open.length ? h("div", { class: "card stack ag-approvals" },
        h("div", { class: "card-head" }, icon("alert"), h("div", { class: "grow" }, h("h3", null, "Asks you"),
          h("p", null, "Only destructive steps wait for you."))),
        open.map((f) => h("div", { class: "ag-approval stack" },
          h("div", { class: "ag-title" }, f.action),
          f.context ? h("p", { class: "faint" }, f.context) : null,
          f.command ? h("pre", { class: "mono ag-cmd" }, f.command) : null,
          status?.remote ? h("div", { class: "small faint" }, "Waiting for your decision in the agent's own app.")
            : h("div", { class: "row" }, h("button", { class: "btn primary", onclick: decide(f, "granted") }, "Allow"),
              h("button", { class: "btn", onclick: decide(f, "refused") }, "Refuse"))))) : null,
      h("div", { class: "card stack" },
        h("div", { class: "card-head" }, icon("layers"), h("div", { class: "grow" }, h("h3", null, "Waiting"),
          h("p", null, waiting.length ? `${waiting.length} event${waiting.length === 1 ? "" : "s"} for the agent` : "Nothing waits; the agent is up to date."))),
        // the agent keeps events while the model server does not answer (FLE Durable 0.2.2: nothing is lost any more)
        waiting.length && queue?.model?.open === false ? h("div", { class: "callout warn small" },
          "They wait for a model: the agent keeps them until one is loaded on the Engine page.") : null,
        waiting.map((e) => h("div", { class: "ag-event" }, h("div", { class: "row" }, h("span", { class: "tag" }, e.source),
          e.priority === "owner" ? h("span", { class: "pill info" }, "right of way") : null, h("span", { class: "grow" }), h("span", { class: "faint" }, ago(e.accepted))),
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
        h("pre", { class: "mono ag-cmd" }, `curl -X POST ${hookUrl} \\\n  -H 'X-Agent-Key: ${status.webhook_key}' \\\n  -H 'Content-Type: application/json' \\\n  -d '{"id":"ci-42","from":"ci","text":"Build 42 failed on main"}'`),
        h("p", { class: "hint" }, "The key is this agent's own. From another computer add the access key as Bearer token.")));
  }

  // ------------------------------------------------------------------------------------------- computer use

  function renderDesk() {
    deskBox.hidden = !desk || !!status?.remote;
    if (deskBox.hidden) return;
    const left = desk.seconds_left;
    const setMinutes = (e, minutes) => busy(e.currentTarget, async () => { desk = await api.post("/desktop", { minutes }); renderDesk(); });
    const allow = async (e) => {
      const btn = e.currentTarget;
      if (!(await confirmDialog({ title: "Let the agent use this computer for 30 minutes?", body: "It sees your screen and uses your mouse and keyboard, including anything open right now. It ends by itself, and you can end it here at any time.", ok: "Allow", danger: true }))) return;
      await busy(btn, async () => { desk = await api.post("/desktop", { minutes: 30 }); renderDesk(); });
    };
    const missing = desk.missing.length ? `Install first: ${desk.missing.join(", ")}.` : "";
    clear(deskBox, h("div", { class: "card-head" }, icon("alert"), h("div", { class: "grow" }, h("h3", null, "Computer use"),
      h("p", null, "Off unless you allow it here. While allowed, the agent sees your screen and uses your mouse and keyboard."))),
      left > 0
        ? h("div", { class: "row wrap" }, h("span", { class: "pill warn" }, `Allowed, ${Math.floor(left / 60)}:${pad(left % 60)} left`), h("button", { class: "btn primary", onclick: (e) => setMinutes(e, 0) }, "End now"))
        : h("div", { class: "row wrap" }, h("button", { class: "btn", disabled: !desk.ready, onclick: allow }, "Allow for 30 minutes")),
      desk.problem || missing ? callout("info", desk.problem || missing) : null,
      desk.mac && desk.ready ? h("p", { class: "hint" }, "macOS asks once for Screen Recording and Accessibility.") : null);
  }

  // ------------------------------------------------------------------------------------------- tickets, calendar, events

  async function loadWork() {
    try {
      const from = new Date();
      const to = new Date(from.getTime() + 28 * 86400000);
      const [tickets, calendar, bus] = await Promise.all([
        api.get("/tickets?limit=100"),
        api.get(`/calendar?from=${from.toISOString()}&to=${to.toISOString()}`),
        api.get("/bus/state"),
      ]);
      const topics = await Promise.all((bus.topics ?? []).map((t) => api.get(`/bus?topic=${encodeURIComponent(t.topic)}&limit=30&after=${Math.max(0, t.head - 30)}`)));
      const events = topics.flatMap((t) => t.events ?? []).sort((a, b) => b.seq - a.seq).slice(0, 30);
      Object.assign(work, { tickets: tickets.tickets ?? [], calendar: calendar.entries ?? [], unreachable: calendar.unreachable ?? [], events, off: false });
    } catch {
      work.off = true; // an agent without them (an older one, or one reached with a limited token): the page goes on without this card
    }
  }

  function renderWork() {
    if (!status?.running || work.off) {
      workSig = "";
      workBox.hidden = true;
      return;
    }
    const sig = JSON.stringify([work.tab, work.tickets, work.calendar, work.unreachable, work.events]);
    if (sig === workSig) return; // keeps what you are typing
    workSig = sig;
    workBox.hidden = false;
    const again = () => { workSig = ""; return refresh(); };
    const tab = (id, label, n) => h("button", { class: `btn${work.tab === id ? " primary" : ""}`, onclick: () => { work.tab = id; workSig = ""; renderWork(); } }, n ? `${label} (${n})` : label);
    const input = (attrs) => h("input", { class: "input", ...attrs });
    let body;
    if (work.tab === "tickets") {
      const title = input({ placeholder: "A new ticket for the agent or for you" });
      const add = (e) => busy(e.currentTarget, async () => {
        if (!title.value.trim()) return;
        await api.post("/tickets", { type: "VOR", title: title.value.trim() });
        await again();
      });
      const row = (t) => h("div", { class: "row ag-hist" }, h("span", { class: "tag" }, t.id), h("span", { class: "grow" }, t.title),
        h("select", { class: "select", onchange: (e) => busy(null, async () => { await api.post(`/tickets/${encodeURIComponent(t.id)}`, { status: e.target.value }); await again(); }) },
          TICKET_STATES.map(([v, l]) => h("option", { value: v, selected: v === t.status }, l))));
      body = [h("div", { class: "row" }, title, h("button", { class: "btn primary", onclick: add }, "Add")),
        work.tickets.length ? work.tickets.map(row) : h("p", { class: "faint" }, "No tickets yet.")];
    } else if (work.tab === "calendar") {
      const title = input({ placeholder: "What" });
      const when = input({ type: "datetime-local" });
      const add = (e) => busy(e.currentTarget, async () => {
        if (!title.value.trim() || !when.value) return;
        await api.post("/calendar", { title: title.value.trim(), from: new Date(when.value).toISOString() });
        await again();
      });
      const row = (c) => h("div", { class: "row ag-hist" }, h("span", { class: "mono faint" }, `${dayLabel(Date.parse(c.from))} ${timeOf(Date.parse(c.from))}`), h("span", { class: "grow" }, c.title),
        c.id.startsWith("ext-")
          ? h("span", { class: "tag" }, c.source)
          : h("button", { class: "btn", onclick: (e) => busy(e.currentTarget, async () => { await api.del(`/calendar/${encodeURIComponent(c.id)}`); await again(); }) }, "Remove"));
      body = [h("div", { class: "row" }, title, when, h("button", { class: "btn primary", onclick: add }, "Add")),
        ...work.unreachable.map((u) => callout("alert", `${u.source} could not be read: ${u.reason}. What is shown from it may be out of date.`)),
        work.calendar.length ? work.calendar.map(row) : h("p", { class: "faint" }, "Nothing in the next four weeks.")];
    } else {
      body = work.events.length
        ? work.events.map((e) => h("div", { class: "ag-event" }, h("div", { class: "row" }, h("span", { class: "tag" }, e.source ?? "event")), h("div", { class: "faint ag-text" }, e.text)))
        : [h("p", { class: "faint" }, "Nothing has happened yet.")];
    }
    clear(workBox, h("div", { class: "card-head" }, icon("layers"), h("div", { class: "grow" }, h("h3", null, "Tickets, calendar, events"),
      h("p", null, "What you and the agent keep track of. Both can add to it."))),
      h("div", { class: "row wrap" }, tab("tickets", "Tickets", work.tickets.length), tab("calendar", "Calendar", work.calendar.length), tab("events", "Events")), body);
  }

  // ------------------------------------------------------------------------------------------- data

  async function refresh() {
    try {
      status = await api.get("/status");
      desk = await api.get("/desktop").catch(() => null);
      if (status.running) {
        [agenda, queue, approvals] = await Promise.all([
          api.get("/agenda").then((a) => { agendaOff = false; return a; }).catch((e) => { agendaOff = e.status === 503; return agendaOff ? null : agenda; }),
          api.get("/queue").catch(() => queue),
          api.get("/approvals").then((r) => r.approvals ?? []).catch(() => approvals),
        ]);
        await loadWork();
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
    renderWork();
    renderDesk();
    if (!!status?.running !== planRunning) {
      planRunning = !!status?.running; // the form opens as soon as the agent runs, what was typed stays
      renderPlan();
    }
  }

  async function loop() {
    await refresh();
    if (!gone) timer = setTimeout(loop, POLL_MS);
  }

  renderStatus();
  renderPlan();
  loop();
  return { unmount() { gone = true; clearTimeout(timer); } };
}
