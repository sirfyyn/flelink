// Plays the terminal recordings on flelink.run: real asciicast v2 files (site/casts/*.cast), in their recorded time.
// No library: the recordings only use \r, \n and five colour codes, so a small line buffer is enough.
//
//   <figure class="cast" data-cast="casts/a.cast casts/b.cast" data-labels="first|then" data-start="5.1" data-autoplay>
//     <pre class="cast-screen">the end state, in the page from the start (no JavaScript, search engines)</pre>
//   </figure>
//
// data-cast     one or more recordings, played one after the other (the screen is cleared between them)
// data-labels   one caption per recording, separated by |
// data-start    seconds until the command is entered; the stopwatch counts from there (default 0)
// data-autoplay start when the figure scrolls into view (never with prefers-reduced-motion: the end state stays)
// The controls are built here; without JavaScript the <pre> simply shows the end of the recording.
(() => {
  "use strict";
  const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
  // SGR code -> class. Everything else resets.
  const STYLE = { "1;35": "t-m", "36": "t-c", "2": "t-d", "1;32": "t-g", "1": "t-b" };
  const esc = (s) => s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c]);
  const clock = (s) => {
    s = Math.round(Math.max(0, s));
    return Math.floor(s / 60) + ":" + String(s % 60).padStart(2, "0");
  };

  function parse(text) {
    const lines = text.split("\n").filter((l) => l.trim());
    const events = lines.slice(1).map((l) => JSON.parse(l)).filter((e) => e[1] === "o");
    return { events, duration: events.length ? events[events.length - 1][0] : 0 };
  }

  // The screen: logical lines of [char, class] cells. \r goes to the start of the line, \n starts a new one; a write
  // replaces what is there, like a terminal. Long lines are wrapped by CSS, so the same recording fits a phone.
  class Screen {
    constructor() { this.reset(); }
    reset() { this.lines = [[]]; this.col = 0; this.style = ""; }
    write(data) {
      for (let i = 0; i < data.length; i++) {
        const ch = data[i];
        if (ch === "\x1b") {
          const m = /^\x1b\[([0-9;?]*)([A-Za-z])/.exec(data.slice(i, i + 16));
          if (m) { if (m[2] === "m") this.style = STYLE[m[1]] || ""; i += m[0].length - 1; continue; }
        }
        if (ch === "\r") this.col = 0;
        else if (ch === "\n") { this.lines.push([]); this.col = 0; }
        else {
          const line = this.lines[this.lines.length - 1];
          while (line.length < this.col) line.push([" ", ""]);
          line[this.col++] = [ch, this.style];
        }
      }
    }
    // The last `rows` lines as HTML. `narrow`: progress bars are shortened so a line still fits a small screen.
    html(rows, narrow) {
      return this.lines.slice(-rows).map((line) => {
        let out = "", run = "", cls = null;
        const flush = () => { if (run) out += cls ? `<span class="${cls}">${esc(run)}</span>` : esc(run); run = ""; };
        for (const [ch, c] of line) { if (c !== cls) { flush(); cls = c; } run += ch; }
        flush();
        out = out.replace(/\s+(<\/span>)?$/, "$1");
        if (narrow) out = out.replace(/━{8,}/g, "━━━━").replace(/#{8,}/g, "####").replace(/ {3,}/g, "  ");
        return out;
      }).join("\n");
    }
  }

  function setup(fig) {
    const urls = fig.dataset.cast.trim().split(/\s+/);
    const labels = (fig.dataset.labels || "").split("|");
    const start = parseFloat(fig.dataset.start || "0");
    const pre = fig.querySelector(".cast-screen");
    const rows = parseInt(fig.dataset.rows || "26", 10);
    const screen = new Screen();
    let casts = null, total = 0, t = 0, shown = -1, playing = false, rate = 1, last = 0, raf = 0, loading = null, applied = 0, part = -1;

    // controls
    const bar = document.createElement("div");
    bar.className = "cast-bar";
    bar.innerHTML =
      '<button type="button" class="cast-play" aria-label="Play the recording">Play</button>' +
      '<span class="cast-clock" aria-hidden="true"><b>0:00</b><i></i></span>' +
      '<input class="cast-seek" type="range" min="0" max="1000" value="0" aria-label="Position in the recording">' +
      '<button type="button" class="cast-rate" aria-label="Playback speed">1×</button>';
    const caption = document.createElement("p");
    caption.className = "cast-label";
    fig.insertBefore(bar, pre);
    if (labels[0]) fig.insertBefore(caption, pre);
    const [btn, clockEl, seek, rateBtn] = [bar.querySelector(".cast-play"), bar.querySelector(".cast-clock"), bar.querySelector(".cast-seek"), bar.querySelector(".cast-rate")];
    const marks = fig.querySelector(".cast-marks");
    const bigClock = fig.dataset.clock ? document.querySelector(fig.dataset.clock) : null;   // a large stopwatch outside the figure

    const load = () => loading || (loading = Promise.all(urls.map((u) => fetch(u).then((r) => { if (!r.ok) throw new Error(u); return r.text(); }).then(parse)))
      .then((list) => {
        let at = 0;
        casts = list.map((c) => { const o = { ...c, at }; at += c.duration + 1.2; return o; });
        total = at - 1.2;
        clockEl.lastElementChild.textContent = " / " + clock(total - start);
        if (marks) chapters();
      }));

    // Chapter buttons: where a known line first appears in the recording.
    function chapters() {
      marks.querySelectorAll("[data-find]").forEach((b) => {
        const hit = casts[0].events.find((e) => e[2].includes(b.dataset.find));
        if (!hit) { b.hidden = true; return; }
        const at = Math.max(0, hit[0] - 0.4);
        b.querySelector("i").textContent = clock(at - start);
        b.addEventListener("click", () => { go(at); play(); });
      });
    }

    // Bring the screen to time `to`. Forward: only the new events; backward: from the start of that recording.
    function go(to) {
      t = Math.max(0, Math.min(total, to));
      let p = casts.length - 1;
      while (p > 0 && t < casts[p].at) p--;
      const c = casts[p], local = t - c.at;
      if (p !== part || local < shown) { screen.reset(); applied = 0; part = p; if (labels[p] !== undefined) caption.textContent = labels[p]; }
      while (applied < c.events.length && c.events[applied][0] <= local) screen.write(c.events[applied++][2]);
      shown = local;
      draw();
    }
    function draw() {
      pre.innerHTML = screen.html(rows, pre.clientWidth < 620) + (playing && t < total ? '<span class="cast-cur"></span>' : "");
      pre.scrollTop = pre.scrollHeight; // like a terminal: from the top while it fits, then the newest lines stay in view
      clockEl.firstElementChild.textContent = clock(t - start);
      if (bigClock) bigClock.textContent = clock(t - start);
      seek.value = total ? String(Math.round((t / total) * 1000)) : "0";
      fig.classList.toggle("is-done", t >= total);
      fig.classList.toggle("is-waiting", t < start);
    }
    function tick(now) {
      if (!playing) return;
      go(t + ((now - last) / 1000) * rate);
      last = now;
      if (t >= total) { pause(); return; }
      raf = requestAnimationFrame(tick);
    }
    function play() {
      load().then(() => {
        if (t >= total) { shown = Infinity; go(0); }
        if (playing) return;
        playing = true; last = performance.now();
        btn.textContent = "Pause"; btn.setAttribute("aria-label", "Pause the recording");
        fig.classList.add("is-playing");
        raf = requestAnimationFrame(tick);
      }).catch(() => { btn.textContent = "Recording not available"; btn.disabled = true; });
    }
    function pause() {
      playing = false; cancelAnimationFrame(raf);
      btn.textContent = t >= total ? "Replay" : "Play"; btn.setAttribute("aria-label", "Play the recording");
      fig.classList.remove("is-playing");
      if (casts) draw();
    }

    btn.addEventListener("click", () => (playing ? pause() : play()));
    rateBtn.addEventListener("click", () => { rate = rate === 1 ? 4 : rate === 4 ? 16 : 1; rateBtn.textContent = rate + "×"; });
    seek.addEventListener("input", () => load().then(() => { shown = Infinity; go((seek.value / 1000) * total); }));
    // A tab in the background gets no frames: do not jump ahead when it comes back.
    document.addEventListener("visibilitychange", () => { last = performance.now(); });

    fig.classList.add("is-ready");
    if ("autoplay" in fig.dataset && !still && "IntersectionObserver" in window) {
      const io = new IntersectionObserver((e) => { if (e[0].isIntersecting) { io.disconnect(); play(); } }, { threshold: 0.35 });
      io.observe(fig);
    }
    return { play, pause, fig };
  }

  const players = [...document.querySelectorAll("figure.cast[data-cast]")].map(setup);

  // Tabs for the "broken on purpose" recordings: one panel visible, its recording starts when it is chosen.
  document.querySelectorAll("[data-cast-tabs]").forEach((box) => {
    const tabs = [...box.querySelectorAll("[role=tab]")];
    const show = (id, run) => {
      tabs.forEach((tab) => {
        const on = tab.getAttribute("aria-controls") === id;
        tab.setAttribute("aria-selected", String(on));
        tab.tabIndex = on ? 0 : -1;
        const panel = document.getElementById(tab.getAttribute("aria-controls"));
        panel.hidden = !on;
        const p = players.find((x) => panel.contains(x.fig));
        if (p) (on && run && !still ? p.play() : p.pause());
      });
    };
    tabs.forEach((tab, i) => {
      tab.addEventListener("click", () => show(tab.getAttribute("aria-controls"), true));
      tab.addEventListener("keydown", (e) => {
        const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
        if (!d) return;
        e.preventDefault();
        const next = tabs[(i + d + tabs.length) % tabs.length];
        next.focus(); show(next.getAttribute("aria-controls"), true);
      });
    });
    show(tabs[0].getAttribute("aria-controls"), false);
  });
})();
