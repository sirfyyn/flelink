// The race on flelink.run: three engines write the same text at the decode speed each was measured at, at the same time.
// Measured: 91 (FLE), 74 (Strata) and 36 (llama.cpp) tok/s for Qwen3.8-Flash-Next on one RTX PRO 4500, median over six
// tasks, 6 Oct 2026. Illustration: the text. The scene's small print says so.
//
//   <section data-race> <div class="lane" data-tok="91"> .lane-text .lane-bar>i .lane-time </div> … <button data-again>
// Without JavaScript the lanes show their names and speeds, which is the statement.
(() => {
  "use strict";
  const box = document.querySelector("[data-race]");
  if (!box) return;
  const TEXT = "A mixture-of-experts model does not use all of itself for every word. Each layer holds hundreds of experts, and a router picks a few of them per token. The ones that are picked most often stay on the graphics card. The rest wait in memory, and the rarest on disk. When a token needs an expert that is not on the card, the CPU computes it while the GPU keeps going. A small draft head guesses the next words ahead; the model checks every guess, keeps the right ones and throws the others away. The answer is the same. It just arrives sooner. ";
  const toks = (TEXT + TEXT + TEXT).match(/\s*[^\s]{1,4}|\s+/g);
  const TOTAL = 420;                                     // tokens to write: FLE needs 4.6 s, llama.cpp 11.7 s
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const lanes = [...box.querySelectorAll(".lane")].map((el) => ({ el, tok: +el.dataset.tok, text: el.querySelector(".lane-text"), bar: el.querySelector(".lane-bar i"), time: el.querySelector(".lane-time"), done: 0, shown: -1 }));
  const fastest = lanes.reduce((a, l) => (l.tok > a.tok ? l : a));
  let t0 = 0, raf = 0;

  function paint(l, k) {
    if (k === l.shown) return;
    l.shown = k;
    l.text.textContent = toks.slice(Math.max(0, k - 46), k).join("").replace(/^\s+/, "");
    l.bar.style.width = (k / TOTAL) * 100 + "%";
  }
  function frame(now) {
    const t = (now - t0) / 1000;
    let open = false;
    for (const l of lanes) {
      const k = Math.min(TOTAL, Math.floor(t * l.tok));
      paint(l, k);
      if (k >= TOTAL) { if (!l.done) { l.done = t; l.el.classList.add("is-done"); l.time.textContent = "done in " + (TOTAL / l.tok).toFixed(1) + " s"; } }
      else { open = true; if (fastest.done && !l.time.textContent) l.time.textContent = "at " + Math.round((fastest.done * l.tok / TOTAL) * 100) + " % when FLE was done"; }
    }
    raf = open ? requestAnimationFrame(frame) : 0;
  }
  function start() {
    cancelAnimationFrame(raf);
    lanes.forEach((l) => { l.done = 0; l.shown = -1; l.time.textContent = ""; l.el.classList.remove("is-done"); paint(l, 0); });
    t0 = performance.now();
    raf = requestAnimationFrame(frame);
  }
  box.querySelector("[data-again]")?.addEventListener("click", start);
  // a tab in the background gets no frames: start over instead of jumping to the end
  document.addEventListener("visibilitychange", () => { if (!document.hidden && raf) start(); });

  if (still) { lanes.forEach((l) => { paint(l, TOTAL); l.time.textContent = "done in " + (TOTAL / l.tok).toFixed(1) + " s"; }); return; }
  if ("IntersectionObserver" in window) new IntersectionObserver((e, io) => { if (e[0].isIntersecting) { io.disconnect(); start(); } }, { threshold: 0.45 }).observe(box);
  else start();
})();
