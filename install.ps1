# FLE Link installer for Windows (PowerShell 5.1+).
#   irm https://flelink.run/install.ps1 | iex
# Downloads ONE file, checks its SHA-256, installs to %LOCALAPPDATA%\FLELink\bin and adds it to your user PATH.
# No administrator rights, no Python, no git.
param([string]$BinDir = $(if ($env:FLE_BIN_DIR) { $env:FLE_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'FLELink\bin' }),
      [string]$Repo = $(if ($env:FLE_REPO) { $env:FLE_REPO } else { 'sirfyyn/flelink' }), [string]$Base = $env:FLE_URL_BASE)
$ErrorActionPreference = 'Stop'
if (-not $Base) {
  $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
  if (-not $rel) { throw "no release found in $Repo yet" }
  $Base = "https://github.com/$Repo/releases/download/$($rel.tag_name)"
}
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw "no build for $($env:PROCESSOR_ARCHITECTURE) yet (Windows x64 only)" }
$file = 'flelink-x86_64-pc-windows-msvc.zip'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("flelink-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "==> Downloading $file"
  Invoke-WebRequest "$Base/$file" -OutFile "$tmp\$file"
  $want = ((Invoke-WebRequest "$Base/$file.sha256").Content.Trim() -split '\s+')[0].ToLower()
  $got = (Get-FileHash "$tmp\$file" -Algorithm SHA256).Hash.ToLower()
  if ($want -ne $got) { throw "checksum mismatch (wanted $want, got $got); nothing was installed" }
  Write-Host "==> Checksum ok"
  Expand-Archive "$tmp\$file" -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
  Copy-Item "$tmp\flelink.exe" "$BinDir\flelink.exe" -Force
  Write-Host "==> Installed $BinDir\flelink.exe"
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (($userPath -split ';') -notcontains $BinDir) { [Environment]::SetEnvironmentVariable('Path', "$userPath;$BinDir", 'User'); Write-Host "==> Added $BinDir to your PATH (open a new terminal)" }
  Write-Host "`nDone. Start with:  flelink   (terminal)   |   flelink studio   (browser)   |   flelink system   (what this machine can do)"
} finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
