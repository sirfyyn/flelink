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

// A second kind of loop (ISTA IQ2_XXS, greedy, thinking a Pagoda in three.js, 16000 tokens, 05.11.2026): the sentences are not
// the same but have the same shape, only the numbers change ("Need maybe add `rocks.push(createRockCluster(1, -21.0, 1, 0.00005));`
// etc. Good." again and again). Normalised by turning every number into #, 48 of the last 50 sentences were one and the
// same; in answers that did not loop the most frequent one came 5 times in 44 at most.
const SOFT_WINDOW = 4000;  // only the end of the text is looked at
const SOFT_MIN_PARTS = 24; // fewer sentences than this: too little to tell
const SOFT_MIN_SEG = 25;   // shorter pieces are ignored
const SOFT_MIN_COUNT = 14; // the same shape this often ...
const SOFT_MIN_SHARE = 0.4; // ... and as a share of all the sentences in the window

export function softRepeats(text) {
  const t = text.length > SOFT_WINDOW ? text.slice(-SOFT_WINDOW) : text;
  const parts = t.split(/(?<=[.!?;])\s+|\n+/).map((x) => x.trim()).filter((x) => x.length >= SOFT_MIN_SEG);
  if (parts.length < SOFT_MIN_PARTS) return false;
  const count = new Map();
  let top = 0;
  for (const p of parts) {
    const k = p.replace(/-?\d+(\.\d+)?/g, "#").replace(/\s+/g, " ");
    const n = (count.get(k) || 0) + 1;
    count.set(k, n);
    if (n > top) top = n;
  }
  return top >= SOFT_MIN_COUNT && top / parts.length >= SOFT_MIN_SHARE;
}

export function loopGuardOn() {
  try { return localStorage.getItem(KEY) !== "off"; } catch { return true; }
}

export function setLoopGuard(on) {
  try { localStorage.setItem(KEY, on ? "on" : "off"); } catch { /* storage blocked: stays on */ }
}

// true when the end of the text is one piece repeated back to back (MIN_REPEATS times or more), or one sentence shape over and over
// with only the numbers changing (softRepeats)
export function repeatsItself(text) {
  if (softRepeats(text)) return true;
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
