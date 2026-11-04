// A model that repeats itself without end (2-bit quants do it now and then; reports of 100k-token answers and one sentence
// 1,110 times) fills the context and the minutes for nothing. This notices the loop in the text as it arrives, so chat can
// stop it. It reads text only: no engine setting changes, and an answer that does not loop is never touched.
// It is on by default; Settings → Appearance turns it off (kept in this browser only).
const KEY = "fle.loopguard";
const MIN_UNIT = 12;      // shortest repeated piece, in characters
const MAX_UNIT = 480;     // longest repeated piece: a whole paragraph
const MIN_REPEATS = 5;    // how often it must come back, one right after the other
const MIN_SPAN = 400;     // and the repeats together cover at least this much text
const WINDOW = 4800;      // only the end of the text is looked at

export function loopGuardOn() {
  try { return localStorage.getItem(KEY) !== "off"; } catch { return true; }
}

export function setLoopGuard(on) {
  try { localStorage.setItem(KEY, on ? "on" : "off"); } catch { /* storage blocked: stays on */ }
}

// true when the end of the text is one piece repeated back to back (MIN_REPEATS times or more)
export function repeatsItself(text) {
  const t = text.length > WINDOW ? text.slice(-WINDOW) : text;
  if (t.length < MIN_SPAN) return false;
  for (let unit = MIN_UNIT; unit <= MAX_UNIT && unit * MIN_REPEATS <= t.length; unit += 1) {
    const piece = t.slice(-unit);
    if (!/\S/.test(piece)) continue;                   // blank lines and spaces are not a loop
    let n = 1;
    while (t.length >= unit * (n + 1) && t.slice(t.length - unit * (n + 1), t.length - unit * n) === piece) n += 1;
    if (n >= MIN_REPEATS && n * unit >= MIN_SPAN) return true;
  }
  return false;
}
