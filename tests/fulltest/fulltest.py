#!/usr/bin/env python3
"""Fulltest for FLE Link: every command once as intended and once under a fault.

Runs against a built binary with its own data folder, never the user's. Standard library only.

    python3 tests/fulltest/fulltest.py --bin target/release/flelink --work /tmp/ft [--only id,id] [--model llama-3.2-1b]

`--work` must hold `prime/`, a data folder in which `flelink ask MODEL` has run once (engine and model in place); the
scenario `first-run` creates it when it is missing. Results go to `<work>/results.json` and `<work>/results.md`.
"""
from __future__ import annotations

import argparse
import http.client as httpc
import json
import os
import platform
import re
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import threading
import time
import traceback
import urllib.error
import urllib.request
from pathlib import Path

ARGS: argparse.Namespace
RESULTS: list[dict] = []
SCENARIOS: list[tuple[str, str, object]] = []


def scenario(sid: str, area: str):
    def deco(fn):
        SCENARIOS.append((sid, area, fn))
        return fn
    return deco


class Skip(Exception):
    pass


# ---------------------------------------------------------------------------------------------------------- helpers

def env_for(home: Path, extra: dict | None = None) -> dict:
    e = dict(os.environ)
    e.update({
        "FLE_HOME": str(home),
        "FLE_CONFIG": str(home / "config.toml"),
        "FLE_STUDIO_HOME": str(home),  # one folder for both; the split itself is tested in cli-studio-share
    })
    for k in ("FLE_ENGINE", "FLE_UPSTREAM_ENGINE", "HF_TOKEN"):
        e.pop(k, None)
    if extra:
        for k, v in extra.items():
            if v is None:
                e.pop(k, None)
            else:
                e[k] = v
    return e


def cli(home: Path, *args: str, timeout: float = 120, extra: dict | None = None, stdin: str | None = None) -> dict:
    t0 = time.time()
    try:
        p = subprocess.run([ARGS.bin, *args], env=env_for(home, extra), capture_output=True, text=True,
                           timeout=timeout, input=stdin)
        rc, out, err = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired as ex:
        rc, out, err = "timeout", (ex.stdout or b"").decode(errors="replace") if isinstance(ex.stdout, bytes) else (ex.stdout or ""), \
            (ex.stderr or b"").decode(errors="replace") if isinstance(ex.stderr, bytes) else (ex.stderr or "")
    return {"rc": rc, "out": out, "err": err, "s": round(time.time() - t0, 2)}


def fresh(name: str) -> Path:
    d = Path(ARGS.work) / "homes" / name
    if d.exists():
        shutil.rmtree(d)
    d.mkdir(parents=True)
    return d


def prime() -> Path:
    return Path(ARGS.work) / "prime"


def primed_home(name: str) -> Path:
    """A new data folder that shares engines and models with prime/ (symlinked, nothing is copied)."""
    src = prime()
    if not (src / "models").exists():
        raise Skip("prime/ has no models; run the scenario first-run")
    d = fresh(name)
    sp = Path(ARGS.work) / "sprime"  # engines installed through the Studio carry engines.json, which the CLI does not write
    eng = sp / "engines" if (sp / "engines" / "engines.json").exists() else src / "engines"
    if eng.exists():
        os.symlink(eng, d / "engines")
    if (src / "models").exists():
        os.symlink(src / "models", d / "models")
    if (src / "config.toml").exists():
        shutil.copy(src / "config.toml", d / "config.toml")
    return d


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def http(method: str, url: str, body=None, headers: dict | None = None, timeout: float = 30, ctx=None) -> dict:
    data = None
    h = dict(headers or {})
    if body is not None:
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        h.setdefault("Content-Type", "application/json")
    req = urllib.request.Request(url, data=data, method=method, headers=h)
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=ctx) as r:
            raw = r.read()
            code, hdrs = r.status, dict(r.headers)
    except urllib.error.HTTPError as e:
        raw, code, hdrs = e.read(), e.code, dict(e.headers)
    except Exception as e:  # noqa: BLE001
        return {"code": None, "error": f"{type(e).__name__}: {e}", "s": round(time.time() - t0, 2)}
    try:
        js = json.loads(raw)
    except Exception:  # noqa: BLE001
        js = None
    return {"code": code, "json": js, "text": raw[:2000].decode(errors="replace"), "headers": hdrs,
            "s": round(time.time() - t0, 2)}


class Studio:
    def __init__(self, home: Path, *extra_args: str, port: int | None = None, extra_env: dict | None = None):
        self.home, self.port = home, port or free_port()
        self.log = open(home / "studio.log", "w")
        self.p = subprocess.Popen([ARGS.bin, "studio", "--no-browser", "--port", str(self.port), *extra_args],
                                  env=env_for(home, extra_env), stdout=self.log, stderr=subprocess.STDOUT,
                                  start_new_session=True)
        self.scheme = "https" if ("--host" in extra_args and "--no-tls" not in extra_args) else "http"
        self.base = f"{self.scheme}://127.0.0.1:{self.port}"

    def wait(self, t: float = 30) -> bool:
        ctx = ssl._create_unverified_context() if self.scheme == "https" else None
        end = time.time() + t
        while time.time() < end:
            if self.p.poll() is not None:
                return False
            r = http("GET", self.base + "/api/ping", timeout=2, ctx=ctx)
            if r.get("code") in (200, 401):
                return True
            time.sleep(0.3)
        return False

    token = ""

    def get_token(self) -> str:
        if not self.token:
            ctx = ssl._create_unverified_context() if self.scheme == "https" else None
            page = http("GET", self.base + "/", ctx=ctx, headers=getattr(self, "auth", {})).get("text") or ""
            m = re.search(r'name="fle-token" content="([^"]+)"', page)
            self.token = m.group(1) if m else ""
        return self.token

    def api(self, method: str, path: str, body=None, headers: dict | None = None, **kw) -> dict:
        h = dict(headers or {})
        if "Authorization" not in h and not getattr(self, "raw", False):
            h.setdefault("X-FLE-Token", self.get_token())
        return http(method, self.base + path, body, headers=h, **kw)

    def text(self) -> str:
        self.log.flush()
        return (self.home / "studio.log").read_text(errors="replace")

    def stop(self, sig=signal.SIGTERM, t: float = 15) -> int | None:
        if self.p.poll() is None:
            try:
                os.killpg(self.p.pid, sig) if sig != signal.SIGINT else self.p.send_signal(sig)
            except ProcessLookupError:
                pass
            try:
                self.p.wait(t)
            except subprocess.TimeoutExpired:
                os.killpg(self.p.pid, signal.SIGKILL)
                self.p.wait(5)
        self.log.close()
        return self.p.returncode


def engine_pids(home: Path) -> list[int]:
    """llama-server processes whose command line points into this data folder (or into prime/)."""
    out = subprocess.run(["ps", "-axww", "-o", "pid=,command="], capture_output=True, text=True).stdout
    sp = Path(ARGS.work) / "sprime"
    keys = {str(home), str(home.resolve()), str(prime()), str(prime().resolve()), str(sp), str(sp.resolve())}
    pids = []
    for line in out.splitlines():
        line = line.strip()
        pid, _, cmd = line.partition(" ")
        if "llama-server" in cmd and any(k in cmd for k in keys):
            pids.append(int(pid))
    return pids


def wait_engine(st: Studio, want: str = "ready", t: float = 180) -> dict:
    end, last = time.time() + t, {}
    while time.time() < end:
        r = st.api("GET", "/api/engine")
        last = r.get("json") or {}
        state = str(last.get("state") or last.get("status") or "")
        if want in state or (want == "ready" and last.get("ready")):
            return last
        if state in ("failed", "crashed", "error") and want == "ready":
            return last
        time.sleep(1)
    return last


def start_engine(st: Studio, model: str | None = None) -> dict:
    models = (st.api("GET", "/api/models").get("json") or {})
    lst = models.get("models") if isinstance(models, dict) else models
    mid = None
    for m in lst or []:
        p = m.get("path") or m.get("file") or ""
        if (model or ARGS.model_file) in p or (model and model == m.get("id")):
            mid = m.get("id") or p
            break
    if mid is None and lst:
        mid = lst[0].get("id") or lst[0].get("path")
    body = {"model": mid} if mid else {}
    return st.api("POST", "/api/engine/start", body)


def result(sid, area, status, note, data=None):
    RESULTS.append({"id": sid, "area": area, "status": status, "note": note, "data": data or {}})
    mark = {"pass": "PASS", "fail": "FAIL", "warn": "WARN", "skip": "SKIP"}[status]
    print(f"[{mark}] {sid:28s} {note}", flush=True)


def check(cond: bool, ok: str, bad: str, data=None, warn: bool = False):
    return ("pass", ok, data) if cond else (("warn" if warn else "fail"), bad, data)


# ---------------------------------------------------------------------------------------------------------- scenarios
# Each returns (status, note, data). Raising Skip marks it skipped; any other exception is a fail.

@scenario("first-run", "install")
def s_first_run():
    p = prime()
    if (p / "models").exists() and any((p / "models").rglob("*.gguf")):
        raise Skip("prime/ already set up")
    p.mkdir(parents=True, exist_ok=True)
    r = cli(p, "ask", ARGS.model, "Name the capital of France in one word.", timeout=1800)
    ok = r["rc"] == 0 and "paris" in r["out"].lower()
    return check(ok, f"zero to answer in {r['s']} s", f"rc={r['rc']}: {r['err'][-400:]}", {"s": r["s"]})


@scenario("help-all", "cli")
def s_help():
    home = fresh("help")
    top = cli(home, "--help")
    cmds = [l.split()[0] for l in top["out"].split("Commands:")[1].split("Options:")[0].splitlines() if l.strip()]
    bad, legacy = [], []
    for c in cmds:
        if c == "help":
            continue
        r = cli(home, c, "--help", timeout=20)
        if r["rc"] != 0:
            bad.append(c)
        for word in ("FLE Studio", "`fle`", "FLE knows", "fle could"):
            if word in r["out"]:
                legacy.append(f"{c}: {word}")
    for word in ("FLE Studio", "`fle`", "FLE knows", "fle could"):
        if word in top["out"]:
            legacy.append(f"top: {word}")
    if bad:
        return "fail", f"--help fails for {bad}", {}
    return check(not legacy, f"{len(cmds)} commands have help", f"old names in help: {sorted(set(legacy))}",
                 {"legacy": legacy}, warn=True)


@scenario("version", "cli")
def s_version():
    r1 = cli(fresh("ver"), "--version")
    exe = Path(ARGS.bin)
    short = exe.with_name("fle" + exe.suffix)
    r2 = subprocess.run([str(short), "--version"], capture_output=True, text=True) if short.exists() else None
    v1 = r1["out"].strip()
    note = f"flelink: {v1!r}" + (f", fle: {r2.stdout.strip()!r}" if r2 else ", no fle binary next to it")
    return check(r1["rc"] == 0 and v1.startswith("flelink"), note, note, warn=True)


@scenario("unknown-command", "cli")
def s_unknown():
    r = cli(fresh("unk"), "frobnicate")
    return check(r["rc"] not in (0, "timeout") and "frobnicate" in (r["err"] + r["out"]),
                 "clear error for unknown command", f"rc={r['rc']}")


@scenario("system", "cli")
def s_system():
    r = cli(fresh("sys"), "system")
    need = ("OS", "CPU", "Memory", "Disk free")
    miss = [n for n in need if n not in r["out"]]
    return check(r["rc"] == 0 and not miss, "system lists OS, CPU, memory, disk, engines", f"missing {miss}",
                 {"out": r["out"][:800]})


@scenario("models-fit", "cli")
def s_models():
    r = cli(fresh("mod"), "models")
    return check(r["rc"] == 0 and ARGS.model in r["out"] and ("fits" in r["out"]),
                 "catalog shows fit per model", r["err"][-300:], {"out": r["out"][:600]})


@scenario("engines-empty", "cli")
def s_engines_empty():
    r = cli(fresh("eng0"), "engines")
    return check(r["rc"] in (0, 1) and r["rc"] != "timeout" and (r["out"] + r["err"]).strip() != "",
                 f"empty home answers (rc={r['rc']}): {(r['out'] + r['err']).strip()[:120]}", "no output")


@scenario("engines-primed", "cli")
def s_engines():
    h = primed_home("eng1")
    r = cli(h, "engines")
    return check(r["rc"] == 0 and "llama-server" in (r["out"] + r["err"]) or "b" in r["out"],
                 f"finds the installed engine: {r['out'].strip()[:160]}", r["err"][-300:])


@scenario("config-show-set", "cli")
def s_config():
    h = fresh("cfg")
    show = cli(h, "config", "show")
    bad = cli(h, "config", "set", "no.such.key", "1")
    badv = cli(h, "config", "set", "server.port", "not-a-number")
    data = {"show": show["out"][:400], "bad_key": (bad["rc"], (bad["out"] + bad["err"])[:200]),
            "bad_value": (badv["rc"], (badv["out"] + badv["err"])[:200])}
    if show["rc"] != 0:
        return "fail", f"config show rc={show['rc']}", data
    problems = []
    if bad["rc"] == 0:
        problems.append("unknown key accepted")
    if badv["rc"] == 0:
        problems.append("non-numeric port accepted")
    return check(not problems, "rejects unknown keys and bad values", "; ".join(problems), data, warn=True)


@scenario("doctor-missing-model", "cli")
def s_doctor_missing():
    r = cli(fresh("doc0"), "doctor", "/nonexistent/model.gguf")
    msg = (r["out"] + r["err"]).strip()
    return check(r["rc"] != 0 and r["rc"] != "timeout" and ("not" in msg.lower() or "no such" in msg.lower()),
                 f"clear message: {msg[-140:]}", f"rc={r['rc']} {msg[-200:]}")


@scenario("doctor-ok", "cli")
def s_doctor_ok():
    h = primed_home("doc1")
    r = cli(h, "doctor", ARGS.model, timeout=60)
    by_path = cli(h, "doctor", str(model_file()), timeout=60)
    extra = {"by_id": (r["rc"], r["out"][-600:]), "by_path": (by_path["rc"], by_path["out"][-600:])}
    if r["rc"] != 0 and by_path["rc"] == 0:
        return "fail", "doctor knows only file paths, not catalog names that ask/run accept", extra
    return check(r["rc"] == 0, "doctor passes for the primed model", f"rc={r['rc']} {(r['out'] + r['err'])[-300:]}",
                 {"out": r["out"][-800:]})


@scenario("plan", "cli")
def s_plan():
    h = primed_home("plan")
    r = cli(h, "plan", ARGS.model, timeout=60)
    return check(r["rc"] == 0 and "llama-server" in r["out"], "plan prints the engine command",
                 f"rc={r['rc']} {(r['out'] + r['err'])[-300:]}", {"out": r["out"][-800:]})


def model_file() -> Path:
    files = sorted(prime().joinpath("models").rglob("*.gguf"), key=lambda p: p.stat().st_size)
    if not files:
        raise Skip("no model in prime/")
    return files[0]


@scenario("report-redaction", "cli")
def s_report():
    h = primed_home("report")
    secret = "hf_FULLTESTSECRET0123456789abcdef"
    out = h / "report.json"
    r = cli(h, "report", ARGS.model, "--out", str(out), timeout=60, extra={"HF_TOKEN": secret})
    if not out.exists():  # rc 1 only says the doctor checks inside failed
        return "fail", f"no report written, rc={r['rc']}: {(r['out'] + r['err'])[-200:]}", {}
    txt = out.read_text(errors="replace")
    user = Path.home().name
    leaks = [w for w, hit in (("HF_TOKEN", secret in txt), ("user name", f"/Users/{user}/" in txt or f"/home/{user}/" in txt))
             if hit]
    if '"model": null' in txt:
        leaks.append("model given by catalog name not resolved")
    return check(not leaks, f"report written ({len(txt)} bytes), no token, no user name",
                 f"report leaks: {leaks}", {"size": len(txt)}, warn=leaks == ["user name"])


@scenario("bench-against-run", "inference")
def s_bench():
    h = primed_home("bench")
    p, txt, url = run_until_ready(h)
    try:
        if not url:
            return "fail", "run not ready", {"log": txt[-400:]}
        r = cli(h, "bench", "--url", url, timeout=300)
        if r["rc"] != 0:
            r = cli(h, "bench", ARGS.model, "--url", url, timeout=300)
        tail = (r["out"] + r["err"]).strip()[-300:]
        return check(r["rc"] == 0 and "tok" in tail, f"bench: {tail.splitlines()[-1][:160] if tail else ''}",
                     f"rc={r['rc']} {tail}", {"out": tail})
    finally:
        os.killpg(p.pid, signal.SIGINT)
        try:
            p.wait(20)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)


@scenario("inspect-verify-ok", "gguf")
def s_inspect_ok():
    f = model_file()
    h = fresh("insp")
    a = cli(h, "inspect", str(f))
    b = cli(h, "verify", str(f), timeout=300)
    return check(a["rc"] == 0 and b["rc"] == 0, f"inspect {a['s']} s, verify {b['s']} s",
                 f"inspect rc={a['rc']}, verify rc={b['rc']}: {(b['out'] + b['err'])[-300:]}")


@scenario("gguf-truncated", "gguf")
def s_truncated():
    f = model_file()
    h = fresh("trunc")
    t = h / "truncated.gguf"
    with open(f, "rb") as src, open(t, "wb") as dst:
        dst.write(src.read(64 * 1024 * 1024))
    a = cli(h, "inspect", str(t))
    b = cli(h, "verify", str(t))
    msgs = {"inspect": (a["rc"], (a["out"] + a["err"])[-200:]), "verify": (b["rc"], (b["out"] + b["err"])[-200:])}
    return check(b["rc"] not in (0, "timeout"), "verify refuses a cut-off file", f"verify accepted it: {msgs}", msgs)


@scenario("gguf-garbage", "gguf")
def s_garbage():
    h = fresh("garb")
    g = h / "garbage.gguf"
    g.write_bytes(os.urandom(4096))
    a = cli(h, "inspect", str(g))
    b = cli(h, "verify", str(g))
    c = cli(h, "doctor", str(g))
    out = {k: (v["rc"], (v["out"] + v["err"]).strip()[-160:]) for k, v in (("inspect", a), ("verify", b), ("doctor", c))}
    panics = [k for k, v in (("inspect", a), ("verify", b), ("doctor", c)) if "panicked" in v["err"]]
    if panics:
        return "fail", f"panic in {panics}", out
    return check(all(v[0] not in (0, "timeout") for v in out.values()), "all three refuse with a message",
                 f"some accepted random bytes: {out}", out)


@scenario("gguf-empty", "gguf")
def s_empty():
    h = fresh("empty")
    e = h / "empty.gguf"
    e.write_bytes(b"")
    a = cli(h, "inspect", str(e))
    return check(a["rc"] not in (0, "timeout") and "panicked" not in a["err"],
                 f"empty file refused: {(a['out'] + a['err']).strip()[-120:]}", f"rc={a['rc']} {a['err'][-200:]}")


@scenario("offline-first-run", "network")
def s_offline():
    h = fresh("offline")
    bad = {"HTTPS_PROXY": "http://127.0.0.1:9", "HTTP_PROXY": "http://127.0.0.1:9", "ALL_PROXY": "http://127.0.0.1:9",
           "https_proxy": "http://127.0.0.1:9", "http_proxy": "http://127.0.0.1:9", "NO_PROXY": ""}
    r = cli(h, "ask", ARGS.model, "hi", timeout=120, extra=bad)
    msg = (r["out"] + r["err"]).strip()
    if r["rc"] == "timeout":
        return "fail", "hangs without network (120 s)", {"tail": msg[-400:]}
    if r["rc"] == 0:
        return "warn", "succeeded although the proxy is dead (proxy env ignored?)", {"tail": msg[-400:]}
    return check("panicked" not in msg, f"fails in {r['s']} s with: {msg.splitlines()[-1][:160] if msg else ''}",
                 "panic", {"tail": msg[-400:]})


@scenario("download-interrupt-resume", "network")
def s_resume():
    h = fresh("resume")
    if (prime() / "engines").exists():
        os.symlink(prime() / "engines", h / "engines")
    p = subprocess.Popen([ARGS.bin, "ask", ARGS.model, "hi"], env=env_for(h), stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, start_new_session=True)
    part, end = None, time.time() + 300
    while time.time() < end:
        parts = list(h.rglob("*.part"))
        if parts and parts[0].stat().st_size > 100 * 1024 * 1024:
            part = parts[0]
            break
        time.sleep(0.5)
    os.killpg(p.pid, signal.SIGKILL)
    p.wait()
    if part is None:
        return "fail", "no .part file above 100 MB within 300 s", {}
    size1 = part.stat().st_size
    t0 = time.time()
    r = cli(h, "ask", ARGS.model, "Name the capital of France in one word.", timeout=1800)
    log = r["out"] + r["err"]
    resumed = "resum" in log.lower() or any(f"{x:4.1f} %" in log for x in range(1, 100)) and " 0.0 %" not in log[:600]
    ok = r["rc"] == 0 and "paris" in r["out"].lower()
    data = {"part_bytes_at_kill": size1, "second_run_s": round(time.time() - t0, 1), "head": log[:600]}
    if not ok:
        return "fail", f"second run failed rc={r['rc']}", data
    return check(resumed, f"killed at {size1 >> 20} MiB, second run resumed and answered",
                 "second run answered, but the log does not show a resume (started from 0 %?)", data, warn=True)


@scenario("disk-write-limit", "disk")
def s_disk():
    if platform.system() == "Windows":
        raise Skip("ulimit not on Windows")
    h = fresh("disk")
    if (prime() / "engines").exists():
        os.symlink(prime() / "engines", h / "engines")
    sh = f"ulimit -f {200 * 1024 * 2}; exec \"$0\" \"$@\""  # 200 MiB in 512-byte blocks
    t0 = time.time()
    try:
        p = subprocess.run(["/bin/sh", "-c", sh, ARGS.bin, "ask", ARGS.model, "hi"], env=env_for(h),
                           capture_output=True, text=True, timeout=900)
        rc, msg = p.returncode, (p.stdout + p.stderr)
    except subprocess.TimeoutExpired:
        return "fail", "hangs when the disk refuses writes", {}
    tail = msg.strip().splitlines()[-1][:200] if msg.strip() else ""
    if rc == -signal.SIGXFSZ or rc == 128 + signal.SIGXFSZ:
        return "warn", "process killed by SIGXFSZ (file size limit); no own message", {"s": round(time.time() - t0, 1)}
    return check(rc != 0 and "panicked" not in msg, f"stops with: {tail}", f"rc={rc} {tail}", {"tail": msg[-500:]})


@scenario("ask-ok", "inference")
def s_ask():
    h = primed_home("ask")
    r = cli(h, "ask", ARGS.model, "What is 2+3? Answer with the number only.", timeout=300)
    left = engine_pids(h)
    data = {"s": r["s"], "out": r["out"][-300:]}
    if r["rc"] != 0:
        return "fail", f"rc={r['rc']} {(r['err'])[-300:]}", data
    if left:
        return "fail", f"llama-server still running after ask: {left}", data
    return check("5" in r["out"], f"answered in {r['s']} s (with load), engine gone afterwards",
                 f"wrong answer: {r['out'][-120:]!r}", data, warn=True)


@scenario("ask-port-8080-taken", "inference")
def s_ask_8080():
    h = primed_home("ask8080")
    blocker = socket.socket()
    blocker.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        blocker.bind(("127.0.0.1", 8080))
    except OSError:
        raise Skip("8080 is already in use on this machine")
    blocker.listen(1)
    try:
        r = cli(h, "ask", ARGS.model, "What is 2+3? Answer with the number only.", timeout=180)
    finally:
        blocker.close()
    msg = (r["out"] + r["err"])
    for pid in engine_pids(h):
        os.kill(pid, signal.SIGKILL)
    if r["rc"] == "timeout":
        return "fail", "hangs when port 8080 is taken", {"tail": msg[-400:]}
    if r["rc"] == 0 and "5" in r["out"]:
        return "pass", "answers on another port when 8080 is taken", {}
    return "fail", f"fails when 8080 is taken: {msg.strip().splitlines()[-1][:160] if msg.strip() else ''}", \
        {"tail": msg[-600:]}


@scenario("leftover-downloads", "disk")
def s_leftovers():
    d = prime() / "downloads"
    files = [p for p in d.rglob("*") if p.is_file()] if d.exists() else []
    size = sum(p.stat().st_size for p in files)
    return check(not files, "no leftovers after install", f"{len(files)} files ({size >> 20} MiB) left in downloads/",
                 {"files": [p.name for p in files]}, warn=True)


def run_until_ready(h: Path, t: float = 240) -> tuple[subprocess.Popen, str, str | None]:
    log = open(h / "run.log", "w")
    p = subprocess.Popen([ARGS.bin, "run", ARGS.model], env=env_for(h), stdout=log, stderr=subprocess.STDOUT,
                         start_new_session=True)
    end, url = time.time() + t, None
    while time.time() < end and p.poll() is None:
        txt = (h / "run.log").read_text(errors="replace")
        for tok in txt.split():
            if tok.startswith("http://127.0.0.1:"):
                url = tok.rstrip(".,)").removesuffix("/").removesuffix("/v1")
        if url and http("GET", url.rstrip("/") + "/v1/models", timeout=2).get("code") == 200:
            break
        time.sleep(1)
    return p, (h / "run.log").read_text(errors="replace"), url


@scenario("run-v1-chat", "inference")
def s_run_v1():
    h = primed_home("runv1")
    p, txt, url = run_until_ready(h)
    try:
        if not url:
            return "fail", "run printed no URL or never became ready", {"log": txt[-600:]}
        base = url.rstrip("/")
        models = http("GET", base + "/v1/models")
        chat = http("POST", base + "/v1/chat/completions",
                    {"model": "x", "messages": [{"role": "user", "content": "Say OK."}], "max_tokens": 8}, timeout=120)
        ok = models.get("code") == 200 and chat.get("code") == 200
        return check(ok, f"{base}: /v1/models and chat answer", f"models={models.get('code')} chat={chat.get('code')}",
                     {"chat": (chat.get("text") or "")[:300]})
    finally:
        os.killpg(p.pid, signal.SIGINT)
        try:
            p.wait(20)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)


@scenario("run-ctrl-c-during-load", "inference")
def s_ctrl_c():
    h = primed_home("ctrlc")
    log = open(h / "run.log", "w")
    p = subprocess.Popen([ARGS.bin, "run", ARGS.model], env=env_for(h), stdout=log, stderr=subprocess.STDOUT,
                         start_new_session=True)
    end = time.time() + 60
    while time.time() < end and not engine_pids(h):
        time.sleep(0.2)
    had = engine_pids(h)
    os.killpg(p.pid, signal.SIGINT)
    try:
        rc = p.wait(20)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        return "fail", "run does not end within 20 s after Ctrl+C", {}
    time.sleep(1)
    left = engine_pids(h)
    for pid in left:
        os.kill(pid, signal.SIGKILL)
    return check(not left, f"Ctrl+C while loading: exit {rc}, no engine left (engine seen: {bool(had)})",
                 f"engine survived Ctrl+C: {left}", {"log": (h / 'run.log').read_text()[-400:]})


@scenario("run-parent-killed", "inference")
def s_parent_kill():
    h = primed_home("pkill")
    p, txt, url = run_until_ready(h)
    pids = engine_pids(h)
    p.kill()  # SIGKILL only the parent, not the group: the engine must not outlive it for long
    p.wait()
    time.sleep(5)
    left = engine_pids(h)
    for pid in left:
        os.kill(pid, signal.SIGKILL)
    return check(not left, "engine ends when flelink is killed hard",
                 f"orphaned llama-server after kill -9 of flelink: {left} (seen before: {pids})", {}, warn=True)


@scenario("cli-studio-share", "studio")
def s_share():
    h = primed_home("share")
    st = Studio(h, extra_env={"FLE_STUDIO_HOME": str(h / "studio")})  # the defaults: two different folders
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        models = (st.api("GET", "/api/models").get("json") or {}).get("models") or []
        return check(bool(models), "Studio sees the model the CLI downloaded",
                     "Studio does not see the model `flelink ask` downloaded (CLI and Studio use different data "
                     "folders by default: FLE vs FLE Studio)", {"models": models})
    finally:
        st.stop()


@scenario("studio-start-ping", "studio")
def s_studio_ping():
    h = primed_home("st0")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio did not answer /api/ping", {"log": st.text()[-500:]}
        ping = st.api("GET", "/api/ping")
        page = http("GET", st.base + "/")
        return check(page.get("code") == 200 and "<html" in (page.get("text") or "").lower(),
                     f"page and API up, ping={ping.get('json')}", f"page={page.get('code')}", {"ping": ping.get("json")})
    finally:
        st.stop()


@scenario("studio-port-taken", "studio")
def s_port_taken():
    h = primed_home("st1")
    blocker = socket.socket()
    blocker.bind(("127.0.0.1", 0))
    blocker.listen(1)
    port = blocker.getsockname()[1]
    st = Studio(h, port=port)
    try:
        time.sleep(3)
        txt = st.text()
        moved = any(f":{port + i}" in txt for i in range(1, 20))
        alive = st.p.poll() is None
        return check(alive and moved, "moves to the next free port and says where", "did not move or did not say",
                     {"log": txt[-400:]})
    finally:
        st.stop()
        blocker.close()


@scenario("studio-second-instance", "studio")
def s_second():
    h = primed_home("st2")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "first studio not up", {}
        r = cli(h, "studio", "--no-browser", "--port", str(st.port), timeout=15)
        return check(r["rc"] == 0 and "already running" in r["out"], "second start points to the running one",
                     f"rc={r['rc']} {(r['out'] + r['err'])[-200:]}")
    finally:
        st.stop()


@scenario("studio-api-surface", "studio")
def s_surface():
    h = primed_home("st3")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        paths = ["/api/system", "/api/state", "/api/settings", "/api/models", "/api/engines", "/api/profiles",
                 "/api/live", "/api/engine", "/api/engine/log", "/api/catalog", "/api/downloads", "/api/chats",
                 "/api/tools", "/api/plugins", "/api/chat/capabilities",
                 "/api/bench", "/api/forge", "/api/tuning", "/api/quality", "/api/cache",
                 "/api/flags", "/api/mcp", "/api/remote", "/api/update", "/api/does-not-exist"]
        codes = {p: st.api("GET", p).get("code") for p in paths}
        five01 = [p for p, c in codes.items() if c == 501]
        errs = [p for p, c in codes.items() if c is None or c >= 500 and c != 501]
        if errs:
            return "fail", f"server errors on {errs}", codes
        return "warn" if five01 else "pass", f"{len(paths)} routes, {len(five01)} answer 501 not yet: {five01}", codes
    finally:
        st.stop()


@scenario("studio-hf-search", "studio")
def s_hf():
    h = primed_home("hf")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        short = st.api("GET", "/api/hf/search?q=l")
        found = st.api("GET", "/api/hf/search?q=Llama-3.2-1B-Instruct", timeout=60)
        repo = st.api("GET", "/api/hf/repo?repo=bartowski/Llama-3.2-1B-Instruct-GGUF", timeout=60)
        bad = st.api("GET", "/api/hf/repo?repo=not%20a%20repo")
        trick = st.api("POST", "/api/hf/download", {"repo": "bartowski/Llama-3.2-1B-Instruct-GGUF", "files": ["../../x.gguf"]},
                       timeout=60)
        res = (found.get("json") or {}).get("results") or []
        groups = (repo.get("json") or {}).get("groups") or []
        data = {"short": short.get("json"), "n_results": len(res), "n_groups": len(groups), "bad": bad.get("code"),
                "trick": (trick.get("code"), (trick.get("text") or "")[:120]),
                "first_group": groups[0] if groups else None}
        problems = []
        if (short.get("json") or {}).get("results") != []:
            problems.append("one-letter query not empty")
        if not res:
            problems.append("search found nothing")
        if not groups or not all("fit" in g for g in groups):
            problems.append("repo has no groups with fit")
        if bad.get("code") != 400:
            problems.append(f"bad repo answered {bad.get('code')}")
        if trick.get("code") not in (400,):
            problems.append(f"path trick answered {trick.get('code')}")
        return check(not problems, f"search {len(res)} repos, repo {len(groups)} quants with fit, bad input refused",
                     "; ".join(problems), data)
    finally:
        st.stop()


@scenario("studio-engine-lifecycle", "studio")
def s_engine_cycle():
    h = primed_home("st4")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        r = start_engine(st)
        if r.get("code") not in (200, 202):
            return "fail", f"engine/start {r.get('code')}: {r.get('text', '')[:200]}", {}
        s = wait_engine(st)
        state = s.get("state") or s.get("status")
        chat = st.api("POST", "/v1/chat/completions",
                      {"model": "x", "messages": [{"role": "user", "content": "Say OK."}], "max_tokens": 8}, timeout=120)
        dup = start_engine(st)
        stop = st.api("POST", "/api/engine/stop", {})
        time.sleep(3)
        left = engine_pids(h)
        data = {"state": s, "chat": chat.get("code"), "dup": (dup.get("code"), dup.get("text", "")[:160]),
                "stop": stop.get("code"), "left": left}
        if chat.get("code") != 200:
            return "fail", f"/v1 chat through studio: {chat.get('code')} (state {state})", data
        if left:
            return "fail", f"engine still running after stop: {left}", data
        return check(dup.get("code") not in (200, 202), "start, /v1 chat, refuses a second start, stop",
                     "accepted a second start while running", data, warn=True)
    finally:
        st.stop()
        for pid in engine_pids(h):
            os.kill(pid, signal.SIGKILL)


def stream_chat(base: str, abort_after: int | None = None, n_tokens: int = 200, headers: dict | None = None) -> dict:
    u = base.split("://", 1)[1]
    host, port = u.split("/")[0].split(":")
    conn = httpc.HTTPConnection(host, int(port), timeout=120)
    body = json.dumps({"model": "x", "stream": True, "max_tokens": n_tokens,
                       "messages": [{"role": "user", "content": "Count from 1 to 100, one number per line."}]})
    conn.request("POST", "/v1/chat/completions", body, {"Content-Type": "application/json", **(headers or {})})
    r = conn.getresponse()
    chunks, t0, first = 0, time.time(), None
    while True:
        line = r.fp.readline()
        if not line:
            break
        if line.startswith(b"data:"):
            chunks += 1
            first = first or time.time() - t0
            if abort_after and chunks >= abort_after:
                r.close()
                conn.close()
                return {"code": r.status, "chunks": chunks, "aborted": True, "ttft": first}
            if b"[DONE]" in line:
                break
    return {"code": r.status, "chunks": chunks, "aborted": False, "ttft": first, "s": time.time() - t0}


@scenario("studio-stream-abort-parallel", "studio")
def s_stream():
    h = primed_home("st5")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        start_engine(st)
        s = wait_engine(st)
        full = stream_chat(st.base)
        cut = stream_chat(st.base, abort_after=5)
        after = stream_chat(st.base, n_tokens=20)
        res, threads = [], []
        for _ in range(4):
            t = threading.Thread(target=lambda: res.append(stream_chat(st.base, n_tokens=40)))
            t.start()
            threads.append(t)
        for t in threads:
            t.join(180)
        par_ok = len(res) == 4 and all(x["code"] == 200 and x["chunks"] > 3 for x in res)
        data = {"full": full, "cut": cut, "after_cut": after, "parallel": res, "engine": s.get("state")}
        ok = full["code"] == 200 and full["chunks"] > 10 and after["code"] == 200 and after["chunks"] > 3
        if not ok:
            return "fail", "streaming broken or engine unusable after an aborted stream", data
        return check(par_ok, f"stream {full['chunks']} chunks, abort ok, 4 parallel ok, ttft {full['ttft']:.2f} s",
                     "parallel streams failed", data)
    finally:
        st.stop()
        for pid in engine_pids(h):
            os.kill(pid, signal.SIGKILL)


@scenario("studio-engine-killed", "studio")
def s_engine_killed():
    h = primed_home("st6")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        start_engine(st)
        wait_engine(st)
        pids = engine_pids(h)
        if not pids:
            return "fail", "no engine process found", {}
        for pid in pids:
            os.kill(pid, signal.SIGKILL)
        time.sleep(4)
        s = (st.api("GET", "/api/engine").get("json") or {})
        chat = st.api("POST", "/v1/chat/completions",
                      {"model": "x", "messages": [{"role": "user", "content": "hi"}], "max_tokens": 4}, timeout=30)
        restart = start_engine(st)
        s2 = wait_engine(st)
        data = {"after_kill": s, "chat_code": chat.get("code"), "chat": chat.get("text", "")[:200],
                "restart": restart.get("code"), "after_restart": s2}
        state = str(s.get("state") or s.get("status"))
        noticed = state not in ("ready", "running") or s.get("ready") is False
        good_err = chat.get("code") in (502, 503, 409)
        restarted = bool(s2.get("ready")) or "ready" in str(s2.get("state"))
        problems = [x for x, ok in (("status still says ready", noticed), ("chat error code unclear", good_err),
                                    ("restart failed", restarted)) if not ok]
        return check(not problems, f"crash seen (state {state}), chat {chat.get('code')}, restart works",
                     "; ".join(problems), data)
    finally:
        st.stop()
        for pid in engine_pids(h):
            os.kill(pid, signal.SIGKILL)


@scenario("studio-stop-kills-engine", "studio")
def s_stop_kills():
    h = primed_home("st7")
    st = Studio(h)
    if not st.wait():
        st.stop()
        return "fail", "studio not up", {}
    start_engine(st)
    wait_engine(st)
    had = engine_pids(h)
    st.p.send_signal(signal.SIGTERM)
    try:
        st.p.wait(20)
    except subprocess.TimeoutExpired:
        pass
    time.sleep(2)
    left = engine_pids(h)
    st.stop()
    for pid in left:
        os.kill(pid, signal.SIGKILL)
    return check(had and not left, "SIGTERM to studio ends the engine too", f"engine left: {left} (had {had})")


@scenario("studio-bad-requests", "studio")
def s_bad_requests():
    h = primed_home("st8")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        tests = {
            "invalid json": st.api("POST", "/api/settings", b"{not json", headers={"Content-Type": "application/json"}),
            "huge body": st.api("POST", "/api/settings", b"{\"a\":\"" + b"x" * (12 << 20) + b"\"}"),
            "start bogus model": st.api("POST", "/api/engine/start", {"model": "/nope/missing.gguf"}),
            "path traversal": http("GET", st.base + "/../../../../etc/passwd"),
            "traversal encoded": http("GET", st.base + "/%2e%2e/%2e%2e/%2e%2e/etc/passwd"),
            "chat w/o engine": st.api("POST", "/v1/chat/completions", {"messages": []}),
        }
        codes = {k: (v.get("code"), (v.get("text") or v.get("error") or "")[:120]) for k, v in tests.items()}
        problems = []
        if any("root:" in (v.get("text") or "") for v in tests.values()):
            problems.append("PATH TRAVERSAL serves /etc/passwd")
        if codes["invalid json"][0] in (200, None) or (codes["invalid json"][0] or 0) >= 500:
            problems.append("invalid json not a 4xx")
        if codes["huge body"][0] in (200, None):
            problems.append("12 MB body accepted")
        if codes["start bogus model"][0] in (200, 202):
            problems.append("start with missing model accepted")
        if st.p.poll() is not None:
            problems.append("studio died")
        return check(not problems, "all refused with 4xx, studio still up", "; ".join(problems), codes)
    finally:
        st.stop()


@scenario("studio-ui-browser", "studio")
def s_ui():
    smoke = Path(__file__).resolve().parents[1] / "e2e" / "studio_smoke.py"
    try:
        import playwright  # noqa: F401
    except ImportError:
        raise Skip("playwright not installed for this python")
    h = primed_home("ui")
    st = Studio(h)
    try:
        if not st.wait():
            return "fail", "studio not up", {}
        r = subprocess.run([sys.executable, str(smoke), "--url", st.base], capture_output=True, text=True, timeout=300)
        return check(r.returncode == 0, "browser smoke test passes", (r.stdout + r.stderr)[-400:],
                     {"out": (r.stdout + r.stderr)[-1500:]})
    finally:
        st.stop()


# ---- host mode

def host_studio(name: str, *extra: str) -> tuple[Studio, str]:
    h = primed_home(name)
    st = Studio(h, "--host", "0.0.0.0", *extra)
    if not st.wait():
        raise RuntimeError("host studio not up: " + st.text()[-400:])
    key, lines = "", st.text().splitlines()
    for i, line in enumerate(lines):
        if "Access key" in line and i + 1 < len(lines):
            key = lines[i + 1].strip()
    return st, key


def lan_ip() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("192.0.2.1", 9))  # no packet is sent; this only picks the outgoing interface
        return s.getsockname()[0]
    finally:
        s.close()


@scenario("host-auth", "host")
def s_host_auth():
    st, key = host_studio("h0", "--no-tls")
    try:
        lan = f"http://{lan_ip()}:{st.port}"
        no = http("GET", lan + "/api/state")
        bad = http("GET", lan + "/api/state", headers={"Authorization": "Bearer wrong"})
        good = http("GET", lan + "/api/state", headers={"Authorization": f"Bearer {key}"})
        v1 = http("GET", lan + "/v1/models")
        page = http("GET", lan + "/")
        host_hdr = http("GET", lan + "/api/state", headers={"Authorization": f"Bearer {key}", "Host": "evil.example"})
        local = st.api("GET", "/api/state")
        t0 = time.time()
        for _ in range(5):
            http("POST", lan + "/login", b"key=wrong", headers={"Content-Type": "application/x-www-form-urlencoded"})
        five = time.time() - t0
        codes = {"no_key": no.get("code"), "bad": bad.get("code"), "good": good.get("code"), "v1_nokey": v1.get("code"),
                 "page_nokey": page.get("code"), "evil_host": host_hdr.get("code"), "5_bad_logins_s": round(five, 1), "loopback_nokey": local.get("code"),
                 "key_found": bool(key)}
        problems = []
        if not key:
            problems.append("no access key printed")
        if no.get("code") != 401 or bad.get("code") != 401:
            problems.append("API open without the right key")
        if v1.get("code") != 401:
            problems.append("/v1 open without key")
        if good.get("code") != 200:
            problems.append("right key refused")
        if five < 4:
            problems.append("wrong logins not slowed down")
        if host_hdr.get("code") == 200:
            problems.append("foreign Host header accepted (DNS rebinding)")
        return check(not problems, "key enforced on /api and /v1, logins slowed, foreign Host refused",
                     "; ".join(problems), codes)
    finally:
        st.stop()


@scenario("host-tls", "host")
def s_host_tls():
    st, key = host_studio("h1")
    try:
        txt = st.text()
        fp_line = next((l for l in txt.splitlines() if "SHA-256" in l or "fingerprint" in l.lower()), "")
        ctx = ssl.create_default_context()
        strict = http("GET", st.base + "/api/ping", ctx=ctx)
        loose = http("GET", st.base + "/api/ping", ctx=ssl._create_unverified_context())
        plain = http("GET", f"http://127.0.0.1:{st.port}/api/ping")
        pem = ssl.get_server_certificate(("127.0.0.1", st.port))
        der = ssl.PEM_cert_to_DER_cert(pem)
        import hashlib
        fp = hashlib.sha256(der).hexdigest()
        printed = fp_line.replace(":", "").lower()
        keyfile = next((p for p in st.home.rglob("*key*.pem")), None)
        mode = oct(keyfile.stat().st_mode & 0o777) if keyfile else None
        data = {"fp_line": fp_line, "real_fp": fp, "strict": strict.get("error", strict.get("code")),
                "loose": loose.get("code"), "plain_http": plain.get("code") or plain.get("error"), "keyfile_mode": mode}
        problems = []
        if fp not in printed:
            problems.append("printed fingerprint does not match the certificate")
        if strict.get("code") is not None:
            problems.append("self-signed cert validated by default store?")
        if loose.get("code") not in (200, 401):
            problems.append("https does not answer")
        if mode and mode not in ("0o600", "0o400"):
            problems.append(f"key file mode {mode}")
        return check(not problems, "https with matching fingerprint, key file 0600", "; ".join(problems), data)
    finally:
        st.stop()


@scenario("host-cert-deleted", "host")
def s_cert_deleted():
    st, _ = host_studio("h2")
    st.stop()
    tls = next((p for p in st.home.rglob("tls") if p.is_dir()), None)
    if not tls:
        return "fail", "no tls folder", {}
    first = {p.name: p.read_bytes() for p in tls.iterdir()}
    for p in tls.iterdir():
        p.unlink()
    st2 = Studio(st.home, "--host", "0.0.0.0")
    try:
        up = st2.wait()
        new = {p.name: p.read_bytes() for p in tls.iterdir()} if tls.exists() else {}
        changed = new and new != first
        return check(up and changed, "new certificate made after deletion; clients must check the new fingerprint",
                     f"up={up} new={bool(new)}", {"log": st2.text()[-300:]})
    finally:
        st2.stop()


# ---- installer

@scenario("installer-checksum", "install")
def s_installer():
    if platform.system() == "Windows":
        raise Skip("install.sh is for macOS and Linux")
    repo = Path(__file__).resolve().parents[2]
    srv_dir = Path(ARGS.work) / "dist"
    if srv_dir.exists():
        shutil.rmtree(srv_dir)
    srv_dir.mkdir(parents=True)
    script = (repo / "install.sh").read_text()
    names = sorted({w.strip("\"'") for w in script.replace("/", " ").split() if w.startswith(("\"flelink-", "flelink-"))})
    return "skip", f"installer needs a release layout; names seen in install.sh: {names[:6]}", {}


# ---------------------------------------------------------------------------------------------------------- main

def write_report():
    work = Path(ARGS.work)
    old = work / "results.json"
    if old.exists():  # keep earlier results of scenarios not run this time
        try:
            prev = json.loads(old.read_text()).get("results", [])
        except Exception:  # noqa: BLE001
            prev = []
        ids = {r["id"] for r in RESULTS}
        order = [s for s, _, _ in SCENARIOS]
        merged = {r["id"]: r for r in prev if r["id"] not in ids}
        merged.update({r["id"]: r for r in RESULTS})
        RESULTS_ALL = sorted(merged.values(), key=lambda r: order.index(r["id"]) if r["id"] in order else 999)
    else:
        RESULTS_ALL = RESULTS
    meta = {"bin": ARGS.bin, "os": f"{platform.system()} {platform.release()} {platform.machine()}",
            "when": time.strftime("%Y-%m-%d %H:%M:%S"), "model": ARGS.model}
    (work / "results.json").write_text(json.dumps({"meta": meta, "results": RESULTS_ALL}, indent=2, default=str))
    lines = [f"# Fulltest {meta['os']} · {meta['when']}", "",
             "| Status | Fall | Bereich | Befund |", "|---|---|---|---|"]
    for r in RESULTS_ALL:
        lines.append(f"| {r['status'].upper()} | `{r['id']}` | {r['area']} | {str(r['note']).replace('|', '/')} |")
    (work / "results.md").write_text("\n".join(lines) + "\n")


def main():
    global ARGS
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--model", default="llama-3.2-1b")
    ap.add_argument("--model-file", default="Llama-3.2-1B")
    ap.add_argument("--only", default="")
    ap.add_argument("--skip", default="")
    ARGS = ap.parse_args()
    ARGS.bin = str(Path(ARGS.bin).resolve())
    Path(ARGS.work).mkdir(parents=True, exist_ok=True)
    only = {x for x in ARGS.only.split(",") if x}
    skip = {x for x in ARGS.skip.split(",") if x}
    for sid, area, fn in SCENARIOS:
        if (only and sid not in only) or sid in skip:
            continue
        try:
            status, note, data = fn()
            result(sid, area, status, note, data)
        except Skip as s:
            result(sid, area, "skip", str(s))
        except Exception as e:  # noqa: BLE001
            result(sid, area, "fail", f"test error: {type(e).__name__}: {e}", {"trace": traceback.format_exc()[-1500:]})
        write_report()
    n = {k: sum(1 for r in RESULTS if r["status"] == k) for k in ("pass", "warn", "fail", "skip")}
    print(f"\n{n['pass']} pass, {n['warn']} warn, {n['fail']} fail, {n['skip']} skip -> {Path(ARGS.work) / 'results.md'}")


if __name__ == "__main__":
    main()
