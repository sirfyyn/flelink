#!/usr/bin/env node
// Writes the sub-pages of flelink.run: one per model (site/models/) and one per GPU size (site/gpu/).
//
//   node site/seiten_bauen.mjs            write the pages
//   node site/seiten_bauen.mjs --pruefen  write nothing; exit 1 if a page on disk differs from what this script would write
//
// Every verdict comes from site/fit.js (the rule FLE Link itself uses) and every speed from its MEASURED list, so the
// sub-pages cannot say something else than the main page or the program. The texts below add only what the recipes and
// the catalog state (crates/flelink-core/recipes, catalog.rs). site/pruefen.py runs the --pruefen mode before a roll-out.
// No inline script or style: the server's Content-Security-Policy allows files from itself only.
import { createRequire } from "node:module";
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SITE = dirname(fileURLToPath(import.meta.url));
const fit = createRequire(import.meta.url)("./fit.js");
const BASE = "https://flelink.run/";
const CARDS = [12, 16, 24, 32];
const RAMS = [32, 64, 128];
const VCLASS = { fits: "v-fits", tight: "v-tight", "too big": "v-big", "no engine": "v-none" };

const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const model = (id) => fit.MODELS.find((m) => m.id === id);
const gib = (bytes) => (bytes / fit.GIB).toFixed(bytes < 10 * fit.GIB ? 1 : 0);
const verdict = (id, vendor, gpu, ram) => fit.verdict(model(id), fit.machine(vendor, gpu, ram)).fit;
const mark = (v) => `<span class="v ${VCLASS[v]}">${v}</span>`;

// What the pages say beyond the rule. Sources: the recipe notes, the catalog notes and the main page.
const PAGES = [
  {
    slug: "deepseek-v4.1-flash", ids: ["deepseek-v41-ista-dspark", "rin-deepseek-v41-dspark"],
    name: "DeepSeek V4.1 Flash",
    title: "Run DeepSeek V4.1 Flash locally on one 32 GB GPU",
    desc: "DeepSeek V4.1 Flash (763B) on one 32 GB GPU with 120 GB RAM: about 50 tokens/s with FLE Link and the FLE engine. One command, what fits which card, every number measured.",
    lead: "DeepSeek V4.1 Flash is a mixture-of-experts model with 763 billion parameters. As a 2-bit GGUF it is 232 GiB, seven times the memory of a 32 GB card. FLE Link runs it anyway: the FLE engine keeps the experts in RAM, puts the busiest ones on the GPU and lets a small draft model (DSpark) propose tokens that the big model checks.",
    needs: "One NVIDIA card with 32 GB, 120 GB of RAM and about 260 GB on a fast NVMe. FLE Link installs the FLE engine (v1.2.5 or newer) by itself.",
    more: [
      "The recipe <code>deepseek-v41-ista-dspark</code> uses ISTA DASLab's GSQ-RCO quant (IQ2_XXS) with the DSpark draft model. The draft does not change the answer: the model checks every drafted token.",
      "<b>Rin</b> (<code>rin-deepseek-v41-dspark</code>) is the same model split into a hot half that stays on the GPU and a cold half served from RAM. It is a little faster and a little worse than the quant it is made from; the model card has the numbers.",
      "The 2-bit quant itself is lossy: its next token agrees with the original weights 83.5 to 85.2 % of the time.",
    ],
    links: [
      ["https://huggingface.co/sirfyyn/Rin-Deepseek-V4.1-Flash-1017-FLE-GGUF", "Rin on Hugging Face"],
      ["https://huggingface.co/sirfyyn/DeepSeek-V4.1-Flash-1017-DSpark-FLE-GGUF", "DSpark draft model"],
      ["https://huggingface.co/ISTA-DASLab/DeepSeek-V4.1-Flash-1017-GSQ-RCO-GGUF", "ISTA DASLab's quant"],
      ["https://sirfyyn.de/en/fle/", "How FLE was measured"],
    ],
  },
  {
    slug: "qwen3.8-flash-next", ids: ["fle-iq3-m"],
    name: "Qwen3.8-Flash-Next",
    title: "Run Qwen3.8-Flash-Next locally on one GPU",
    desc: "Qwen3.8-Flash-Next (125B mixture of experts, 88.9 GB GGUF) with FLE Link: about 90 tokens/s on one 32 GB GPU, about 15 on a 16 GB card with 64 GB RAM. One command.",
    lead: "Qwen3.8-Flash-Next is a mixture-of-experts model with 125 billion parameters. The FLE quant (FLE-IQ3_M) is an 88.9 GB file, far more than a graphics card holds. The FLE engine keeps the experts in RAM and the busiest ones on the GPU, so the model runs from a 16 GB card and 64 GB of RAM.",
    needs: "One NVIDIA card from 16 GB and 64 GB of RAM; the measured top speed needs 32 GB and 120 GB.",
    more: [
      "On the same file and the same 32 GB card the FLE engine decoded at 91 tokens/s, Strata at 74 and llama.cpp at 36 (median over six tasks, 6 Oct 2026).",
      "The 91 tokens/s were measured with the engine's MTP draft head started by hand; <code>flelink run</code> starts the model without the head so far.",
    ],
    links: [
      ["https://huggingface.co/sirfyyn/Qwen3.8-Flash-Next-FLE-GGUF", "The FLE quant on Hugging Face"],
      ["https://github.com/sirfyyn/FastLocalExperts", "The FLE engine"],
      ["https://sirfyyn.de/en/fle/", "How FLE was measured"],
    ],
  },
  {
    slug: "katy-27b", ids: ["katy-27b-dflash"],
    name: "Katy 27B",
    title: "Run Katy 27B locally on a 12 to 32 GB GPU",
    desc: "Katy 27B (IQ4_XS with the DFlash2 draft) with FLE Link: 104 tokens/s from 20 GB of GPU memory, 19.4 on a 16 GB card, 11.2 on 12 GB. One command, every number measured.",
    lead: "Katy is a quantization of Swift-1.5-Qwen3.8-27B, a dense 27B model, as IQ4_XS with the DFlash2 draft model. From 20 GB of GPU memory all 66 layers are on the card; below that the rest runs on the CPU, which is slower.",
    needs: "One NVIDIA card from 12 GB and 24 GB of RAM. A 12 GB card is the minimum the recipe was measured with.",
    more: [
      "Below 20 GB the speed depends on the CPU and the RAM: the numbers were measured on a Ryzen 9 9950X3D with 120 GB RAM, and a slower machine will be slower.",
      "Licence of the model: Swift Open License v1.0, see the model page.",
    ],
    links: [
      ["https://huggingface.co/sirfyyn/Katy-27B-GGUF", "Katy on Hugging Face"],
      ["https://sirfyyn.de/en/katy/", "Katy: size, quality and speed"],
    ],
  },
  {
    slug: "llama-3.2-1b", ids: ["llama-3.2-1b"],
    name: "Llama 3.2 1B",
    title: "Run Llama 3.2 1B locally on any laptop",
    desc: "Llama 3.2 1B (Q4_K_M, 0.8 GiB) with FLE Link: 68 seconds from an empty laptop to the first answer, 91 tokens/s on a MacBook Air M2 with 8 GB. One command.",
    lead: "Llama 3.2 1B Instruct is the smallest model in the catalog: a 0.8 GiB file that runs on any laptop, with or without a GPU. It is the model of the first-run recording on the main page: 68 seconds from an empty home folder to the first answer.",
    needs: "Any machine: macOS, Linux or Windows, no GPU required.",
    more: ["Good for trying FLE Link out before a download of tens of gigabytes."],
    links: [["https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF", "The GGUF on Hugging Face"]],
  },
];

const head = (title, desc, path, ld) => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>${esc(title)} · FLE Link</title>
<meta name="description" content="${esc(desc)}">
<meta name="theme-color" content="#0b0e14">
<link rel="canonical" href="${BASE}${path}">
<link rel="icon" href="../favicon.svg?v=2" type="image/svg+xml"><link rel="icon" href="../favicon-32.png?v=2" type="image/png" sizes="32x32"><link rel="icon" href="../favicon-64.png?v=2" type="image/png" sizes="64x64">
<meta property="og:title" content="${esc(title)} · FLE Link">
<meta property="og:description" content="${esc(desc)}">
<meta property="og:url" content="${BASE}${path}">
<meta property="og:type" content="article">
<meta property="og:site_name" content="FLE Link">
<meta property="og:image" content="${BASE}og.png">
<meta property="og:image:width" content="1200"><meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:image" content="${BASE}og.png">
<script type="application/ld+json">${JSON.stringify(ld)}</script>
<link rel="stylesheet" href="../style.css?v=11">
</head>
<body>
<header class="top"><div class="wrap top-in">
  <a class="brand" href="../" aria-label="FLE Link home"><img src="../logo-mark.svg" alt="" width="34" height="34"><span><b>FLE</b> Link</span></a>
  <nav class="top-nav" aria-label="Main"><a href="../models/">Models</a><a href="../gpu/">Your card</a><a class="btn ghost" href="../#install">Install</a></nav>
</div></header>
`;

const foot = `<h2>Install FLE Link</h2>
<div class="cmd"><code>curl -fsSL https://flelink.run/install.sh | sh</code></div>
<p>Windows: <code>irm https://flelink.run/install.ps1 | iex</code>. Open source, MIT. No account, no telemetry. <a href="../">What FLE Link does →</a></p>
</main>
<footer class="foot"><div class="wrap foot-in">
  <div class="brand"><img src="../logo-mark.svg" alt="" width="26" height="26"><span><b>FLE</b> Link</span></div>
  <nav aria-label="Links"><a href="../">FLE Link</a><a href="../models/">Models</a><a href="../gpu/">Your card</a><a href="https://github.com/sirfyyn/flelink">Source</a><a href="https://github.com/sirfyyn/FastLocalExperts">FLE engine</a><a href="https://sirfyyn.de/en/">sirfyyn.de</a></nav>
  <nav aria-label="Legal"><a href="../impressum.html">Impressum</a><a href="../privacy.html">Privacy</a><a href="../terms.html">Terms</a><a href="../datenschutz.html">Datenschutz</a></nav>
  <p class="fine">MIT licence · no cookies, no third-party trackers · © 2026 Max Bitzer</p>
</div></footer>
</body>
</html>
`;

const crumbs = (items) => ({
  "@type": "BreadcrumbList",
  itemListElement: items.map(([name, path], i) => ({ "@type": "ListItem", position: i + 1, name, item: BASE + path })),
});
const article = (title, desc, path) => ({
  "@type": "TechArticle", headline: title, description: desc, url: BASE + path, inLanguage: "en",
  author: { "@type": "Person", name: "Max Bitzer", url: "https://sirfyyn.de/" },
  about: { "@type": "SoftwareApplication", name: "FLE Link", url: BASE },
});
const ld = (title, desc, path, trail) => ({ "@context": "https://schema.org", "@graph": [article(title, desc, path), crumbs(trail)] });

const RULE = `<p class="fine">The verdicts are the rule FLE Link applies before it downloads anything (<a href="https://github.com/sirfyyn/flelink/blob/main/docs/HOW-IT-WORKS.md">the rules in full</a>). A speed is shown only for the hardware it was measured on. A size nobody measured yet? <code>flelink bench --task pagoda --share</code> posts yours to the <a href="https://github.com/sirfyyn/flelink/discussions/categories/benchmarks">benchmarks board</a>.</p>`;

function modelPage(p) {
  const path = `models/${p.slug}.html`;
  const ms = p.ids.map(model);
  let h = head(p.title, p.desc, path, ld(p.title, p.desc, path, [["FLE Link", ""], ["Models", "models/"], [p.name, path]]));
  h += `<main class="wrap legal doc"><p class="crumb"><a href="../">FLE Link</a> / <a href="./">Models</a></p>\n<h1>${esc(p.title)}</h1>\n<p>${p.lead}</p>\n`;
  for (const m of ms) h += `<div class="cmd"><code>flelink run ${m.id}</code></div>\n`;
  h += `<h2>What you need</h2>\n<p>${p.needs}</p>\n`;
  h += `<h2>Measured speed</h2>\n<div class="fit-scroll"><table class="fit-table"><thead><tr><th scope="col">Recipe</th><th scope="col">Decode</th><th scope="col">Measured on</th></tr></thead><tbody>\n`;
  for (const m of ms)
    for (const s of fit.MEASURED.filter((x) => x.id === m.id))
      h += `<tr><th scope="row">${esc(m.title)}<small>${esc(m.what)}</small></th><td><b>${s.about ? "about " : ""}${s.tok}</b> tok/s</td><td>${esc(s.note)}</td></tr>\n`;
  h += `</tbody></table></div>\n`;
  for (const m of ms) {
    h += `<h2>Does ${esc(m.title)} fit my card?</h2>\n<div class="fit-scroll"><table class="fit-table"><thead><tr><th scope="col">Graphics</th>${RAMS.map((r) => `<th scope="col">${r} GB RAM</th>`).join("")}</tr></thead><tbody>\n`;
    for (const c of CARDS) h += `<tr><th scope="row"><a href="../gpu/${c}gb.html">${c} GB NVIDIA card</a></th>${RAMS.map((r) => `<td>${mark(verdict(m.id, "nvidia", c, r))}</td>`).join("")}</tr>\n`;
    h += `<tr><th scope="row">Apple Silicon Mac</th>${RAMS.map((r) => `<td>${mark(verdict(m.id, "apple", 0, r))}</td>`).join("")}</tr>\n`;
    h += `<tr><th scope="row">No GPU</th>${RAMS.map((r) => `<td>${mark(verdict(m.id, "none", 0, r))}</td>`).join("")}</tr>\n`;
    h += `</tbody></table></div>\n`;
    if (m.fle) h += `<p class="fine">"no engine": this recipe needs the FLE engine, which has an NVIDIA CUDA build only. File size ${gib(m.size + (m.draft || 0))} GiB including the draft model.</p>\n`;
  }
  h += RULE + `\n<h2>Good to know</h2>\n<ul>${p.more.map((x) => `<li>${x}</li>`).join("")}</ul>\n`;
  h += `<h2>Sources</h2>\n<ul>${p.links.map(([u, t]) => `<li><a href="${u}">${esc(t)}</a></li>`).join("")}</ul>\n`;
  return [path, h + foot];
}

function gpuPage(c) {
  const path = `gpu/${c}gb.html`;
  const title = `What runs on a ${c} GB GPU: local AI models that fit`;
  const best = [...fit.MODELS].reverse().find((m) => verdict(m.id, "nvidia", c, 128) === "fits");
  const desc = `Local AI models for an NVIDIA card with ${c} GB: which models FLE Link fits with 32, 64 or 128 GB of RAM, the measured tokens per second and the one command for each. Biggest that fits: ${best.title}.`;
  let h = head(title, desc, path, ld(title, desc, path, [["FLE Link", ""], ["Your card", "gpu/"], [`${c} GB`, path]]));
  h += `<main class="wrap legal doc"><p class="crumb"><a href="../">FLE Link</a> / <a href="./">Your card</a></p>\n<h1>${esc(title)}</h1>\n`;
  h += `<p>A ${c} GB NVIDIA card runs more than its memory suggests. For mixture-of-experts models the FLE engine keeps the experts in RAM and only the busiest ones on the GPU, so the RAM decides which model fits. With 128 GB of RAM the biggest model in the catalog that fits a ${c} GB card is <a href="../models/${PAGES.find((p) => p.ids.includes(best.id)).slug}.html">${esc(best.title)}</a>.</p>\n`;
  for (const r of RAMS) {
    h += `<h2>${c} GB card with ${r} GB of RAM</h2>\n<div class="fit-scroll"><table class="fit-table"><thead><tr><th scope="col">Model</th><th scope="col">Does it fit?</th><th scope="col">Measured on this hardware</th><th scope="col">One command</th></tr></thead><tbody>\n`;
    for (const m of fit.MODELS) {
      const mach = fit.machine("nvidia", c, r);
      const v = fit.verdict(m, mach).fit;
      const s = fit.speed(m.id, mach);
      const slug = PAGES.find((p) => p.ids.includes(m.id)).slug;
      h += `<tr${v === "too big" ? ' class="off"' : ""}><th scope="row"><a href="../models/${slug}.html">${esc(m.title)}</a><small>${esc(m.what)}</small></th><td>${mark(v)}</td>`;
      h += s ? `<td><b>${s.about ? "about " : ""}${s.tok}</b> tok/s<small>${esc(s.note)}</small></td>` : `<td class="none">not measured on this size</td>`;
      h += `<td><code>flelink run ${m.id}</code></td></tr>\n`;
    }
    h += `</tbody></table></div>\n`;
  }
  h += RULE + `\n<p>Other sizes: ${CARDS.filter((x) => x !== c).map((x) => `<a href="${x}gb.html">${x} GB</a>`).join(" · ")}. Any other GGUF file works with <code>flelink run /path/to/model.gguf</code>; the <a href="../#fit">calculator on the main page</a> takes any size, an Apple Silicon Mac or no GPU.</p>\n`;
  return [path, h + foot];
}

function indexPage(dir, title, desc, crumbName, body) {
  const path = `${dir}/`;
  let h = head(title, desc, path, ld(title, desc, path, [["FLE Link", ""], [crumbName, path]]));
  h += `<main class="wrap legal doc"><p class="crumb"><a href="../">FLE Link</a></p>\n<h1>${esc(title)}</h1>\n${body}\n`;
  return [`${dir}/index.html`, h + foot];
}

function allPages() {
  const out = PAGES.map(modelPage).concat(CARDS.map(gpuPage));
  out.push(indexPage("models", "Local AI models FLE Link runs in one command",
    "The models in FLE Link's catalog, from a 1B model for any laptop to DeepSeek V4.1 Flash with 763 billion parameters on one 32 GB GPU: size, measured speed and the command for each.",
    "Models",
    `<p>FLE Link reads your machine, installs the engine that fits and downloads the model. These are the models of the built-in catalog and recipes; any other GGUF file works with <code>flelink run /path/to/model.gguf</code>.</p>\n<ul class="doc-list">${PAGES.map((p) => `<li><a href="${p.slug}.html"><b>${esc(p.name)}</b></a> ${esc(p.ids.map((id) => model(id).what).join("; "))}</li>`).join("")}</ul>\n<p>Not sure what fits? <a href="../gpu/">Start from your card →</a></p>`));
  out.push(indexPage("gpu", "Which local AI model fits your GPU",
    "Pick the size of your NVIDIA card (12, 16, 24 or 32 GB) and see which local AI models FLE Link runs on it, with the measured tokens per second and the command for each.",
    "Your card",
    `<p>FLE Link checks the memory before it downloads anything. These pages show the same verdict for the common card sizes; the <a href="../#fit">calculator on the main page</a> takes any size, an Apple Silicon Mac or a machine without a GPU.</p>\n<ul class="doc-list">${CARDS.map((c) => `<li><a href="${c}gb.html"><b>${c} GB NVIDIA card</b></a> biggest that fits with 128 GB of RAM: ${esc([...fit.MODELS].reverse().find((m) => verdict(m.id, "nvidia", c, 128) === "fits").title)}</li>`).join("")}</ul>\n<p><a href="../models/">All models →</a></p>`));
  return out;
}

const check = process.argv.includes("--pruefen");
let diff = 0;
for (const [path, html] of allPages()) {
  const file = join(SITE, path);
  if (check) {
    if (!existsSync(file) || readFileSync(file, "utf8") !== html) {
      console.log(`${path}: differs from site/seiten_bauen.mjs (run: node site/seiten_bauen.mjs)`);
      diff++;
    }
  } else {
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, html);
    console.log(path);
  }
}
process.exit(diff ? 1 : 0);
