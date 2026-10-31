#!/usr/bin/env python3
"""Set up <work>/sprime: a data folder in which the Studio itself installed the best engine (with engines.json).

    python3 tests/fulltest/studio_prime.py --bin target/release/flelink --work /tmp/ft

Models are shared with <work>/prime (symlink). The Studio scenarios of fulltest.py use this engine folder.
"""
import argparse, json, os, re, socket, subprocess, sys, time, urllib.request
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--bin", required=True)
ap.add_argument("--work", required=True)
a = ap.parse_args()
work = Path(a.work)
sp = work / "sprime"
sp.mkdir(parents=True, exist_ok=True)
if not (sp / "models").exists() and (work / "prime" / "models").exists():
    os.symlink(work / "prime" / "models", sp / "models")
s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
env = dict(os.environ, FLE_HOME=str(sp), FLE_CONFIG=str(sp / "config.toml"), FLE_STUDIO_HOME=str(sp))
log = open(sp / "studio.log", "w")
p = subprocess.Popen([a.bin, "studio", "--no-browser", "--port", str(port)], env=env, stdout=log, stderr=subprocess.STDOUT)
base = f"http://127.0.0.1:{port}"


def call(method, path, body=None, token=""):
    req = urllib.request.Request(base + path, data=json.dumps(body).encode() if body is not None else None, method=method,
                                 headers={"X-FLE-Token": token, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read() or b"null")


try:
    for _ in range(100):
        try:
            urllib.request.urlopen(base + "/api/ping", timeout=1)
            break
        except Exception:
            time.sleep(0.2)
    token = re.search(r'name="fle-token" content="([^"]+)"', urllib.request.urlopen(base + "/").read().decode()).group(1)
    eng = call("GET", "/api/engines", token=token)
    variants = eng.get("variants") or eng.get("available") or []
    best = next((v for v in variants if v.get("recommended") or v.get("best")), variants[0] if variants else None)
    if not best:
        sys.exit(f"no engine variant offered: {json.dumps(eng)[:400]}")
    print("installing", best.get("id"))
    call("POST", "/api/engines/install", {"variant": best["id"]}, token=token)
    for _ in range(600):
        if (sp / "engines" / "engines.json").exists():
            inst = call("GET", "/api/engines", token=token).get("installed") or []
            if inst:
                print("installed:", [i.get("id") for i in inst])
                break
        time.sleep(1)
    else:
        sys.exit("engine install did not finish in 10 minutes")
finally:
    p.terminate()
    p.wait(15)
