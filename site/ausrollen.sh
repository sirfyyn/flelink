#!/bin/bash
# flelink.run ausrollen: site/ plus die beiden Installer auf den VPS (nginx-Block: FYOS-Repo infra/vps/nginx/flelink.conf).
# Aufruf aus der Wurzel des Repos:  bash site/ausrollen.sh            (Host: vps, sonst VPS=vps-notweg bash site/ausrollen.sh)
# Ohne --ja nur Trockenlauf (zeigt, was sich ändert).
set -euo pipefail
cd "$(dirname "$0")/.."
host="${VPS:-vps}"
stage="$(mktemp -d)"
trap 'rm -rf -- "$stage"' EXIT
rsync -a --exclude logo_gen.py --exclude og.svg --exclude CNAME --exclude ausrollen.sh --exclude .DS_Store site/ "$stage/"
cp install.sh install.ps1 "$stage/"
if [ "${1:-}" != "--ja" ]; then
  rsync -a --delete --dry-run --itemize-changes --rsync-path="sudo rsync" "$stage/" "$host:/var/www/flelink/"
  echo "Trockenlauf. Ausrollen mit: bash site/ausrollen.sh --ja"
  exit 0
fi
ssh "$host" 'sudo cp -a /var/www/flelink /var/www/flelink.vor-$(date +%F-%H%M)'
rsync -a --delete --rsync-path="sudo rsync" "$stage/" "$host:/var/www/flelink/"
ssh "$host" 'sudo chown -R root:root /var/www/flelink'
for u in / /impressum.html /datenschutz.html /install.sh; do
  curl -s -o /dev/null -w "%{http_code} $u\n" "https://flelink.run$u"
done
