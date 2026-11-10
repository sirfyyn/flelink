// Small things only: the install tabs, the copy button, the first-run replay and the engine slot. The page reads fine without it.
(() => {
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;

  // install command: macOS/Linux or Windows; Windows visitors start on their tab
  // every copy of the command on the page follows the tab (hero and the closing call)
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

  // the first run, replayed line by line once it is on screen (the full text is in the page from the start)
  const term = document.getElementById("term");
  if (term && !still && "IntersectionObserver" in window) {
    const lines = term.innerHTML.split("\n");
    let shown = 0;
    const step = () => {
      shown += 1;
      term.innerHTML = lines.slice(0, shown).join("\n") + (shown < lines.length ? '\n<span class="cursor"></span>' : "");
      if (shown < lines.length) {
        const l = lines[shown - 1];
        const wait = l.includes("100 %") ? 700 : l.includes("starting") ? 1100 : l.trim() === "" ? 160 : 260;
        setTimeout(step, wait);
      } else {
        setTimeout(() => { shown = 1; step(); }, 9000);
      }
    };
    const io = new IntersectionObserver((e) => {
      if (e[0].isIntersecting) { io.disconnect(); shown = 0; step(); }
    });
    io.observe(term);
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
