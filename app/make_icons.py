"""App icons for FLE Link from the 1024 px render of site/logo.svg: Tauri PNGs, Windows .ico, macOS .icns (via iconutil)."""
import subprocess, sys, shutil
from pathlib import Path
from PIL import Image

src, out = Path(sys.argv[1]), Path(sys.argv[2])
full = Image.open(src).convert("RGBA")
# macOS: the shape is 824 of 1024, centred (Apple's icon grid); the other systems use the full square
mac = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
mac.paste(full.resize((824, 824), Image.LANCZOS), (100, 100))

def sized(img, n):
    return img.resize((n, n), Image.LANCZOS)

for name, n in {"32x32.png": 32, "64x64.png": 64, "128x128.png": 128, "128x128@2x.png": 256, "icon.png": 512,
                "Square30x30Logo.png": 30, "Square44x44Logo.png": 44, "Square71x71Logo.png": 71, "Square89x89Logo.png": 89,
                "Square107x107Logo.png": 107, "Square142x142Logo.png": 142, "Square150x150Logo.png": 150,
                "Square284x284Logo.png": 284, "Square310x310Logo.png": 310, "StoreLogo.png": 50}.items():
    sized(full, n).save(out / name)

full.save(out / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

iconset = out.parent / "icon.iconset"
shutil.rmtree(iconset, ignore_errors=True)
iconset.mkdir()
for n in (16, 32, 128, 256, 512):
    sized(mac, n).save(iconset / f"icon_{n}x{n}.png")
    sized(mac, n * 2).save(iconset / f"icon_{n}x{n}@2x.png")
subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out / "icon.icns")], check=True)
shutil.rmtree(iconset)
# macOS 26 and later draw an icon in the new format (Icon Composer) edge to edge; icns-only icons get a grey tile.
# icons/AppIcon.icon holds the cubes as a layer on a dark fill; actool (Xcode 26 or newer) compiles it into Assets.car,
# which the bundle carries (tauri.conf.json bundle.macOS.files) with CFBundleIconName in Info.plist. The CI runners have
# no such Xcode, so Assets.car is kept in the repository: run this script again on a Mac after changing the logo.
if shutil.which("xcrun"):
    car = out.parent / "icon-car"
    shutil.rmtree(car, ignore_errors=True)
    car.mkdir()
    r = subprocess.run(["xcrun", "actool", str(out / "AppIcon.icon"), "--compile", str(car), "--platform", "macosx",
                        "--minimum-deployment-target", "11.0", "--app-icon", "AppIcon",
                        "--output-partial-info-plist", str(car / "partial.plist")], capture_output=True, text=True)
    if (car / "Assets.car").is_file():
        shutil.copy(car / "Assets.car", out / "Assets.car")
        print("Assets.car written")
    else:
        print("actool could not compile AppIcon.icon (needs Xcode 26 or newer):", r.stderr[-300:])
    shutil.rmtree(car)
print("icons written to", out)
