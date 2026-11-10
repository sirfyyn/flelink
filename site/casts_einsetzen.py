#!/usr/bin/env python3
"""Schreibt den Endzustand der Terminal-Aufzeichnungen in die Seite, für Leser ohne JavaScript und für Suchmaschinen.

In index.html steht je Aufzeichnung ein `<pre class="cast-screen" data-end="casts/NAME.cast">…</pre>`. Dieses Skript ersetzt den
Inhalt durch das, was am Ende der Aufzeichnung im Terminal steht (die letzten Zeilen, ohne Farbcodes). player.js spielt
darüber die Aufzeichnung selbst ab; ohne JavaScript bleibt dieser Text stehen.

  python3 -I site/casts_einsetzen.py            schreibt index.html neu, wenn sich etwas geändert hat
  python3 -I site/casts_einsetzen.py --pruefen  schreibt nichts, Exit 1 wenn die Seite nicht zum Stand der Aufzeichnungen passt

Nach jeder neuen oder geänderten Aufzeichnung ausführen. pruefen.py ruft `--pruefen` vor dem Ausrollen auf.
"""
from __future__ import annotations

import html
import json
import re
import sys
from pathlib import Path

SITE = Path(__file__).resolve().parent
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
ZEILEN = 26  # so viele Zeilen wie player.js höchstens zeigt


def endzustand(datei: Path) -> str:
    """Wie player.js (Screen): \\r setzt an den Zeilenanfang, \\n beginnt eine neue Zeile, Schreiben ersetzt."""
    zeilen: list[list[str]] = [[]]
    spalte = 0
    for roh in datei.read_text(encoding="utf-8").splitlines()[1:]:
        if not roh.strip():
            continue
        _, art, daten = json.loads(roh)
        if art != "o":
            continue
        for z in ANSI.sub("", daten):
            if z == "\r":
                spalte = 0
            elif z == "\n":
                zeilen.append([])
                spalte = 0
            else:
                zeile = zeilen[-1]
                while len(zeile) < spalte:
                    zeile.append(" ")
                if spalte < len(zeile):
                    zeile[spalte] = z
                else:
                    zeile.append(z)
                spalte += 1
    text = ["".join(z).rstrip() for z in zeilen]
    while text and not text[-1]:
        text.pop()
    return html.escape("\n".join(text[-ZEILEN:]), quote=False)


def main() -> int:
    pruefen = "--pruefen" in sys.argv
    index = SITE / "index.html"
    alt = index.read_text(encoding="utf-8")
    fehlt: list[str] = []

    def ersetzen(m: re.Match) -> str:
        datei = SITE / m.group(2)
        if not datei.is_file():
            fehlt.append(m.group(2))
            return m.group(0)
        return f"{m.group(1)}{endzustand(datei)}</pre>"

    neu = re.sub(r'(<pre class="cast-screen" data-end="([^"]+)">).*?</pre>', ersetzen, alt, flags=re.S)
    for f in fehlt:
        print(f"fehlt: {f}")
    if fehlt:
        return 1
    if neu == alt:
        print("Endzustände stimmen.")
        return 0
    if pruefen:
        print("index.html passt nicht zu den Aufzeichnungen: python3 -I site/casts_einsetzen.py ausführen.")
        return 1
    index.write_text(neu, encoding="utf-8")
    print("index.html: Endzustände der Aufzeichnungen eingesetzt.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
