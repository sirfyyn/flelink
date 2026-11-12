// The live panel: what the engine does right now (speed, accepted drafts, expert cache, GPUs, memory). The direct chat and the agent chat
// both show it, so the numbers do not vanish when the agent answers.
import { on, store } from "./app.js";
import { clear, countTo, debounce, fmt, h, icon, meter, ring, sparkline, stateBadge } from "./ui.js";

export function createLivePanel() {
  let open = false;
  const live = h("aside", { class: "live-panel", "aria-label": "Live data" });
  const button = h("button", { class: "btn ghost icon", title: "Live data", "aria-label": "Live data",
    onclick: () => { open = !open; live.classList.toggle("open", open); } }, icon("panel"));

  function render() {
    const hist = store.live.slice(-120);
    const lastS = hist[hist.length - 1] || {};
    const meta = store.liveMeta || {};
    const e = store.engine || {};
    const recent = (key, secs) => {
      const cut = (lastS.t || 0) - secs;
      for (let k = hist.length - 1; k >= 0; k -= 1) {
        if (hist[k].t < cut) break;
        if (hist[k][key] !== undefined && hist[k][key] !== null) return hist[k][key];
      }
      return null;
    };
    const lr = meta.last_response;
    const perAnswer = !!lastS.last_answer;  // a server without --metrics: the numbers are each finished answer's
    const decodeNow = perAnswer ? null : recent("decode", 3);
    const decode = decodeNow ?? lr?.predicted_per_second ?? null;
    const prefill = recent("prefill", 15) ?? lr?.prompt_per_second ?? null;
    let accept = recent("accept", 20);
    if (accept === null && meta.accept_logged !== null && meta.accept_logged !== undefined) accept = meta.accept_logged;
    if (accept === null && lr?.draft_n) accept = (lr.draft_n_accepted || 0) / lr.draft_n;
    const cacheHit = meta.cache?.hit_rate !== undefined && meta.cache?.hit_rate !== null ? meta.cache.hit_rate / 100 : null;
    const svc = e.service || null;
    const fle = (e.plan?.args || []).includes("--moe-expert-cache-inserts") || cacheHit !== null || !!svc?.expert_cache;

    const tiles = [];
    tiles.push(h("div", { class: "row between" }, h("h4", null, "Live"), stateBadge(e.state)));
    tiles.push(h("div", { class: "tile" },
      h("div", { class: "k" }, h("span", null, "Generation"), h("span", { class: "faint" }, decodeNow !== null ? "now" : lr ? "last answer" : "")),
      h("div", { class: "v" }, countTo("live-decode", decode, fmt.rate), h("small", null, "tok/s")),
      sparkline(hist.map((x) => x.decode ?? null), { color: "var(--accent)", id: "dec" }),
      h("div", { class: "sub" }, perAnswer ? "Average of each finished answer, last two minutes" : "Tokens per second while answering, last two minutes")));
    tiles.push(h("div", { class: "tile" },
      h("div", { class: "k", title: "How fast the model reads your message and the chat so far before it starts to answer" }, h("span", null, "Prompt reading (prefill)")),
      h("div", { class: "v" }, countTo("live-prefill", prefill, fmt.rate), h("small", null, "tok/s")),
      sparkline(hist.map((x) => x.prefill ?? null), { color: "var(--violet)", id: "pre" })));
    tiles.push(h("div", { class: "tile" }, h("div", { class: "rings" },
      ring(accept, { color: "var(--cyan)", label: "Drafts accepted",
        help: "The model guesses a few tokens ahead and checks them; this is the share it kept. Higher is faster. Empty: the model drafts nothing." }),
      ring(fle ? cacheHit : null, { color: "var(--fuchsia)", label: fle ? "Expert cache hits" : e.state === "attached" ? "Expert cache (not reported)" : "Expert cache (FLE only)",
        help: "How often the GPU already had the expert a token needed, instead of reading it from RAM. Higher is faster. Only the FLE engine has this cache." }))));

    const gpus = lastS.gpus || [];
    if (gpus.length) {
      tiles.push(h("h4", null, "GPU"));
      for (const g of gpus) {
        tiles.push(h("div", { class: "tile gpu" },
          h("div", { class: "gpu-name" }, g.name, h("span", null, [g.temperature_c ? `${g.temperature_c} °C` : "", g.power_w ? `${Math.round(g.power_w)} W` : ""].filter(Boolean).join(" · "))),
          meter("Busy", g.utilization || 0, 100, "cyan", `${g.utilization ?? "–"} %`),
          meter("Memory", (g.memory_used || 0), (g.memory_total || 0), "violet")));
      }
    }
    tiles.push(h("h4", null, "Memory"));
    const memTile = h("div", { class: "tile gpu" });
    if (lastS.ram_total) memTile.append(meter("System RAM", lastS.ram_used, lastS.ram_total));
    if (lastS.engine_mem) memTile.append(h("div", { class: "row between small" }, h("span", null, "Engine process"), h("span", { class: "muted num" }, fmt.mem(lastS.engine_mem))));
    if (lastS.slots) memTile.append(h("div", { class: "row between small" }, h("span", { title: "How many requests the engine is answering at once, of how many it can" }, "Slots busy"), h("span", { class: "muted num" }, `${lastS.slots_busy || 0} of ${lastS.slots}`)));
    if (meta.totals?.generated) memTile.append(h("div", { class: "row between small" }, h("span", null, "Generated since start"), h("span", { class: "muted num" }, `${fmt.int(meta.totals.generated)} tokens`)));
    tiles.push(memTile);
    if (meta.metrics_available === false) {
      tiles.push(h("div", { class: "callout" }, icon("info", "sm"), h("div", { class: "small" }, "This server runs without --metrics, so the numbers come from each finished answer.")));
    }
    if (svc?.expert_cache && !svc.cache_in_log && cacheHit === null) {
      tiles.push(h("div", { class: "callout" }, icon("info", "sm"), h("div", { class: "small" },
        `The expert-cache hit rate is a log line at level 4; ${svc.unit} logs up to level ${svc.log_level}. Add -lv 4 to its command to see it here.`)));
    }
    clear(live, tiles);
  }

  const offLive = on("live", render);
  const offEngine = on("engine", render);
  const resizeOnce = debounce(() => render(), 150);
  window.addEventListener("resize", resizeOnce);
  render();

  return {
    el: live,
    button,
    unmount() {
      offLive();
      offEngine();
      window.removeEventListener("resize", resizeOnce);
    },
  };
}
