# FLE Link: Windows-Test am 14.11.2026

Testbau aus der CI, kein Release. Zweig `feat/windows-test`, Entwurfs-PR #90. Geprüft werden zwei Dinge, die es unter
Windows bisher nicht gab: die **Computer-Bedienung** (Bildschirm, Maus, Tastatur für den Agenten) und das
**Selbst-Update der Desktop-App** über den Setup-Installer des Releases.

Rechner: Titan unter Windows (externe NVMe, `titan-win`). Vorher: keine Messung auf Titan, Katy aus.

## 0. Download

CI-Lauf mit den Installern (manueller Lauf `app`, `bundle=["windows-latest"]`):
<https://github.com/sirfyyn/flelink/actions/runs/43578849522>

Unten auf der Seite unter **Artifacts** liegt `app-windows-latest` (zip, 25,5 MB, nur mit GitHub-Anmeldung, bis
21.11.), direkt: <https://github.com/sirfyyn/flelink/actions/runs/43578849522/artifacts/14107777222>. Gebaut aus Commit
`cfb1d52` (der Commit danach ändert nur diese Anleitung). Darin:

| Datei | Wozu |
|---|---|
| `app/target/release/bundle/nsis/FLE Link_0.1.14_x64-setup.exe` | der Setup-Installer des Testbaus (dieser wird getestet) |
| `app/target/release/bundle/msi/FLE Link_0.1.14_x64_en-US.msi` | msi, für diesen Test **nicht** nehmen |
| `target/release/flelink.exe`, `fle.exe` | das Programm des Testbaus, für `desktop-probe` im Terminal |

Die zip nach `%USERPROFILE%\Downloads\flelink-test` entpacken. Die Dateien sind nicht signiert: SmartScreen zeigt
„Weitere Informationen“, dann „Trotzdem ausführen“.

## 1. Grundtest (offizielles 0.1.14)

In PowerShell (ohne Administrator):

```powershell
irm https://flelink.run/install.ps1 | iex
```

- Die Frage nach der Desktop-App mit `y` beantworten (installiert das offizielle Setup 0.1.14).
- Neues Terminal öffnen, `flelink --version` zeigt 0.1.14.
- `flelink studio` (oder die App): Engine installieren, ein kleines Modell starten (z. B. ein 1–4B-GGUF), im Studio eine
  Frage stellen. Beobachten: Antwort kommt, Live-Panel zeigt Werte.

## 2. Testbau installieren

1. Die App schließen.
2. `FLE Link_0.1.14_x64-setup.exe` aus der zip starten. Gleiche Version wie installiert: das Setup fragt, was es tun soll; die
   erste Wahl (neu installieren bzw. „Add/Reinstall components“) nehmen, nicht „Deinstallieren“. Ziel ist dasselbe Verzeichnis wie beim offiziellen Setup.
3. Prüfen, wo die App liegt (wird für Schritt 4 gebraucht):

```powershell
Get-ChildItem "$env:LOCALAPPDATA\FLE Link\*.exe"
```

Erwartet: `flelink-app.exe` (und `uninstall.exe`). Wenn die App woanders liegt (etwa `C:\Program Files\FLE Link`), war es
die msi: dann geht das Selbst-Update nicht (gewollt, die App zeigt dann den Link zur Release-Seite).

## 3. Computer-Bedienung

### 3a. Ohne Agent, im Terminal

```powershell
cd $env:USERPROFILE\Downloads\flelink-test\target\release
.\flelink.exe desktop-probe --out $env:TEMP\probe.png
start $env:TEMP\probe.png
.\flelink.exe desktop-probe --out $env:TEMP\probe.png --tippen "hallo von FLE Link"
```

Beobachten:

- Ausgabe nennt `status: {"ready":true,"problem":"",...}`, die Bildgröße und „mouse: moved to …“.
- **Die Bildgröße ist die echte Auflösung des Hauptbildschirms** (bei 4K und 150 % Skalierung 3840 x 2160, nicht 2560 x 1440).
- Der Mauszeiger springt in die Mitte des Hauptbildschirms.
- Das Bild zeigt den ganzen Hauptbildschirm, scharf, Farben richtig (nicht blau-rot vertauscht).
- Mit `--tippen`: nach zwei Sekunden steht „hallo von FLE Link“ in der Eingabezeile des Terminals (Umlaute gehen auch:
  `--tippen "äöü ß €"`).

### 3b. In der App, mit dem Agenten

Voraussetzung: der Agent läuft (Seite **Agent**, braucht git und Node 22.19 oder neuer) und ein Modell ist geladen.

1. Seite **Agent** → Karte **Computer-Bedienung**. Erwartet: kein Hinweis „not available on Windows“, Knopf
   **Für 30 Minuten erlauben**. Klicken und bestätigen; die Karte zählt die Restzeit.
2. Dem Agenten im Chat Aufgaben geben, eine nach der anderen:
   - „Mach ein Bildschirmfoto und sag mir, was du siehst.“
   - „Öffne das Startmenü und starte den Editor (Notepad).“ (Klick, Taste)
   - „Schreib in den Editor: Test 1 2 3, dann eine neue Zeile und äöü.“ (Tippen, Zeilenumbruch)
   - „Drück Strg+A und dann Entf.“ (Tastenkombination)
   - „Scroll im Browserfenster nach unten.“ (Mausrad)
3. Gegenprobe: **Jetzt beenden** klicken, den Agenten erneut um ein Bildschirmfoto bitten. Erwartet: abgelehnt
   („Computer use is off …“).
4. Gegenprobe Administrator: den Task-Manager öffnen (läuft erhöht) und den Agenten dort klicken lassen. Erwartet: eine
   klare Fehlermeldung „… runs as administrator does not take input from FLE Link“, kein Absturz.

Was auffallen könnte: Klicks daneben (Skalierung), nur ein Teil des Bildschirms im Bild, ein zweiter Bildschirm fehlt
(gewollt: nur der Hauptbildschirm), Sonderzeichen fehlen.

## 4. Selbst-Update der App

Der Testbau ist 0.1.14 wie das neueste Release. `FLE_UPDATE_FORCE=1` lässt die App das Release trotzdem anbieten. Ziel des
Updates ist das **offizielle** 0.1.14, also ein Bau ohne die Windows-Teile: daran sieht man hinterher, dass wirklich
ersetzt wurde.

1. Die App ganz schließen (Task-Manager: kein flelink-app.exe mehr).
2. In PowerShell:

```powershell
$env:FLE_UPDATE_FORCE = '1'
& "$env:LOCALAPPDATA\FLE Link\flelink-app.exe"
```

3. **Einstellungen → Updates → Jetzt prüfen.** Erwartet: 0.1.14 wird angeboten, Knopf **Jetzt aktualisieren** (kein Text
   „installieren Sie die neue Version von …“).
4. **Jetzt aktualisieren** klicken. Beobachten, in dieser Reihenfolge:
   - „Downloading the new version …“, dann „Installing …“, dann „FLE Link starts again …“ (Setup 5,3 MB).
   - Die App schließt sich nach etwa 1,5 Sekunden; Engine und Agent werden vorher gestoppt.
   - Etwa 2 Sekunden später erscheint das Setup nur mit Fortschrittsbalken (keine Fragen), schließt sich selbst.
   - Die App startet von allein wieder.
5. Prüfen:

```powershell
Get-Content "$env:LOCALAPPDATA\FLE Link\flelink-update-result.json"
Get-Item "$env:LOCALAPPDATA\FLE Link\flelink-app.exe" | Select-Object LastWriteTime
```

   - Die Datei zeigt `"status":"done"`, `"tag":"v0.1.14"` und `"setup"` mit dem Pfad
     `%TEMP%\flelink-update-<Nummer>\FLE.Link_0.1.14_x64-setup.exe`.
   - `LastWriteTime` der App ist jetzt (vom offiziellen Setup), nicht die Zeit von Schritt 2.
   - Seite **Agent → Computer-Bedienung** zeigt wieder „Computer use is not available on Windows yet.“ Das ist richtig:
     jetzt läuft das offizielle 0.1.14.

Wenn die App nach dem Schließen **nicht** wiederkommt: prüfen, ob das Setup im Ordner aus `"setup"` liegt, und es von Hand
starten (`& "<Pfad>" /P /UPDATE /R`). Dann ist der Start aus der App heraus das Problem (Job-Objekt oder `cmd`), nicht
Download oder Signatur.

Fehlerfall zum Ansehen (optional): ohne Netz auf **Jetzt aktualisieren** klicken. Erwartet: „The download of … failed“,
„The old version keeps running“, die App läuft weiter.

## 5. Wo die Logs liegen

| Was | Wo |
|---|---|
| Datenordner (Einstellungen, Update-Stand) | `%LOCALAPPDATA%\FLE Link` |
| Ergebnis des letzten Updates | `%LOCALAPPDATA%\FLE Link\flelink-update-result.json` |
| Letzte Update-Prüfung | `%LOCALAPPDATA%\FLE Link\flelink-update-check.json` |
| Engine- und Agent-Logs | `%LOCALAPPDATA%\FLE Link\logs\` |
| Heruntergeladenes Setup | `%TEMP%\flelink-update-<Nummer>\` |
| Bildschirmfoto der Probe | `%TEMP%\probe.png` |

Die App selbst schreibt keine Konsole (Fensterprogramm). Für Ausgaben das Studio im Terminal starten: `flelink.exe studio`
aus dem Testbau zeigt Fehler der Computer-Bedienung direkt; das Update im Terminal (`flelink update`) ersetzt nur die
Programmdatei, nicht die App.

## 6. Was danach passiert

Ergebnisse in `reports/Offene Punkte.md` (Punkt 11) eintragen. Erst wenn beides unter Windows echt funktioniert hat, kommt es
in eine Version; bis dahin bleibt PR #90 ein Entwurf und wird nicht gemergt.
