// "Does it fit?" on flelink.run: the rule FLE Link itself uses, ported line by line so the page and the program give the
// same verdict. Sources: crates/flelink-core/src/catalog.rs (fit_size, fit_fle) and crates/flelink-core/src/recipe.rs
// (Recipe::fit, the measured `[requires]`). tests/site-fit.test.mjs repeats the Rust tests against this file; when the rule
// changes in Rust, change it here and there. The disk is left out: the page cannot know it, the program checks it.
//
// Sizes are bytes, like in the catalog and the recipes. A machine is {vendor, gpu, ram}: `gpu` is the memory the GPU may
// use (on Apple Silicon two thirds of the unified memory, see system.rs), 0 without a GPU.
(function (root) {
  "use strict";
  const GIB = 2 ** 30;

  // Models the page offers: the built-in catalog and the built-in recipes (ids, sizes and requirements copied from them).
  // `fle`: needs the FLE engine, which has an NVIDIA CUDA build only (engines.rs: "fle-cuda").
  const MODELS = [
    { id: "llama-3.2-1b", title: "Llama 3.2 1B", what: "1B, a first try on any laptop", size: 807694464, fle: false },
    { id: "katy-27b-dflash", title: "Katy 27B + DFlash2", what: "dense 27B with a draft model", size: 15309039680, draft: 2056414112, fle: true,
      requires: { ram_gib: 24, vram_gib: 12, vram_is_minimum: true } },
    { id: "fle-iq3-m", title: "Qwen3.8-Flash-Next", what: "125B mixture of experts, 88.9 GB", size: 88872241728, fle: true, expertsInRam: true },
    { id: "deepseek-v41-ista-dspark", title: "DeepSeek V4.1 Flash + DSpark", what: "763B mixture of experts, 232 GiB", size: 248834257920, draft: 5216519072, fle: true,
      requires: { ram_gib: 120, vram_gib: 32 } },
    { id: "rin-deepseek-v41-dspark", title: "Rin (DeepSeek V4.1 Flash, split)", what: "763B, hot half on the GPU", size: 237857494656, draft: 5216519072, fle: true,
      requires: { ram_gib: 120, vram_gib: 32 } },
  ];

  // Speeds that were measured, and only on the machine they were measured on. `at(m)` says whether a machine is that one.
  // Sources are named so every number can be found: the recipe notes in crates/flelink-core/recipes, the catalog note,
  // the README, and the first-run recording on this page.
  const MEASURED = [
    { id: "llama-3.2-1b", tok: 91.0, at: (m) => m.vendor === "apple" && m.ram === 8 * GIB,
      note: "MacBook Air M2, 8 GB: the recording at the top of this page (3 Nov 2026)" },
    { id: "katy-27b-dflash", tok: 104, at: (m) => m.vendor === "nvidia" && m.gpu >= 20 * GIB,
      note: "from 20 GB up all 66 layers are on the card; one run, engine direct, context 32768, in a machine with 128 GB RAM" },
    { id: "katy-27b-dflash", tok: 19.4, at: (m) => m.vendor === "nvidia" && m.gpu === 16 * GIB,
      note: "16 GB card, 51 of 66 layers on it; one run, Ryzen 9 9950X3D with 128 GB RAM; a slower CPU will be slower" },
    { id: "katy-27b-dflash", tok: 11.2, at: (m) => m.vendor === "nvidia" && m.gpu === 12 * GIB,
      note: "12 GB, 33 of 66 layers; one run, memory limited on a larger card to simulate the size" },
    { id: "fle-iq3-m", tok: 91, at: (m) => m.vendor === "nvidia" && m.gpu >= 32 * GIB && m.ram >= 120 * GIB,
      note: "32 GB card, 128 GB RAM, with the engine's MTP draft head started by hand; flelink run starts it without the head so far" },
    { id: "fle-iq3-m", tok: 15, about: true, at: (m) => m.vendor === "nvidia" && m.gpu === 16 * GIB && m.ram === 64 * GIB,
      note: "16 GB card, 64 GB RAM (8 Nov 2026)" },
    { id: "deepseek-v41-ista-dspark", tok: 49.9, at: (m) => m.vendor === "nvidia" && m.gpu >= 32 * GIB && m.ram >= 120 * GIB,
      note: "32 GB card, 128 GB RAM (120 GB for the model, 8 GB left to the system), through FLE Link, median of three (5 Nov 2026); 37.2 without the draft" },
    { id: "rin-deepseek-v41-dspark", tok: 53.4, at: (m) => m.vendor === "nvidia" && m.gpu >= 32 * GIB && m.ram >= 120 * GIB,
      note: "32 GB card, 128 GB RAM, FLE v1.2.6; lossy, see the model card" },
  ];

  // catalog.rs fit_size: weights plus KV cache and compute buffers against what the GPU may use, or half the RAM.
  function fitSize(size, expertsInRam, m) {
    if (expertsInRam) return fitFle(size, m);
    const need = Math.floor(size * 1.25);
    const have = Math.max(m.gpu, Math.floor(m.ram / 2));
    if (need <= Math.floor((have * 8) / 10)) return "fits";
    if (need <= have) return "tight";
    return "too big";
  }

  // catalog.rs fit_fle: FLE pins about 0.55 of the file in RAM; the card holds the dense part and the KV cache first.
  function fitFle(size, m) {
    const pinned = Math.floor(size / 100) * 55;
    if (m.gpu > 0 && m.gpu < 10 * GIB) return "too big";
    if (m.ram >= pinned + 16 * GIB && m.gpu >= 16 * GIB) return "fits";
    if (m.ram >= pinned + 6 * GIB) return "tight";
    return "too big";
  }

  // recipe.rs: what the recipe was measured with comes before the rule; up to a fifth less is tight.
  function measured(needGib, have) {
    if (needGib == null) return null;
    const need = needGib * GIB;
    if (have >= Math.floor((need * 98) / 100)) return "fits";
    if (have >= Math.floor((need * 8) / 10)) return "tight";
    return "too big";
  }

  function fitRecipe(model, m) {
    const req = model.requires || {};
    const ram = measured(req.ram_gib, m.ram);
    let vram = measured(req.vram_gib, m.gpu);
    if (vram === "tight" && req.vram_is_minimum) vram = "too big";
    if (ram === null && vram === null) return fitSize(model.size + (model.draft || 0), !!model.expertsInRam, m);
    const both = [ram, vram].filter((x) => x !== null);
    if (both.includes("too big")) return "too big";
    if (both.includes("tight")) return "tight";
    return "fits";
  }

  // The verdict for the page: the engine first (an FLE recipe needs an NVIDIA card), then the memory rule.
  function verdict(model, m) {
    if (model.fle && m.vendor !== "nvidia") return { fit: "no engine", why: "needs the FLE engine, which has an NVIDIA CUDA build only" };
    return { fit: fitRecipe(model, m), why: "" };
  }

  function speed(id, m) {
    return MEASURED.find((x) => x.id === id && x.at(m)) || null;
  }

  // A machine from the page's two choices. Apple Silicon: the GPU may use two thirds of the unified memory.
  function machine(vendor, gpuGib, ramGib) {
    const ram = ramGib * GIB;
    if (vendor === "apple") return { vendor, ram, gpu: Math.floor((ram * 2) / 3) };
    if (vendor === "none") return { vendor, ram, gpu: 0 };
    return { vendor, ram, gpu: gpuGib * GIB };
  }

  const api = { GIB, MODELS, MEASURED, fitSize, fitFle, fitRecipe, verdict, speed, machine };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.FLEFit = api;
})(typeof window !== "undefined" ? window : globalThis);
