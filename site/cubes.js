// The cubes of flelink.run: one cube is one GiB. Two scenes use them.
//
//   [data-scale]      the first screen. 232 cubes build the model (DeepSeek V4.1 Flash, 232 GiB), the card appears next to
//                     it (32), the busiest 32 fly over, then it answers at the measured speed, with or without the draft.
//   [data-fit-stage]  "your card": a pile as big as the model that fits, a card as big as the memory you chose
//                     (site.js calls FLECubes.fit(pile, card)).
//
// Measured: the sizes and the speeds (50.6 tok/s with the DSpark draft, 37.6 without; FLE v1.2.5, one RTX PRO 4500,
// 128 GB RAM). Illustration: which cubes move and light up. The scenes say so in their small print.
// Without JavaScript the canvases stay empty; the text next to them says the same in words.
(() => {
  "use strict";
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const COLD = ["#3a4458", "#232b3a", "#161c28"], HOT = ["#ff5fb0", "#e0177c", "#a80f5c"], PINK = "#ff2d95";
  const ease = (t) => (t <= 0 ? 0 : t >= 1 ? 1 : t * t * (3 - 2 * t));
  const NX = 8, NY = 6;                                  // a layer of the pile; the card is 8 wide and one cube high

  // A canvas with an isometric floor: x runs to the lower right, y to the lower left, z up.
  function stage(cv, place) {
    const ctx = cv.getContext("2d");
    const s = { ctx, W: 0, H: 0, u: 20, ox: 0, oy: 0 };
    s.layout = () => {
      const dpr = Math.min(2, window.devicePixelRatio || 1), r = cv.getBoundingClientRect();
      s.W = Math.max(100, r.width); s.H = Math.max(100, r.height);
      cv.width = Math.round(s.W * dpr); cv.height = Math.round(s.H * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      place(s);
    };
    s.iso = (x, y, z) => [s.ox + (x - y) * s.u * 0.86, s.oy + (x + y) * s.u * 0.5 - z * s.u];
    s.cube = (x, y, z, cols, alpha, heat) => {
      const i = s.iso, a = i(x, y, z + 1), b = i(x + 1, y, z + 1), c = i(x + 1, y + 1, z + 1), d = i(x, y + 1, z + 1), e = i(x + 1, y + 1, z), f = i(x, y + 1, z), g = i(x + 1, y, z);
      const face = (p, col) => { ctx.beginPath(); ctx.moveTo(p[0][0], p[0][1]); for (let k = 1; k < p.length; k++) ctx.lineTo(p[k][0], p[k][1]); ctx.closePath(); ctx.fillStyle = col; ctx.fill(); };
      ctx.globalAlpha = alpha;
      face([a, b, c, d], cols[0]); face([d, c, e, f], cols[1]); face([b, g, e, c], cols[2]);
      // a thin light edge on top, so a settled pile still reads as single cubes and not as one block
      ctx.globalAlpha = alpha * 0.22; ctx.strokeStyle = "#ffffff"; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(d[0], d[1]); ctx.lineTo(a[0], a[1]); ctx.lineTo(b[0], b[1]); ctx.stroke();
      ctx.globalAlpha = alpha * 0.5; ctx.strokeStyle = "#06080d";
      ctx.beginPath(); ctx.moveTo(d[0], d[1]); ctx.lineTo(c[0], c[1]); ctx.lineTo(b[0], b[1]); ctx.moveTo(c[0], c[1]); ctx.lineTo(e[0], e[1]); ctx.stroke();
      if (heat > 0.03) { ctx.globalAlpha = alpha * heat; face([a, b, c, d], "#ffffff"); }
      ctx.globalAlpha = 1;
    };
    // the card: an outline on the floor, `cells` cubes big (8 wide), dashed while it is empty
    s.card = (cx, cy, cells, alpha, dashed) => {
      if (cells <= 0 || alpha <= 0) return;
      const rows = Math.ceil(cells / NX), i = s.iso, p = [i(cx, cy, 0), i(cx + NX, cy, 0), i(cx + NX, cy + rows, 0), i(cx, cy + rows, 0)];
      ctx.beginPath(); ctx.moveTo(p[0][0], p[0][1]); for (let k = 1; k < 4; k++) ctx.lineTo(p[k][0], p[k][1]); ctx.closePath();
      ctx.globalAlpha = alpha * 0.13; ctx.fillStyle = PINK; ctx.fill();
      ctx.globalAlpha = alpha; ctx.strokeStyle = PINK; ctx.lineWidth = 2.5; ctx.setLineDash(dashed ? [6, 5] : []); ctx.stroke(); ctx.setLineDash([]);
      ctx.globalAlpha = 1;
    };
    s.glow = (on) => { ctx.shadowColor = PINK; ctx.shadowBlur = on ? 18 * on : 0; };
    return s;
  }
  // run a frame function only while its element is on screen and the tab is visible
  function loop(el, frame) {
    let raf = 0, seen = false, last = 0;
    const tick = (now) => { raf = 0; if (!seen || document.hidden) return; const dt = Math.min(0.1, (now - last) / 1000); last = now; if (frame(dt) !== false) raf = requestAnimationFrame(tick); };
    const go = () => { if (!raf && seen && !document.hidden) { last = performance.now(); raf = requestAnimationFrame(tick); } };
    document.addEventListener("visibilitychange", go);
    if ("IntersectionObserver" in window) new IntersectionObserver((e) => { seen = e[0].isIntersecting; go(); }, { threshold: 0.15 }).observe(el);
    else { seen = true; go(); }
    return go;
  }
  const spot = (i) => ({ x: i % NX, y: Math.floor(i / NX) % NY, z: Math.floor(i / (NX * NY)) });

  /* ---------- the first screen ---------- */
  (() => {
    const box = document.querySelector("[data-scale]");
    if (!box) return;
    const cv = box.querySelector("canvas"), say = box.querySelector("[data-say]"), rateEl = box.querySelector("[data-rate]");
    const modes = [...box.querySelectorAll("[data-mode]")];
    const N = 232, CARD = 32, RATE = { draft: 50.6, exact: 37.6 };
    const STEPS = [[0, "This is the model."], [3.0, "This is your card."], [5.0, "FLE moves the busiest experts onto it."], [8.2, "Now it answers."]];
    const T_CARD = 3.0, T_FLY = 5.0, T_RUN = 8.2, CX = NX + 2, CY = -3;
    const cubes = [];
    for (let i = 0; i < N; i++) cubes.push({ ...spot(i), hot: -1, heat: 0 });
    // the busiest: scattered over the upper layers, so they are seen leaving
    cubes.map((c, i) => i).sort((p, q) => (cubes[q].z * 7 + ((q * 37) % 11)) - (cubes[p].z * 7 + ((p * 37) % 11))).slice(0, CARD).forEach((i, k) => { cubes[i].hot = k; });
    const hot = cubes.filter((c) => c.hot >= 0), cold = cubes.filter((c) => c.hot < 0);
    const s = stage(cv, (st) => {
      const narrow = st.W < 900;
      // the pile and the card together are about 23 cubes wide and 17 high; the words keep the upper left
      st.u = Math.max(8, Math.min(st.W / (narrow ? 34 : 39), st.H / (narrow ? 32 : 28)));
      st.ox = st.W * (narrow ? 0.36 : 0.54); st.oy = st.H * (narrow ? 0.46 : 0.5);
    });
    // ?t=9 starts later in the sequence (for looking at a state while building)
    let clock = parseFloat(new URLSearchParams(location.search).get("t")) || 0, step = -1, mode = "draft", rate = 0, due = 0;

    function draw(t) {
      s.ctx.clearRect(0, 0, s.W, s.H);
      s.card(CX, CY, CARD, ease((t - T_CARD) / 0.8), t < T_FLY + 1.2);
      const list = [], dim = ease((t - T_FLY - 0.4) / 1.5);
      for (const c of cubes) {
        const born = ease((t - (c.z * 0.42 + (c.x + c.y) * 0.03)) / 0.5);
        if (born <= 0) continue;
        let x = c.x, y = c.y, z = c.z + (1 - born) * 9, fly = 0;
        if (c.hot >= 0) {
          fly = ease((t - T_FLY - c.hot * 0.07) / 1.1);
          if (fly > 0) { x += (CX + (c.hot % NX) - x) * fly; y += (CY + Math.floor(c.hot / NX) - y) * fly; z = z * (1 - fly) + Math.sin(fly * Math.PI) * 5; }
        }
        list.push({ c, x, y, z, fly, born, key: (fly > 0 && fly < 1 ? 1e6 : 0) + (x + y) * 10 + z });
      }
      list.sort((p, q) => p.key - q.key);
      for (const o of list) {
        const isHot = o.fly > 0;
        s.glow(isHot ? o.fly : 0);
        s.cube(o.x, o.y, o.z, isHot ? HOT : COLD, o.born * (isHot ? 1 : 1 - dim * 0.5), o.c.heat);
      }
      s.glow(0);
    }
    function label(t) {
      const k = STEPS.reduce((a, x, i) => (t >= x[0] ? i : a), 0);
      if (k !== step) { step = k; say.textContent = STEPS[k][1]; box.dataset.step = String(k); }
    }
    modes.forEach((b) => b.addEventListener("click", () => {
      mode = b.dataset.mode;
      modes.forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
      if (still) rateEl.textContent = RATE[mode].toFixed(1);
      else if (clock < T_RUN) clock = T_RUN;             // someone who presses the switch wants to see it run
    }));
    box.querySelector("[data-replay]")?.addEventListener("click", () => { clock = 0; rate = 0; rateEl.textContent = "0.0"; go(); });

    s.layout();
    if ("ResizeObserver" in window) new ResizeObserver(() => { s.layout(); draw(still ? 99 : clock); }).observe(cv);
    if (still) { label(99); rateEl.textContent = RATE[mode].toFixed(1); hot.slice(0, 9).forEach((c) => { c.heat = 0.8; }); draw(99); return; }
    rateEl.textContent = "0.0";
    label(clock);
    const go = loop(box, (dt) => {
      clock += dt; label(clock);
      for (const c of cubes) if (c.heat > 0.03) c.heat *= Math.pow(0.84, dt * 60);
      if (clock > T_RUN) {
        due += dt * RATE[mode];
        // with the draft, tokens arrive a few at a time (up to five are drafted and checked together); without, one by one
        const burst = mode === "draft" ? 1 + Math.floor(Math.random() * 4) : 1;
        if (due >= burst) { due -= burst; for (let k = 0; k < burst; k++) { const pool = Math.random() < 0.72 ? hot : cold; pool[Math.floor(Math.random() * pool.length)].heat = 1; } }
        if (rate !== RATE[mode]) { rate += (RATE[mode] - rate) * Math.min(1, dt * 3.5); if (Math.abs(rate - RATE[mode]) < 0.05) rate = RATE[mode]; rateEl.textContent = rate.toFixed(1); }
      }
      draw(clock);
    });
  })();

  /* ---------- your card ---------- */
  const fit = (() => {
    const cv = document.querySelector("[data-fit-stage]");
    if (!cv) return () => {};
    const s = stage(cv, (st) => {
      const narrow = st.W < 900;
      st.u = Math.max(7, Math.min(st.W / (narrow ? 34 : 44), st.H / (narrow ? 34 : 26)));
      st.ox = st.W * (narrow ? 0.36 : 0.62); st.oy = st.H * (narrow ? 0.44 : 0.5);
    });
    const CX = NX + 2, CY = -2;
    let pile = 0, card = 0, on = 0, t = 0, from = 0;     // `from`: the pile before the change, so cubes only appear or leave
    function draw(time) {
      s.ctx.clearRect(0, 0, s.W, s.H);
      s.card(CX, CY, card, 1, on < card);
      const most = Math.max(pile, from), list = [];
      for (let i = 0; i < most; i++) {
        const stays = i < pile && i < from, comes = i < pile && i >= from, p = spot(i);
        const a = stays ? 1 : comes ? ease((time - (i - from) * 0.004) / 0.45) : 1 - ease(time / 0.35);
        if (a <= 0) continue;
        // the last `on` cubes of the pile lie on the card (pink); the rest stays in the pile
        const k = i - (pile - on);
        let { x, y, z } = p, isHot = false;
        if (i < pile && k >= 0) { x = CX + (k % NX); y = CY + Math.floor(k / NX); z = 0; isHot = true; }
        list.push({ x, y, z: z + (comes ? (1 - a) * 6 : 0), a, isHot, key: (x + y) * 10 + z });
      }
      list.sort((p, q) => p.key - q.key);
      for (const o of list) { s.glow(o.isHot ? 0.8 : 0); s.cube(o.x, o.y, o.z, o.isHot ? HOT : COLD, o.a, 0); }
      s.glow(0);
    }
    s.layout();
    if ("ResizeObserver" in window) new ResizeObserver(() => { s.layout(); draw(9); }).observe(cv);
    const go = loop(cv, (dt) => { t += dt; draw(t); return t < 1.6; });
    return (newPile, newCard) => {
      from = pile; pile = Math.max(0, Math.round(newPile)); card = Math.max(0, Math.round(newCard)); on = Math.min(pile, card);
      t = 0;
      if (still) { from = pile; draw(9); } else { draw(0); go(); }
    };
  })();

  window.FLECubes = { fit };
})();
