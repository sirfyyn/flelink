// The Studio in other languages (Max 04.11.2026: German, French, Spanish, Chinese, Japanese, Korean, Russian). The page
// keeps writing English; this layer swaps the texts it shows for the chosen language as they appear (text nodes and the title, placeholder and aria-label
// attributes), so no page has to know about languages. Chat messages, code, logs and inputs are never touched.
// The language follows the browser unless one is picked in Settings (kept in this browser only).
// the languages there are, in their own words (the Settings list); each has i18n-<code>.js with T (texts) and P (patterns)
export const LANGUAGES = [["en", "English"], ["de", "Deutsch"], ["fr", "Français"], ["es", "Español"], ["zh", "中文"],
  ["ja", "日本語"], ["ko", "한국어"], ["ru", "Русский"]];
const CODES = LANGUAGES.map(([c]) => c);
let DICT = {};
let PATTERNS = [];

const KEY = "fle.lang";
const SKIP = "pre, code, textarea, input, .msg, .markdown, .mono, .log, [data-no-i18n]";
const ATTRS = ["title", "placeholder", "aria-label"];

export function language() {
  let pick = "auto";
  try { pick = localStorage.getItem(KEY) || "auto"; } catch { /* storage blocked */ }
  if (CODES.includes(pick)) return pick;
  const nav = (navigator.language || "en").toLowerCase().slice(0, 2);
  return CODES.includes(nav) ? nav : "en";
}

export function chosenLanguage() {
  try { return localStorage.getItem(KEY) || "auto"; } catch { return "auto"; }
}

export function setLanguage(v) {
  try { localStorage.setItem(KEY, v); } catch { /* storage blocked */ }
  location.reload();
}

function translate(text) {
  const t = text.trim();
  if (!t) return null;
  const hit = DICT[t];
  if (hit !== undefined) return text.replace(t, hit);
  for (const [re, to] of PATTERNS) {
    if (re.test(t)) return text.replace(t, t.replace(re, to));
  }
  return null;
}

const done = new WeakMap(); // node -> the German text we put there (so our own change is not translated again)

function onText(n) {
  if (done.get(n) === n.nodeValue) return;
  const p = n.parentElement;
  if (!p || p.closest(SKIP)) return;
  const de = translate(n.nodeValue);
  if (de !== null && de !== n.nodeValue) {
    done.set(n, de);
    n.nodeValue = de;
  }
}

function onAttrs(el) {
  if (el.closest(SKIP + ", .msg") && el.tagName !== "TEXTAREA" && el.tagName !== "INPUT") return;
  for (const a of ATTRS) {
    const v = el.getAttribute(a);
    if (!v) continue;
    const de = translate(v);
    if (de !== null && de !== v) el.setAttribute(a, de);
  }
}

function walk(root) {
  if (root.nodeType === 3) return onText(root);
  if (root.nodeType !== 1) return;
  onAttrs(root);
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT);
  let n;
  while ((n = w.nextNode())) {
    if (n.nodeType === 3) onText(n); else onAttrs(n);
  }
}

export async function startI18n() {
  const lang = language();
  document.documentElement.lang = lang;
  if (lang === "en") return;
  try {
    const m = await import(`./i18n-${lang}.js`);
    DICT = m.T;
    PATTERNS = m.P;
  } catch { return; } // a missing file: English
  walk(document.body);
  new MutationObserver((list) => {
    for (const m of list) {
      if (m.type === "characterData") onText(m.target);
      else if (m.type === "attributes") onAttrs(m.target);
      else m.addedNodes.forEach(walk);
    }
  }).observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ATTRS });
}
