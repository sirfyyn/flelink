// Demo mode (server demo.rs): steps from outside, done the way a person would, so a recording shows the real Studio.
// Only when the Studio runs with --demo (or FLE_DEMO=1); otherwise this asks once and stops.

import { get, post } from "./api.js";
import { navigate, store } from "./app.js";

const warte = (ms) => new Promise((r) => setTimeout(r, ms));
// an answer is being written while the send button is the stop button (chat.js sets its title)
const beschaeftigt = () => !!document.querySelector('button.send[title^="Stop"]');

async function tippen(ta, text, ms) {
  ta.focus();
  ta.value = "";
  for (const ch of text) {
    ta.value += ch;
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    if (ms) await warte(ms);
  }
}

async function chat(s) {
  if (location.pathname !== "/") { navigate("/"); await warte(800); }
  // answer length and temperature for this message, as the user would set them in Settings
  const c = {};
  if (s.max_tokens != null) c.max_tokens = s.max_tokens;
  if (s.temperature != null) Object.assign(c, { sampling: "custom", temperature: s.temperature });
  if (Object.keys(c).length) store.state.settings = await post("/api/settings", { chat: c });
  const ta = document.querySelector('textarea[aria-label="Message"]');
  if (!ta) return;
  await tippen(ta, s.text, s.tippen_ms);
  await warte(300);
  ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
}

const AKTION = {
  chat,
  neu: async () => { if (location.pathname !== "/") { navigate("/"); await warte(600); }
    [...document.querySelectorAll("button")].find((b) => /New chat/.test(b.textContent))?.click(); },
  seite: async (s) => navigate(s.pfad),
  vorschau: async () => { const b = [...document.querySelectorAll("button")].filter((x) => /^\s*Preview\s*$/.test(x.textContent)).pop();
    b?.scrollIntoView({ block: "center" }); b?.click(); },
  scroll: async (s) => { (document.querySelector("main") || document.scrollingElement).scrollBy({ top: s.dy, behavior: "smooth" }); },
};

export async function startDemo() {
  const st = await get("/api/demo").catch(() => null);
  if (!st?.on) return;
  document.documentElement.dataset.demo = "1";
  for (;;) {
    const r = await post("/api/demo/next", { busy: beschaeftigt() }).catch(() => null);
    const s = r?.schritt;
    if (s && AKTION[s.aktion]) { try { await AKTION[s.aktion](s); } catch (e) { console.warn("demo", e); } }
    await warte(s ? 200 : 500);
  }
}
