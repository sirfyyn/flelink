// The fit rule on flelink.run (site/fit.js) must give the same verdicts as the program. These are the Rust tests of
// crates/flelink-core/src/catalog.rs and recipe.rs, repeated against the page's copy of the rule.
//   node --test tests/site-fit.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const fit = createRequire(import.meta.url)("../site/fit.js");
const { GIB, MODELS, fitSize, fitRecipe, verdict, speed, machine } = fit;
const model = (id) => MODELS.find((m) => m.id === id);
const nvidia = (gpuGib, ramGib) => machine("nvidia", gpuGib, ramGib);

test("small model fits a laptop and the big one does not (catalog.rs)", () => {
  const mac = machine("apple", 0, 8);
  assert.equal(mac.gpu, Math.floor((8 * GIB * 2) / 3));
  assert.equal(fitRecipe(model("llama-3.2-1b"), mac), "fits");
  assert.equal(fitRecipe(model("fle-iq3-m"), mac), "too big");
});

test("an FLE model needs the RAM, not the VRAM (catalog.rs)", () => {
  const q = model("fle-iq3-m");
  assert.equal(fitRecipe(q, nvidia(32, 32)), "too big");
  assert.equal(fitRecipe(q, nvidia(32, 48)), "too big");
  assert.equal(fitRecipe(q, nvidia(32, 56)), "tight");
  assert.equal(fitRecipe(q, nvidia(32, 64)), "fits");
  assert.equal(fitRecipe(q, nvidia(32, 128)), "fits");
});

test("an FLE model on a small GPU (catalog.rs)", () => {
  const q = model("fle-iq3-m");
  assert.equal(fitRecipe(q, nvidia(16, 64)), "fits");
  assert.equal(fitRecipe(q, nvidia(12, 64)), "tight");
  assert.equal(fitRecipe(q, nvidia(8, 64)), "too big");
});

test("measured requirements decide, up to a fifth less is tight (recipe.rs)", () => {
  const d = model("deepseek-v41-ista-dspark");
  // the machine it was measured on: 120.9 GiB RAM, 32 GiB VRAM
  assert.equal(fitRecipe(d, { vendor: "nvidia", ram: 129_800_000_000, gpu: 32 * GIB }), "fits");
  assert.equal(fitRecipe(d, nvidia(32, 100)), "tight");
  assert.equal(fitRecipe(d, machine("apple", 0, 8)), "too big");
  assert.equal(fitRecipe(d, nvidia(32, 64)), "too big");
});

test("a minimum VRAM is a minimum: tight becomes too big (recipe.rs, vram_is_minimum)", () => {
  const k = model("katy-27b-dflash");
  assert.equal(fitRecipe(k, nvidia(12, 32)), "fits");
  assert.equal(fitRecipe(k, nvidia(10, 32)), "too big"); // 10 of 12 GB would be tight without the minimum
  assert.equal(fitRecipe(k, nvidia(12, 20)), "tight"); // RAM: 20 of 24 GB
  assert.equal(fitRecipe(k, nvidia(12, 16)), "too big");
});

test("a plain GGUF: 1.25 times its size against the GPU or half the RAM (catalog.rs fit_size)", () => {
  const eightGb = 8 * GIB; // needs 10 GiB
  assert.equal(fitSize(eightGb, false, nvidia(16, 16)), "fits"); // 10 <= 12.8
  assert.equal(fitSize(eightGb, false, nvidia(12, 16)), "tight"); // 10 > 9.6, <= 12
  assert.equal(fitSize(eightGb, false, nvidia(8, 16)), "too big");
  assert.equal(fitSize(eightGb, false, machine("none", 0, 32)), "fits"); // half of 32 GiB RAM
});

test("FLE recipes need an NVIDIA card; measured speeds only on the measured machine", () => {
  assert.equal(verdict(model("katy-27b-dflash"), machine("apple", 0, 64)).fit, "no engine");
  assert.equal(verdict(model("llama-3.2-1b"), machine("apple", 0, 8)).fit, "fits");
  assert.equal(speed("llama-3.2-1b", machine("apple", 0, 8)).tok, 91.0);
  assert.equal(speed("llama-3.2-1b", nvidia(32, 128)), null);
  assert.equal(speed("katy-27b-dflash", nvidia(12, 32)).tok, 11.2);
  assert.equal(speed("katy-27b-dflash", nvidia(16, 32)).tok, 19.4);
  assert.equal(speed("katy-27b-dflash", nvidia(24, 32)).tok, 104);
  assert.equal(speed("deepseek-v41-ista-dspark", nvidia(32, 128)).tok, 49.9);
  assert.equal(speed("deepseek-v41-ista-dspark", nvidia(32, 64)), null);
  assert.equal(speed("fle-iq3-m", nvidia(16, 64)).tok, 15);
});
