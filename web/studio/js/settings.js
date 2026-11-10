// Settings: looks, where models live, how the chat samples, the Hugging Face token and access from other machines.

import { del, get, post } from "./api.js";
import { applyTheme, on, refreshPlugins, refreshState, settingsSections, store, topbar } from "./app.js";
import { toolsSection, stopToolsPolling } from "./settings-tools.js";
import { clear, fmt, h, icon, toast } from "./ui.js";
import { LANGUAGES, chosenLanguage, setLanguage } from "./i18n.js";
import { canPickFolder, pickFolder } from "./app-bridge.js";
import { loopGuardOn, setLoopGuard } from "./loop-guard.js";

// The names of the plugins that ship with FLE Link, for the list when one is off (its own title is not known then).
const BUNDLED = { forge: "Forge", updater: "Updates", remote: "Remote access", recorder: "Recorder", switch: "Model switcher" };

export function mount(main) {
  const unsub = [];
  const page = h("div", { class: "page-inner", style: { maxWidth: "860px" } });
  main.replaceChildren(topbar("Settings"), h("div", { class: "page" }, page));

  async function save(patch, quiet = false) {
    try {
      store.state.settings = await post("/api/settings", patch);
      applyTheme();
      if (!quiet) toast("Saved", "good", 1600);
      return true;
    } catch (e) {
      toast(e.message, "bad", 7000);
      return false;
    }
  }

  function render(force = false) {
    const s = store.state?.settings;
    if (!s) return;
    if (!force && page.contains(document.activeElement) && /^(INPUT|TEXTAREA)$/.test(document.activeElement.tagName)) return;
    const ui = { section, field, toggle, save, render: () => render(true) };
    const parts = [
      [10, look(s)], [20, models(s)], [30, chat(s)], [40, toolsSection(s, ui)], [44, mcpHub()], [45, accounts()], [46, github()],
      [60, hub(s)], [70, network(s)],
      [100, plugins()], [110, about()],
    ];
    for (const extra of settingsSections) { // sections that plugins add (kit.settingsSection)
      try { parts.push([extra.order, extra.build(s, ui)]); } catch (e) { console.error(`plugin ${extra.id}:`, e); }
    }
    clear(page,
      h("div", { class: "page-head" }, h("div", null, h("h2", null, "Settings"),
        h("p", null, `Stored in ${store.state.data_dir}. Start settings for the engine live on the Engine page.`))),
      ...parts.sort((a, b) => a[0] - b[0]).map((x) => x[1]));
  }

  // ------------------------------------------------------------------------------------------- sections

  // The MCP hub: one address whose few meta tools stand for all its plugins, so many tools do not slow the model down.
  // The token is never shown again; leaving the field empty keeps the saved one.
  function mcpHub() {
    const box = h("div", { class: "stack" }, h("span", { class: "small faint" }, "…"));
    // whether the hub answers, from the tool list (the hub is the MCP server "hub")
    const live = h("div", { class: "callout" }, icon("info", "sm"), h("div", { class: "small" }, "Checking the hub …"));
    const check = async (url) => {
      try {
        const t = await get("/api/tools");
        const sv = (t.servers || []).find((x) => x.name === "hub");
        if (!sv) { // the chat's tools are off: the hub is not connected at all
          live.className = "callout warn";
          clear(live, icon("alert", "sm"), h("div", { class: "small" }, "Saved, but the chat's tools are off: turn them on under Tools to use the hub."));
          return;
        }
        const ok = sv?.status === "ready";
        live.className = `callout ${ok ? "good" : sv?.status === "error" ? "bad" : ""}`;
        clear(live, icon(ok ? "ok" : sv?.status === "error" ? "alert" : "info", "sm"), h("div", { class: "small" },
          ok ? `Connected to ${url}: ${sv.tools} meta tools stand for all of the hub's plugins.`
            : sv?.status === "error" ? `The hub does not answer: ${sv.error || "no reason given"}`
            : `Connecting to ${url} …`));
        if (!ok && sv?.status !== "error") setTimeout(() => check(url), 1500);
      } catch { /* the tool list is not reachable: leave the note */ }
    };
    const draw = (st) => {
      if (st.connected) check(st.url);
      const url = h("input", { class: "input mono", value: st.url || "", placeholder: "http://127.0.0.1:8090/mcp" });
      const tok = h("input", { class: "input mono", type: "password", autocomplete: "off",
        placeholder: st.token ? `A token is saved (${st.hint})` : "Access token (if the hub asks for one)" });
      clear(box,
        st.connected ? live : null,
        h("label", { class: "field" }, h("span", null, "Hub address"), url),
        h("label", { class: "field" }, h("span", null, "Access token"), tok),
        h("div", { class: "row" },
          h("button", { class: "btn", onclick: async () => {
            try { draw(await post("/api/hub", { url: url.value.trim(), token: tok.value.trim() })); toast("Hub saved", "good", 1600); }
            catch (e) { toast(e.message, "bad", 7000); }
          } }, "Save"),
          st.connected ? h("button", { class: "btn ghost danger", onclick: async () => {
            try { draw(await del("/api/hub")); } catch (e) { toast(e.message, "bad"); }
          } }, "Remove") : null),
        h("span", { class: "small faint" }, "The FYOS MCP hub (fyos-hub) on this computer or another one. Its token stays in a file only you can read and is only sent to the hub."));
    };
    get("/api/hub").then(draw).catch(() => clear(box));
    return section("grid", "MCP hub", "Tools for the chat and the agent through one hub: it shows the model a handful of meta tools instead of every tool, so adding many tools does not slow it down.", box);
  }

  // Accounts connected with a click (accounts.rs). While a sign-in is open elsewhere, the card asks every few seconds.
  function accounts() {
    const box = h("div", { class: "stack" }, h("span", { class: "small faint" }, "…"));
    let watching = 0;
    const load = () => get("/api/accounts").then(draw).catch(() => clear(box));
    const watch = () => {
      watching = 100; // five minutes
      const tick = () => { if (box.isConnected && --watching > 0) load().then(() => setTimeout(tick, 3000)); };
      setTimeout(tick, 3000);
    };
    const act = (fn) => async () => { try { await fn(); } catch (e) { toast(e.message, "bad", 10000); } };
    const signIn = (id) => act(async () => {
      // the provider's sign-in pages in the language of this page
      const r = await post(`/api/accounts/${id}`, { lang: document.documentElement.lang || navigator.language || "" });
      window.open(r.auth_url, "_blank", "noopener");
      toast("A sign-in page opened in your browser. Come back here when you are done.", "info", 8000);
      watch();
    });
    const row = (title, what, on, id) => h("div", { class: "row" },
      h("div", { class: "grow" }, h("strong", null, title), h("div", { class: "small faint" }, what)),
      on ? h("span", { class: "pill good" }, "Connected") : null,
      on ? h("button", { class: "btn ghost danger", onclick: act(async () => draw(await del(`/api/accounts/${id}`))) }, "Disconnect")
        : h("button", { class: "btn", onclick: signIn(id) }, "Connect"));
    function githubRow(g) {
      const on = !!g.account?.connected;
      const waiting = g.user_code && !g.error && !on;
      return [h("div", { class: "row" },
        h("div", { class: "grow" }, h("strong", null, "GitHub"), h("div", { class: "small faint" }, on ? `Signed in as ${g.account.login}.` : "For the agent's git and gh. Grants access to all repos your account can reach; for chosen repos only, paste a token below.")),
        on ? h("span", { class: "pill good" }, "Connected") : null,
        on ? null : waiting
          ? h("button", { class: "btn ghost", onclick: act(async () => draw(await del("/api/accounts/github"))) }, "Cancel")
          : h("button", { class: "btn", onclick: act(async () => { draw(await post("/api/accounts/github", {})); watch(); }) }, "Connect")),
        waiting ? h("div", { class: "callout info" }, icon("info", "sm"), h("div", { class: "small" },
          "Enter this code at ", h("a", { href: g.uri, target: "_blank", rel: "noopener" }, g.uri, " ", icon("external", "sm")), ": ", h("strong", { class: "mono" }, g.user_code))) : null,
        g.error && !on ? h("div", { class: "callout bad" }, icon("alert", "sm"), h("div", { class: "small" }, g.error)) : null];
    }
    function icloudRow(c) {
      if (c?.signed_in) {
        return [h("div", { class: "row" },
          h("div", { class: "grow" }, h("strong", null, "iCloud"), h("div", { class: "small faint" }, `Your calendars appear on the Agent page and for the agent (${c.user}). FLE Link only reads them.`)),
          h("span", { class: "pill good" }, "Connected"),
          h("button", { class: "btn ghost danger", onclick: act(async () => draw(await del("/api/accounts/icloud"))) }, "Disconnect"))];
      }
      const user = h("input", { class: "input", type: "email", autocomplete: "username", placeholder: "Apple ID" });
      const pw = h("input", { class: "input mono", type: "password", autocomplete: "off", placeholder: "App-specific password" });
      return [h("div", { class: "row" },
        h("div", { class: "grow" }, h("strong", null, "iCloud"), h("div", { class: "small faint" },
          "Your calendars, read only. Apple needs an app-specific password for this, not your normal one: ",
          h("a", { href: "https://account.apple.com", target: "_blank", rel: "noopener" }, "make one at account.apple.com ", icon("external", "sm")),
          " (Sign-In and Security > App-Specific Passwords)."))),
        h("div", { class: "row" }, user, pw,
          h("button", { class: "btn", onclick: (e) => busy(e.currentTarget, async () => {
            if (!user.value.trim() || !pw.value.trim()) return;
            draw(await post("/api/accounts/icloud", { user: user.value.trim(), password: pw.value.trim() }));
            toast("iCloud connected", "good", 2000);
          }) }, "Connect"))];
    }
    async function busy(btn, fn) {
      btn.disabled = true;
      try { await fn(); } catch (e) { toast(e.message, "bad", 10000); } finally { btn.disabled = false; }
    }
    function draw(st) {
      clear(box,
        row("Hugging Face", "For gated and private models. Public models download without it.", st.huggingface.signed_in, "huggingface"),
        ...githubRow(st.github),
        ...icloudRow(st.icloud),
        row("Google","Your main calendar appears on the Agent page and for the agent. FLE Link only reads it, and the events stay on this computer.", st.google.signed_in, "google"));
    }
    load();
    return section("link", "Accounts", "Connect once, FLE Link keeps you signed in. Tokens stay on this computer in files only you can read.", box);
  }

  // The GitHub sign-in: a token for the agent's git and gh, and for the hub's GitHub plugin. Never shown again.
  function github() {
    const box = h("div", { class: "stack" }, h("span", { class: "small faint" }, "…"));
    const draw = (st) => {
      const tok = h("input", { class: "input mono", type: "password", autocomplete: "off", placeholder: "github_pat_…" });
      clear(box,
        st.connected
          ? h("div", { class: "callout good" }, icon("ok", "sm"), h("div", { class: "small" },
              `Signed in as ${st.login}${st.name ? ` (${st.name})` : ""}, token ${st.hint}.`))
          : h("span", { class: "small muted" }, "Not signed in."),
        h("div", { class: "row" }, tok,
          h("button", { class: "btn", onclick: async () => {
            if (!tok.value.trim()) return;
            try { draw(await post("/api/github", { token: tok.value.trim() })); toast("Signed in to GitHub", "good", 2000); }
            catch (e) { toast(e.message, "bad", 8000); }
          } }, st.connected ? "Replace" : "Sign in"),
          st.connected ? h("button", { class: "btn ghost danger", onclick: async () => {
            try { draw(await del("/api/github")); } catch (e) { toast(e.message, "bad"); }
          } }, "Sign out") : null),
        h("span", { class: "small faint" },
          "A fine-grained token is best: only the repos you choose, with Contents, Pull requests and Issues read and write. ",
          h("a", { href: st.new_token_url, target: "_blank", rel: "noopener" }, "Make one on GitHub ", icon("external", "sm")),
          ". FLE Link checks it with GitHub, keeps it in a file only you can read and gives it to the agent (git, gh) and to the hub's GitHub plugin; the page never shows it again."));
    };
    get("/api/github").then(draw).catch(() => clear(box));
    return section("link", "GitHub", "Let the model work in your repos.", box);
  }

  function section(iconName, title, sub, ...body) {
    return h("div", { class: "card" }, h("div", { class: "card-head" }, icon(iconName), h("div", null, h("h3", null, title), sub ? h("p", null, sub) : null)),
      h("div", { class: "stack" }, ...body));
  }

  function look(s) {
    return section("spark", "Appearance", "",
      seg([["system", "Like the system"], ["dark", "Dark"], ["light", "Light"]], s.theme, (v) => save({ theme: v }, true)),
      // the language is kept in this browser (i18n.js); the page reloads in it
      h("label", { class: "field" }, h("span", null, "Language"),
        seg([["auto", "Like the browser"], ...LANGUAGES], chosenLanguage(), (v) => setLanguage(v))),
      // kept in this browser (loop-guard.js)
      h("label", { class: "field" }, h("span", null, "Repeat check"),
        seg([["on", "Stop a model that repeats itself"], ["off", "Never stop it"]], loopGuardOn() ? "on" : "off", (v) => setLoopGuard(v === "on"))));
  }

  function models(s) {
    const dir = h("input", { class: "input mono", value: s.models_dir });
    const el = section("folder", "Models folder", "Where downloads go and where FLE Link looks for GGUF files. A drive with more room is fine.",
      h("div", { class: "row" },
        canPickFolder() ? h("button", { class: "btn", onclick: async () => { const p = await pickFolder("Models folder"); if (p) dir.value = p; } }, icon("folder", "sm"), "Choose…") : null,
        dir, h("button", { class: "btn", onclick: async () => { if (await save({ models_dir: dir.value.trim() })) refreshState(); } }, "Save")),
      h("span", { class: "small faint" }, `Changing it moves nothing; models already in the new folder show up in the library. ${fmt.bytes(store.state.system?.disk_free)} free there now.`));
    el.id = "folder";
    // "use another folder" from the first-run view lands here
    if (location.hash === "#folder") setTimeout(() => { el.scrollIntoView({ block: "start" }); dir.focus(); }, 60);
    return el;
  }

  function chat(s) {
    const c = s.chat;
    const custom = c.sampling === "custom";
    const num = (key, step, min, max) => h("input", { class: "input num", type: "number", step, min, max, value: c[key], disabled: !custom,
      onchange: (e) => save({ chat: { [key]: Number(e.target.value) } }, true) });
    return section("chat", "Chat", "Defaults for new answers. A chat can still turn thinking on or off from the message box.",
      h("label", { class: "field" }, h("span", null, "Sampling"),
        seg([["model", "The model's recommended values"], ["custom", "My own values"]], c.sampling, async (v) => { await save({ chat: { sampling: v } }, true); render(); })),
      h("div", { class: "grid three" },
        field("Temperature", num("temperature", 0.05, 0, 2)),
        field("Top P", num("top_p", 0.01, 0, 1)),
        field("Top K", num("top_k", 1, 0, 1000))),
      h("div", { class: "grid three" },
        field("Min P", num("min_p", 0.01, 0, 1)),
        field("Presence penalty", num("presence_penalty", 0.1, -2, 2)),
        field("Answer length limit", h("input", { class: "input num", type: "number", min: 0, step: 256, value: c.max_tokens,
          onchange: (e) => save({ chat: { max_tokens: Math.max(0, Math.round(Number(e.target.value) || 0)) } }, true) }), "0 = no limit")),
      h("label", { class: "row" }, toggle(c.thinking, (v) => save({ chat: { thinking: v } }, true)),
        h("span", null, "Let the model think before it answers ", h("span", { class: "muted small" }, "(for models that can)"))),
      field("System prompt", h("textarea", { class: "textarea", rows: 3, value: c.system_prompt,
        placeholder: "Optional. Sent at the start of every new chat.", onchange: (e) => save({ chat: { system_prompt: e.target.value } }, true) })),
      field("Largest upload", h("input", { class: "input num", type: "number", min: 1, max: 512, step: 1, value: c.max_upload_mb,
        "aria-label": "Largest upload in MB", onchange: (e) => save({ chat: { max_upload_mb: Math.max(1, Math.min(512, Math.round(Number(e.target.value) || 25))) } }, true) }),
      "MB per attached file (1 to 512). Larger files are refused with a message that names this limit."));
  }

  function hub(s) {
    const token = h("input", { class: "input mono", type: "password", autocomplete: "off",
      placeholder: s.has_hf_token ? "A token is saved" : "hf_…" });
    return section("models", "Hugging Face", "Only needed for gated or private repos. Public models download without it.",
      h("div", { class: "row" }, token,
        h("button", { class: "btn", onclick: async () => { if (token.value.trim() && await save({ hf_token: token.value.trim() })) render(); } }, "Save"),
        s.has_hf_token ? h("button", { class: "btn ghost danger", onclick: async () => { if (await save({ hf_token: "" })) render(); } }, "Remove") : null),
      h("span", { class: "small faint" }, "Kept in settings.json on this computer and only sent to huggingface.co. FLE Link never shows it again."));
  }

  function network(s) {
    const srv = s.server;
    const lan = srv.host !== "127.0.0.1" && srv.host !== "localhost" && srv.host !== "::1";
    const key = h("input", { class: "input mono", type: "password", autocomplete: "off",
      placeholder: s.has_access_key ? "A key is saved" : "Choose a key for other machines" });
    const hosts = h("input", { class: "input mono", value: srv.allowed_hosts, placeholder: "workstation, studio.home.arpa" });
    const port = h("input", { class: "input num", type: "number", min: 1, max: 65535, value: srv.port });
    return section("link", "Access", "Who can open FLE Link. Changes here apply the next time FLE Link starts.",
      seg([["127.0.0.1", "This computer only"], ["0.0.0.0", "Other machines too"]], lan ? "0.0.0.0" : "127.0.0.1",
        async (v) => { await save({ server: { host: v } }, true); render(); }),
      lan ? h("div", { class: "callout warn" }, icon("alert", "sm"), h("div", null,
        "Anyone on your network who knows the access key can chat and start or stop the engine. Use it on networks you trust.",
        s.has_access_key ? null : h("div", null, h("b", null, "Set an access key below; without one, FLE Link makes one up and prints it at start.")))) : null,
      h("div", { class: "grid two" },
        field("Port", port, "Default 8642."),
        field("Access key", h("div", { class: "row" }, key,
          h("button", { class: "btn", onclick: async () => { if (key.value.trim() && await save({ access_key: key.value.trim() })) render(); } }, "Save")),
          "Other machines sign in with it; API clients send it as the Bearer key.")),
      lan ? field("Extra host names", hosts, "Names this machine is reached under, comma separated. IP addresses always work.") : null,
      h("div", { class: "row" },
        h("button", { class: "btn", onclick: async () => {
          const patch = { server: { port: Math.max(1, Math.min(65535, Number(port.value) || 8642)) } };
          if (lan) patch.server.allowed_hosts = hosts.value.trim();
          if (await save(patch)) render();
        } }, lan ? "Save port and names" : "Save port")),
      h("label", { class: "row" }, toggle(srv.open_browser, (v) => save({ server: { open_browser: v } }, true)),
        h("span", null, "Open the browser when FLE Link starts")));
  }

  function plugins() {
    const list = store.plugins;
    const rows = list.length ? list.map((p) => h("div", { class: "row wrap" },
      toggle(p.enabled, async (on) => {
        try {
          await post(`/api/plugins/${p.id}/${on ? "enable" : "disable"}`, {});
          await refreshPlugins();
          if (p.bundled) { location.reload(); return; } // what such a plugin adds to the page comes or goes with a fresh load
          render(true);
          const now = store.plugins.find((x) => x.id === p.id);
          if (now?.error) toast(`${p.id}: ${now.error}`, "bad", 8000);
        } catch (e) { toast(e.message, "bad", 7000); render(true); }
      }),
      h("span", { class: "grow" }, h("b", null, p.title || p.page?.title || BUNDLED[p.id] || p.id), " ",
        h("span", { class: "small muted mono" }, p.bundled ? "comes with FLE Link" : [p.package, p.version].filter(Boolean).join(" ")),
        p.summary ? h("div", { class: "small muted" }, p.summary) : null),
      p.error ? h("span", { class: "pill bad", title: p.error }, "did not load") : p.loaded ? h("span", { class: "pill good" }, "on") : null))
      : [h("span", { class: "small faint" }, "No plugins installed. A plugin is a folder in the data folder's plugins directory; see docs/PLUGINS.md.")];
    return section("grid", "Plugins", "Forge, updates, remote access, the recorder and the model switcher come with FLE Link as plugins and are on; turn off what you do not use. Plugins you install yourself run inside FLE Link with its rights and start off: turn on only those you trust. The page reloads after you turn one of those on or off. Plugins belong to this machine; a paired client does not see them.",
      ...rows);
  }

  function about() {
    const st = store.state;
    const sys = st.system || {};
    // the product version (FLE Link); state.version is the Studio API version the page was written against
    const product = document.querySelector('meta[name="flelink-version"]')?.content;
    return section("info", "About", "",
      h("dl", { class: "kv" },
        h("dt", null, "FLE Link"), h("dd", null, `${product && !product.startsWith("__") ? product : st.version} · ${st.platform}`),
        sys.python ? h("dt", null, "Python") : null, sys.python ? h("dd", null, sys.python) : null,
        h("dt", null, "Data folder"), h("dd", { class: "mono" }, st.data_dir),
        h("dt", null, "Reference machine"), h("dd", null, st.reference?.machine || "–")),
      h("div", { class: "row wrap small" },
        h("a", { href: "https://github.com/sirfyyn/FastLocalExperts", target: "_blank", rel: "noopener" }, "FLE engine on GitHub ", icon("external", "sm")),
        h("a", { href: st.reference?.report || "https://sirfyyn.github.io/FastLocalExperts/", target: "_blank", rel: "noopener" }, "How FLE works ", icon("external", "sm")),
        h("a", { href: "https://huggingface.co/sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF", target: "_blank", rel: "noopener" }, "FLE models ", icon("external", "sm"))),
      h("span", { class: "small faint" }, "FLE Link talks to the internet only when you download a model or an engine, search Hugging Face, look for updates (once a day, switchable above), or turn on web search or a web MCP server for the chat. No telemetry."));
  }

  // ------------------------------------------------------------------------------------------- controls

  function field(label, control, hint = "") {
    return h("label", { class: "field" }, h("span", null, label), control, hint ? h("span", { class: "hint" }, hint) : null);
  }

  function seg(options, value, onPick) {
    const box = h("div", { class: "seg", role: "radiogroup" });
    for (const [v, label] of options) {
      const b = h("button", { type: "button", class: v === value ? "on" : "", role: "radio", "aria-checked": v === value ? "true" : "false" }, label);
      b.addEventListener("click", () => {
        box.querySelectorAll("button").forEach((x) => { x.classList.remove("on"); x.setAttribute("aria-checked", "false"); });
        b.classList.add("on");
        b.setAttribute("aria-checked", "true");
        onPick(v);
      });
      box.append(b);
    }
    return box;
  }

  function toggle(checked, onFlip) {
    return h("span", { class: "switch" }, h("input", { type: "checkbox", checked, onchange: (e) => onFlip(e.target.checked) }), h("i"));
  }

  unsub.push(on("state", render));
  render();
  return { unmount() { unsub.forEach((u) => u()); stopToolsPolling(); } };
}
