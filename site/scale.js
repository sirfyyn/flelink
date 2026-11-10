// The first screen of flelink.run: the model drawn to scale. One square is one GiB of DeepSeek V4.1 Flash (232 GiB):
// 32 where the card is, 90 in the RAM expert store (the recipe's FLE_MOE_STORE_GB=90), the rest read from NVMe.
// Then it answers, at the speed that was measured, with or without the draft model.
//
// What is measured: the sizes, 50.6 tok/s with the DSpark draft, 37.6 without (FLE v1.2.5, one RTX PRO 4500, 120 GB RAM).
// What is illustration: which squares light up, and that drafted tokens arrive in small bursts. The page says so.
//
//   <div class="scale" data-scale>
//     <p data-say></p>  <canvas></canvas>  <b data-rate></b>  <p data-out></p>
//     <button data-mode="draft" aria-pressed="true">, <button data-mode="exact" aria-pressed="false">
//   </div>
// Without JavaScript the canvas stays empty and the text next to it says the same in words.
(() => {
  "use strict";
  const box = document.querySelector("[data-scale]");
  if (!box) return;
  const cv = box.querySelector("canvas"), ctx = cv.getContext("2d");
  const say = box.querySelector("[data-say]"), out = box.querySelector("[data-out]"), rateEl = box.querySelector("[data-rate]");
  const modes = [...box.querySelectorAll("[data-mode]")];
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;

  const N = 232, CARD = 32, RAM = 90;
  const RATE = { draft: 50.6, exact: 37.6 };
  const STEPS = [
    [0.0, "This is the model: 232 GiB."],
    [1.9, "This is your card: 32 GB."],
    [3.6, "FLE keeps the busiest experts on it, 90 GiB more in RAM, and reads the rest from NVMe."],
    [5.6, "Now it answers."],
  ];
  const T_CARD = 1.9, T_ZONES = 3.6, T_RUN = 5.6;
  const TEXT = "A mixture-of-experts model does not use all of itself for every word. Each layer holds hundreds of experts, and a router picks a few of them per token. The ones that are picked most often stay on the graphics card. The rest wait in memory, and the rarest on disk. When a token needs an expert that is not on the card, the CPU computes it while the GPU keeps going. A small draft model guesses the next few words ahead; the big model checks every guess, keeps the right ones and throws the others away. The answer is the same as without the draft. It just arrives sooner. That is how 232 gibibytes of model answer on a card with 32. ";
  const toks = TEXT.match(/\s*[^\s]{1,4}|\s+/g);

  const heat = new Float32Array(N);
  let W = 0, H = 0, cols = 1, size = 8, gap = 3, x0 = 0, y0 = 0;
  let mode = "draft", t0 = 0, last = 0, clock = 0, due = 0, made = 0, step = -1, shownRate = 0, running = false, visible = true, raf = 0;

  function layout() {
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    const r = cv.getBoundingClientRect();
    W = Math.max(120, r.width); H = Math.max(80, r.height);
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    gap = W < 520 ? 2 : 3;
    let best = null;                                  // the largest square whose grid still fits
    for (let c = 8; c <= 58; c++) {
      const rows = Math.ceil(N / c), s = Math.floor(Math.min((W - (c - 1) * gap) / c, (H - (rows - 1) * gap) / rows));
      if (s >= 4 && (!best || s > best.s)) best = { c, s };
    }
    cols = best ? best.c : 29; size = best ? best.s : 6;
    x0 = Math.floor((W - (cols * size + (cols - 1) * gap)) / 2); y0 = 0;
  }
  const zone = (i) => (i < CARD ? 0 : i < CARD + RAM ? 1 : 2);

  function pick() {                                   // illustration: most tokens are served from the card
    const r = Math.random();
    if (r < 0.68) return Math.floor(Math.random() * CARD);
    if (r < 0.93) return CARD + Math.floor(Math.random() * RAM);
    return CARD + RAM + Math.floor(Math.random() * (N - CARD - RAM));
  }
  function emit(n) {
    for (let k = 0; k < n; k++) { heat[pick()] = 1; if ((made + k) % 2) heat[pick()] = 1; }
    made += n;
    const from = Math.max(0, made - 70);
    let s = "";
    for (let i = from; i < made; i++) s += toks[i % toks.length];
    out.textContent = s.replace(/^\s+/, "");
  }

  function draw(t) {
    ctx.clearRect(0, 0, W, H);
    const card = Math.min(1, Math.max(0, (t - T_CARD) / 0.7)), zones = Math.min(1, Math.max(0, (t - T_ZONES) / 0.9));
    for (let i = 0; i < N; i++) {
      const x = x0 + (i % cols) * (size + gap), y = y0 + Math.floor(i / cols) * (size + gap), z = zone(i);
      const born = Math.min(1, Math.max(0, (t - i * 0.007) / 0.35));
      if (born <= 0) continue;
      // before the zones are named every square is the same; then RAM and NVMe step back
      const base = z === 0 ? 0.34 + 0.0 * card : z === 1 ? 0.34 - 0.12 * zones : 0.34 - 0.25 * zones;
      ctx.globalAlpha = born;
      ctx.fillStyle = `rgba(148,160,178,${base.toFixed(3)})`;
      ctx.fillRect(x, y, size, size);
      if (z === 0 && card > 0) {
        ctx.globalAlpha = born * card;
        ctx.fillStyle = "rgba(255,45,149,.30)"; ctx.fillRect(x, y, size, size);
        ctx.strokeStyle = "#ff2d95"; ctx.lineWidth = 1.5; ctx.strokeRect(x + 0.75, y + 0.75, size - 1.5, size - 1.5);
      }
      const h = heat[i];
      if (h > 0.02) { ctx.globalAlpha = h; ctx.fillStyle = z === 0 ? "#ff2d95" : "#ffffff"; ctx.fillRect(x, y, size, size); }
    }
    ctx.globalAlpha = 1;
  }

  function frame(now) {
    raf = 0;
    if (!running) return;
    const dt = Math.min(0.1, (now - last) / 1000); last = now; clock += dt;
    for (let i = 0; i < N; i++) if (heat[i] > 0.02) heat[i] *= Math.pow(0.86, dt * 60);
    const s = STEPS.reduce((a, x, i) => (clock >= x[0] ? i : a), 0);
    if (s !== step) { step = s; say.textContent = STEPS[s][1]; box.dataset.step = String(s); }
    if (clock >= T_RUN) {
      due += dt * RATE[mode];
      // with the draft, tokens arrive a few at a time (up to five are drafted and checked together); without, one by one
      const burst = mode === "draft" ? 1 + Math.floor(Math.random() * 4) : 1;
      if (due >= burst) { emit(burst); due -= burst; }
      if (shownRate !== RATE[mode]) { shownRate += (RATE[mode] - shownRate) * Math.min(1, dt * 6); if (Math.abs(shownRate - RATE[mode]) < 0.05) shownRate = RATE[mode]; rateEl.textContent = shownRate.toFixed(1); }
    }
    draw(clock);
    raf = requestAnimationFrame(frame);
  }
  function go() { if (running && !raf && visible && !document.hidden) { last = performance.now(); raf = requestAnimationFrame(frame); } }

  modes.forEach((b) => b.addEventListener("click", () => {
    mode = b.dataset.mode;
    modes.forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
    if (still) { rateEl.textContent = RATE[mode].toFixed(1); }
    if (clock < T_RUN) clock = T_RUN;                 // someone who presses the switch wants to see it run
  }));

  layout();
  if ("ResizeObserver" in window) new ResizeObserver(() => { layout(); if (!running || still) draw(still ? 99 : clock); }).observe(cv);
  else window.addEventListener("resize", layout);

  if (still) {                                        // no motion: the end state, a few squares lit, the full sentence
    say.textContent = STEPS[2][1]; box.dataset.step = "3";
    for (let i = 0; i < 26; i++) heat[pick()] = 0.35 + Math.random() * 0.65;
    rateEl.textContent = RATE[mode].toFixed(1);
    out.textContent = toks.slice(0, 70).join("");
    draw(99);
    return;
  }
  rateEl.textContent = "0.0";
  out.textContent = "";                               // the text in the page is for readers without JavaScript
  running = true;
  document.addEventListener("visibilitychange", go);
  if ("IntersectionObserver" in window) new IntersectionObserver((e) => { visible = e[0].isIntersecting; if (!visible && raf) { cancelAnimationFrame(raf); raf = 0; } go(); }).observe(box);
  go();
})();
