// Run: node tests/loop-guard.test.mjs   (or deno run --allow-read tests/loop-guard.test.mjs)
// The fixtures are the last 4800 characters of real answers of ISTA IQ2_XXS (FLE v1.2.5, 05.11.2026): one that had fallen into a
// loop whose sentences differ only in their numbers, and six that did not (long thinking, no loop).
import { readFileSync } from "node:fs";
import { repeatsItself, softRepeats } from "../web/studio/js/loop-guard.js";

const fx = JSON.parse(readFileSync(new URL("./fixtures/loop-tails.json", import.meta.url), "utf8"));
let bad = 0;
const check = (name, got, want) => { if (got !== want) { bad += 1; console.log("FAIL", name, "got", got); } };
for (const [name, v] of Object.entries(fx)) check(name, repeatsItself(v.tail), name.startsWith("loop_soft"));
check("soft loop is the soft rule", softRepeats(fx.loop_soft_P1_greedy_16000.tail), true);
check("exact loop", repeatsItself("I should check the answer again. ".repeat(20)), true);
check("short text", repeatsItself("hello ".repeat(3)), false);
check("blank padding", repeatsItself("\n".repeat(900)), false);
console.log(bad ? "FEHLER" : "OK");
process.exit(bad ? 1 : 0);
