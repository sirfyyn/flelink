#!/usr/bin/env python3
"""Which English texts of the Studio pages have no entry in web/studio/js/i18n-<lang>.js?

Static: string literals of web/studio/js and web/plugins that look like shown text (a capital first, a space or one
word), with "a" + "b" chains joined. A text with ${…} needs a pattern (P); it counts as covered when a pattern matches
it with each ${…} replaced by a sample value. Prints a count per file; --json FILE writes the texts.

    python3 ci/i18n_gaps.py de            # report
    python3 ci/i18n_gaps.py de --max 0    # exit 1 when more than 0 texts are missing
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent if (Path(__file__).resolve().parent.parent / "web").is_dir() else Path(sys.argv[sys.argv.index("--root") + 1])
SKIP = re.compile(r'^(GET|POST|PUT|DELETE|HEAD|Bearer|Content-|Accept|X-|M\d)\b')
REGEX_AFTER = set("(,=:[!&|?{};\n")


def tokens(js: str):
    """(start, end, text) of every string literal, in order. Comments and regex literals are skipped; a template literal
    is one token with its ${…} parts kept as written (strings inside those parts are tokens of their own)."""
    out, i, n, last = [], 0, len(js), "\n"
    while i < n:
        c = js[i]
        if c == "/" and js[i + 1:i + 2] == "/":
            i = js.find("\n", i)
            i = n if i < 0 else i
            continue
        if c == "/" and js[i + 1:i + 2] == "*":
            i = js.find("*/", i) + 2
            continue
        if c == "/" and last in REGEX_AFTER:
            j, cls = i + 1, False
            while j < n and (js[j] != "/" or cls) and js[j] != "\n":
                if js[j] == "\\":
                    j += 1
                elif js[j] == "[":
                    cls = True
                elif js[j] == "]":
                    cls = False
                j += 1
            i = j + 1
            last = "x"
            continue
        if c in "\"'":
            j = i + 1
            while j < n and js[j] != c and js[j] != "\n":
                j += 2 if js[j] == "\\" else 1
            raw = js[i + 1:j]
            try:
                text = json.loads('"' + raw.replace('"', '\\"') + '"') if c == "'" else json.loads(js[i:j + 1])
            except ValueError:
                text = raw
            out.append((i, j + 1, text))
            i, last = j + 1, "x"
            continue
        if c == "`":
            j, depth, inner = i + 1, 0, []
            while j < n and (js[j] != "`" or depth):
                if js[j] == "\\":
                    j += 2
                    continue
                if js[j] == "$" and js[j + 1:j + 2] == "{" and not depth:
                    depth, start = 1, j + 2
                    j += 2
                    while j < n and depth:
                        if js[j] == "{":
                            depth += 1
                        elif js[j] == "}":
                            depth -= 1
                        elif js[j] in "\"'`":  # a string inside the expression: jump over it
                            q, j = js[j], j + 1
                            while j < n and js[j] != q:
                                j += 2 if js[j] == "\\" else 1
                        j += 1
                    inner.append((start, j - 1))
                    continue
                j += 1
            out.append((i, j + 1, js[i + 1:j].replace("\\`", "`").replace("\\n", "\n")))
            for s, e in inner:  # the strings inside ${…} are texts too (a ? "x" : "y")
                out.extend((s + a, s + b, x) for a, b, x in tokens(js[s:e]))
            i, last = j + 1, "x"
            continue
        if not c.isspace():
            last = c
        i += 1
    return sorted(out)


def texts(js: str):
    """Shown texts: literals, with "a" + "b" chains joined."""
    toks = tokens(js)
    k = 0
    while k < len(toks):
        s, e, text = toks[k]
        while k + 1 < len(toks) and toks[k + 1][0] >= e and re.fullmatch(r"\s*\+\s*", js[e:toks[k + 1][0]]):
            k += 1
            e, text = toks[k][1], text + toks[k][2]
        yield text
        k += 1


def shown(s: str) -> bool:
    s = s.strip()
    if len(s) < 4 or not re.search(r"[A-Za-z]{3}", s) or not re.match(r"^[A-Z“(·]", s):
        return False
    if " " not in s and not re.match(r"^[A-Z][a-z]+…?$", s):
        return False
    return not SKIP.search(s)


def dictionary(lang: str):
    src = (ROOT / f"web/studio/js/i18n-{lang}.js").read_text()
    keys = set()
    for m in re.finditer(r'("(?:[^"\\\n]|\\.)*")\s*:', src):
        try:
            keys.add(json.loads(m.group(1)))
        except ValueError:
            pass
    pats = []
    for m in re.finditer(r"\[\s*/((?:[^/\\\n]|\\.)+)/([a-z]*)\s*,", src):
        try:
            pats.append(re.compile(m.group(1)))
        except re.error:
            pass
    return keys, pats


def main() -> int:
    lang = sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("-") else "de"
    keys, pats = dictionary(lang)
    out = {}
    files = sorted((ROOT / "web/studio/js").glob("*.js")) + sorted((ROOT / "web/plugins").rglob("*.js"))
    for f in files:
        if f.name.startswith("i18n"):
            continue
        miss = set()
        for s in texts(f.read_text()):
            t = s.strip()
            if not shown(t) or t in keys:
                continue
            sample = re.sub(r"\$\{[^}]*\}", "7", t)
            if any(p.search(sample) for p in pats) or any(p.search(t) for p in pats):
                continue
            miss.add(t)
        if miss:
            out[str(f.relative_to(ROOT))] = sorted(miss)
    total = sum(len(v) for v in out.values())
    for k, v in out.items():
        print(f"{len(v):4}  {k}")
    print(f"{total:4}  texts without a {lang} entry ({len(keys)} entries, {len(pats)} patterns)")
    if "--json" in sys.argv:
        Path(sys.argv[sys.argv.index("--json") + 1]).write_text(json.dumps(out, indent=1, ensure_ascii=False))
    if "--max" in sys.argv:
        return 1 if total > int(sys.argv[sys.argv.index("--max") + 1]) else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
