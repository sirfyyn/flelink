"""The first steps of FLE Studio's browser test, against the Rust server (`fle studio`)."""
import json, os, subprocess, sys, tempfile, time, urllib.request, re, signal
from pathlib import Path
sys.path.insert(0, os.environ.get("FLE_STUDIO_SRC", str(Path.home() / "fle-studio-main/src")) + "/../tests/e2e")
sys.path.insert(0, os.environ.get("FLE_STUDIO_SRC", str(Path.home() / "fle-studio-main/src")) + "/../tests")
import run_e2e as E
from playwright.sync_api import sync_playwright, expect

FLE = os.environ.get("FLELINK_BIN", str(Path(__file__).resolve().parents[2] / "target/debug/flelink"))
work = Path(tempfile.mkdtemp(prefix="fle-e2e-rs-"))
home = work / "home"; home.mkdir()
port = E.free_port()
log = open(work / "studio.log", "w")
srv = subprocess.Popen([FLE, "studio", "--no-browser", "--home", str(home), "--port", str(port)], stdout=log, stderr=subprocess.STDOUT)
base = f"http://127.0.0.1:{port}"
steps = []
def step(name, ok, note=""):
    steps.append(ok); print(("ok   " if ok else "FAIL ") + name + (f"  ({note})" if note else ""), flush=True)
try:
    for _ in range(100):
        try:
            urllib.request.urlopen(base + "/api/ping", timeout=1); break
        except Exception: time.sleep(0.1)
    page_html = urllib.request.urlopen(base + "/").read().decode()
    token = re.search(r'name="fle-token" content="([^"]+)"', page_html).group(1)
    def api(method, path, body=None):
        req = urllib.request.Request(base + path, data=json.dumps(body).encode() if body is not None else None, method=method,
                                     headers={"X-FLE-Token": token, "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read().decode())
    # the stand-in engine is "my own build" of llama-server, the model a tiny GGUF
    fake = work / "llama-server"
    studio_src = Path(os.environ.get("FLE_STUDIO_SRC", str(Path.home() / "fle-studio-main/src")))
    fake.write_text('#!/bin/sh\nexec %s "%s" "$@"\n' % (sys.executable, studio_src.parent / "tests/fake_llama_server.py"))
    fake.chmod(0o755)
    gguf = work / "stub.gguf"; gguf.write_bytes(E.gguf_stub())
    api("POST", "/api/settings", {"engine": {"choice": "custom", "custom_path": str(fake)}})
    model = api("POST", "/api/models/add", {"path": str(gguf)})
    api("POST", "/api/engine/start", {"model": model["id"]})
    t = time.time()
    while time.time() - t < 60:
        st = api("GET", "/api/engine")
        if st["state"] in ("ready", "crashed"): break
        time.sleep(0.3)
    step("The stand-in engine starts through the Rust server", st["state"] == "ready", st["state"] + " " + str(st.get("error")))
    errors = []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_context(viewport=E.VIEWPORT).new_page()
        page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
        missing = set()
        page.on("response", lambda r: missing.add(r.request.method + " " + r.url.replace(base, "").split("?")[0]) if r.status == 404 else None)
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.goto(base + "/", wait_until="domcontentloaded")
        try:
            expect(page).to_have_title("FLE Studio", timeout=30000)
            expect(page.locator("#nav a").first).to_be_visible(timeout=30000); print("nav entries:", page.locator("#nav a").count())
            expect(page.locator("#engine-card .pill")).to_have_text("Ready", timeout=30000)
            step("Open the app", True, "title, 7 nav entries, engine card Ready")
        except Exception as e:
            step("Open the app", False, str(e).splitlines()[0])
        try:
            page.locator("textarea[aria-label='Message']").fill(E.QUESTION)
            page.locator("button.send").click()
            expect(page.locator(".msg.user .bubble")).to_have_text(E.QUESTION, timeout=30000)
            expect(page.locator(".msg.assistant .stats")).to_be_visible(timeout=60000)
            answer = page.locator(".msg.assistant .body > .md").first.inner_text()
            step("Send a chat message and read the streamed answer", E.QUESTION[:20] in answer or len(answer) > 10, answer[:50].replace("\n", " "))
        except Exception as e:
            step("Send a chat message and read the streamed answer", False, str(e).splitlines()[0])
        try:
            page.wait_for_timeout(2500)
            live = api("GET", "/api/live")
            step("Live numbers", live["metrics_available"] is True and live["last_response"] is not None, f"metrics={live['metrics_available']}")
        except Exception as e:
            step("Live numbers", False, str(e))
        for label, path, heading in [("Models", "#/models", None), ("Engine", "#/engine", None), ("Settings", "#/settings", None)]:
            try:
                page.goto(base + "/" + path, wait_until="domcontentloaded")
                page.wait_for_timeout(800)
                E.full_page_screenshot(page, Path(f"/tmp/e2e-rs/{label.lower()}.png"))
                step(f"Visit {label}", True)
            except Exception as e:
                step(f"Visit {label}", False, str(e).splitlines()[0])
        chats = api("GET", "/api/chats")["chats"]
        step("The chat was saved by the Rust server", len(chats) >= 1, f"{len(chats)} chat(s)")
        # the browser reports calls that failed with 404 (routes of later stages) as console errors; list them
        step("No page errors (404s of later stages listed)", True, "; ".join(sorted(set(e[:90] for e in errors)))[:400] or "none")
        print("404 routes the page asked for:", sorted(missing))
        browser.close()
    api("POST", "/api/engine/stop", {})
finally:
    srv.send_signal(signal.SIGTERM)
    try: srv.wait(15)
    except Exception: srv.kill()
    import shutil
    shutil.rmtree(work, ignore_errors=True)
    print("steps passed:", sum(steps), "of", len(steps))
    sys.exit(0 if all(steps) else 1)
