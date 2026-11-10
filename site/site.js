// Small things only: the install tabs, the copy buttons, "your machine" (with fit.js) and the engine slot.
// The recordings are played by player.js. The page reads fine without any of it.
(() => {
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;

  // install command: macOS/Linux or Windows; Windows visitors start on their tab. Every copy of the command on the page
  // follows the tab (hero and the closing call).
  const cmds = [...document.querySelectorAll("code[data-unix]")];
  const tabs = [...document.querySelectorAll("[data-os]")];
  const pick = (os) => {
    tabs.forEach((t) => t.setAttribute("aria-selected", String(t.dataset.os === os)));
    cmds.forEach((c) => (c.textContent = c.dataset[os]));
  };
  tabs.forEach((t) => t.addEventListener("click", () => pick(t.dataset.os)));
  if (/Windows/.test(navigator.userAgent)) pick("win");

  document.querySelectorAll("[data-copy]").forEach((b) => b.addEventListener("click", async () => {
    const text = document.querySelector(b.dataset.copy).textContent;
    try {
      await navigator.clipboard.writeText(text);
      b.textContent = "Copied";
    } catch {
      const r = document.createRange();
      r.selectNodeContents(document.querySelector(b.dataset.copy));
      getSelection().removeAllRanges();
      getSelection().addRange(r);
      b.textContent = "Selected";
    }
    setTimeout(() => (b.textContent = "Copy"), 1600);
  }));

  // "What runs on your machine?": two choices, the verdict from fit.js (the program's own rule), a speed only where one
  // was measured on that hardware. The table in the page is the answer for the machine everything was measured on.
  const form = document.querySelector("[data-fit]");
  const F = window.FLEFit;
  if (form && F) {
    const rows = document.getElementById("fit-rows");
    const big = document.getElementById("fit-b");
    const what = document.getElementById("fit-what");
    const anySize = document.getElementById("any-size");
    const anyOut = document.getElementById("any-out");
    const PARAMS = { "llama-3.2-1b": 1, "katy-27b-dflash": 27, "fle-iq3-m": 125, "deepseek-v41-ista-dspark": 763, "rin-deepseek-v41-dspark": 763 };
    const WORD = { fits: "fits", tight: "tight: runs, slower", "too big": "too big", "no engine": "needs an NVIDIA card" };
    const CLS = { fits: "v-fits", tight: "v-tight", "too big": "v-big", "no engine": "v-none" };
    const esc = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
    const num = (x) => (Number.isInteger(x) ? String(x) : x.toFixed(1));
    const badge = (fit) => `<span class="v ${CLS[fit]}">${WORD[fit]}</span>`;

    const machine = () => {
      const [vendor, gpu] = form.elements.gpu.value.split(":");
      return { m: F.machine(vendor, Number(gpu), Number(form.elements.ram.value)), vendor, gpu: Number(gpu), ram: Number(form.elements.ram.value) };
    };

    const render = () => {
      const { m, vendor, gpu, ram } = machine();
      let best = null;
      rows.innerHTML = F.MODELS.map((model) => {
        const v = F.verdict(model, m);
        const s = v.fit === "fits" || v.fit === "tight" ? F.speed(model.id, m) : null;
        if ((v.fit === "fits" || v.fit === "tight") && (!best || PARAMS[model.id] > PARAMS[best.model.id])) best = { model, s, fit: v.fit };
        const speed = s
          ? `<b>${s.about ? "~" : ""}${num(s.tok)}</b> tok/s<small>${esc(s.note)}</small>`
          : v.fit === "fits" || v.fit === "tight" ? "not measured on this size" : "";
        return `<tr class="${v.fit === "fits" || v.fit === "tight" ? "" : "off"}"><th scope="row">${esc(model.title)}<small>${esc(model.what)}</small></th>` +
          `<td>${badge(v.fit)}${v.why ? `<small>${esc(v.why)}</small>` : ""}</td>` +
          `<td class="${s ? "" : "none"}">${speed}</td><td><code>flelink run ${model.id}</code></td></tr>`;
      }).join("");

      const name = vendor === "apple" ? `Apple Silicon with ${ram} GB` : vendor === "none" ? `no GPU and ${ram} GB RAM` : `an NVIDIA card with ${gpu} GB and ${ram} GB RAM`;
      if (best) {
        big.textContent = PARAMS[best.model.id] + "B";
        what.textContent = `parameters is the biggest model in the catalog that ${best.fit === "tight" ? "runs (tight)" : "fits"} on ${name}: ${best.model.title}` +
          (best.s ? `, measured at ${best.s.about ? "about " : ""}${num(best.s.tok)} tok/s on this hardware.` : ". Nobody has measured its speed on this size yet.");
      } else {
        big.textContent = "0";
        what.textContent = `of these models fit on ${name}. A smaller GGUF may: try a size below.`;
      }
      any(m);
    };

    const any = (m) => {
      const gb = parseFloat(anySize.value);
      if (!(gb > 0)) { anyOut.textContent = "enter a size"; return; }
      const fit = F.fitSize(gb * F.GIB, false, m || machine().m);
      anyOut.innerHTML = `needs about ${num(Math.round(gb * 12.5) / 10)} GB: ${badge(fit)}`;
    };

    form.addEventListener("change", render);
    anySize.addEventListener("input", () => any());
    render();
  }

  // the engine slot: the layer stays, the engine underneath changes
  const engines = [...document.querySelectorAll("#engines .engine")];
  if (engines.length && !still) {
    let i = 0;
    setInterval(() => {
      engines[i].classList.remove("on");
      i = (i + 1) % engines.length;
      engines[i].classList.add("on");
    }, 2400);
  }
})();
