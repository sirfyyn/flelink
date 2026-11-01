"""The Python Forge as the reference for tests/forge_conformance.rs.

Usage: forge_ref.py <studio src> <work dir> <out dir>. Writes the synthetic models the Rust test uses into <work dir>/in
(tests/forge_synth.py of the Studio checkout), runs the same calls and jobs through the Python Forge with the fake
llama.cpp tools, and prints the results as one JSON object.
"""

import json
import sys
import time
from pathlib import Path

src, work, out = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
sys.path[:0] = [str(src), str(src.parent / "tests")]

from forge_synth import make_model  # noqa: E402

from fle_studio_forge.api import Forge  # noqa: E402

fakes = src.parent / "tests" / "fake_tools"
inp = work / "in"
inp.mkdir(parents=True, exist_ok=True)
(inp / "corpus.txt").write_text("some calibration text\n" * 50, encoding="utf-8")
(inp / "eval.txt").write_text("some evaluation text\n" * 50, encoding="utf-8")
make_model(inp / "full.gguf", seed=1, layers=4, types=[("Q8_0",) * 3] * 4)
make_model(inp / "low.gguf", seed=2, layers=4, types=[("Q4_0",) * 3] * 4)
make_model(inp / "mid.gguf", seed=3, layers=4, types=[("Q4_0", "Q4_0", "Q8_0")] * 4)

settings = {"forge.tools_dir": str(fakes)}
forge = Forge(work / "data", settings.get, lambda: out, lambda: [], None, None)


def wait(ident):
    end = time.monotonic() + 120
    while time.monotonic() < end:
        j = forge.jobs.get(ident)
        if j["state"] in ("done", "failed", "cancelled") and not j["running"]:
            return j
        time.sleep(0.05)
    raise SystemExit(f"job {ident} did not finish")


def recipes():
    f = lambda n: str(inp / n)  # noqa: E731
    return {
        "hybrid": {
            "name": "htest",
            "mode": "hybrid",
            "sources": {"B": {"path": f("full.gguf"), "role": "base"}, "L": {"path": f("low.gguf"), "role": "donor"}},
            "hybrid": {"base": "B", "layers": {"1": "L", "3": "L"}},
            "imatrix": {"corpus": f("corpus.txt"), "context": 128},
            "reap": {"enabled": True, "keep": 6, "chunks": 2},
            "verify": {"enabled": True, "reference": "B", "text": f("eval.txt"), "chunks": 3, "context": 128},
        },
        "quantize": {
            "name": "qtest",
            "mode": "quantize",
            "sources": {"S": {"path": f("full.gguf"), "role": "base"}},
            "quantize": {"source": "S", "type": "q4_0", "rules": [{"pattern": "ffn_down_exps", "type": "q8_0"}]},
            "imatrix": {"corpus": f("corpus.txt"), "chunks": 3, "context": 128},
            "verify": {"enabled": True, "reference": "S", "text": f("eval.txt"), "chunks": 3, "context": 128},
        },
    }


result = {
    "inspect": forge.inspect(str(inp / "low.gguf")),
    "calculate": forge.calculate(
        {
            "mode": "hybrid",
            "sources": {"A": str(inp / "full.gguf"), "B": str(inp / "low.gguf"), "C": str(inp / "mid.gguf")},
            "base": "A",
            "budget_gb": 0.00016,
            "ram_gb": 16,
            "vram_gb": 8,
            "context": 8192,
            "reap_keep": 6,
        }
    ),
    "quant_estimate": forge.calculate({"mode": "quantize", "source_path": str(inp / "full.gguf"), "quantize": {"type": "q4_0"}}),
    "corpus": forge.make_corpus({"bundled": True}),
    "corpus_eval": forge.make_corpus({"bundled": True, "part": "evaluation"}),
    "jobs": {},
}
for c in ("corpus", "corpus_eval"):
    result[c]["text"] = Path(result[c]["path"]).read_text(encoding="utf-8")
for key, r in recipes().items():
    result["check_" + key] = forge.jobs.check(r)
    j = wait(forge.jobs.create(r)["id"])
    res = j["result"]
    result["jobs"][key] = {
        "state": j["state"],
        "error": j["error"],
        "steps": [(s["id"], s["state"]) for s in j["steps"]],
        "sha256": res.get("sha256"),
        "size": res.get("size"),
        "card": res.get("card"),
        "recipe_file": Path(res["recipe_file"]).read_text(encoding="utf-8") if res.get("recipe_file") else None,
        "portable": forge.recipe_of(j["id"]),
    }
print(json.dumps(result))
