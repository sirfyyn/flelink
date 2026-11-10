#!/usr/bin/env python3
"""Prüft die Seite flelink.run vor dem Ausrollen gegen das Repo. Schreibt nichts.

  python3 -I site/pruefen.py        aus der Wurzel des Repos; Exit 1 bei einer Abweichung

Regeln:
  version      `softwareVersion` im JSON-LD und jedes „FLE Link X.Y.Z“ im Text nennen die Version aus Cargo.toml
  platzhalter  kein {PLATZHALTER} im HTML
  sitemap      jede Seite in sitemap.xml gibt es als Datei, und keine davon trägt noindex
  links        jede lokale Datei, auf die index.html zeigt (href, src, poster), liegt in site/ (install.sh und install.ps1
               kommen beim Ausrollen aus der Wurzel dazu)

ausrollen.sh ruft dieses Skript vor dem rsync auf. Die Messzahlen der Seite hält im FYOS-Repo
site/gemeinsam/zahlen_pruefen.py gegen sirfyyn.de (eine Quelle für beide Seiten).
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

SITE = Path(__file__).resolve().parent
WURZEL = SITE.parent


def main() -> int:
    funde: list[str] = []
    cargo = (WURZEL / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"', cargo, flags=re.S | re.M)
    if not m:
        print("Cargo.toml: keine Version unter [workspace.package] gefunden")
        return 1
    version = m.group(1)
    for datei in sorted(SITE.glob("*.html")):
        roh = datei.read_text(encoding="utf-8")
        for v in re.findall(r'"softwareVersion"\s*:\s*"([^"]+)"', roh):
            if v != version:
                funde.append(f"{datei.name}: version softwareVersion {v} statt {version}")
        for v in re.findall(r"FLE Link v?(\d+\.\d+\.\d+)", roh):
            if v != version:
                funde.append(f"{datei.name}: version „FLE Link {v}“ statt {version}")
        for p in re.findall(r"\{[A-Z_0-9]+\}", roh):
            funde.append(f"{datei.name}: platzhalter {p}")
    for loc in re.findall(r"<loc>https://flelink\.run/([^<]*)</loc>", (SITE / "sitemap.xml").read_text(encoding="utf-8")):
        ziel = SITE / (loc or "index.html")
        if not ziel.is_file():
            funde.append(f"sitemap.xml: sitemap /{loc} gibt es nicht")
        elif re.search(r'name="robots"[^>]*noindex', ziel.read_text(encoding="utf-8")):
            funde.append(f"sitemap.xml: sitemap /{loc} trägt noindex")
    index = (SITE / "index.html").read_text(encoding="utf-8")
    for ziel in sorted(set(re.findall(r'(?:href|src|poster)="([^"#:?]+)(?:[?#][^"]*)?"', index))):
        if ziel in ("./", "install.sh", "install.ps1"):
            continue
        if not (SITE / ziel).is_file():
            funde.append(f"index.html: links {ziel} fehlt in site/")
    for f in funde:
        print(f)
    print(f"FLE Link {version}: {len(funde)} Abweichungen.")
    return 1 if funde else 0


if __name__ == "__main__":
    sys.exit(main())
