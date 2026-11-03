# FLE Link installer for Windows (PowerShell 5.1+).
#   irm https://flelink.run/install.ps1 | iex
# Downloads ONE file, checks its SHA-256, installs to %LOCALAPPDATA%\FLELink\bin and adds it to your user PATH.
# Then offers the desktop app (the Studio in its own window): its setup, checked against the release's SHA256SUMS.
# No administrator rights, no Python, no git.  With a variable: $env:FLE_APP = 'yes' or 'no' answers the question.
param([string]$BinDir = $(if ($env:FLE_BIN_DIR) { $env:FLE_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'FLELink\bin' }),
      [string]$Repo = $(if ($env:FLE_REPO) { $env:FLE_REPO } else { 'sirfyyn/flelink' }), [string]$Base = $env:FLE_URL_BASE)
$ErrorActionPreference = 'Stop'
# a small text file from GitHub; Windows PowerShell 5.1 hands binary content types over as bytes
function Get-Text([string]$Url) {
  $c = (Invoke-WebRequest $Url -UseBasicParsing).Content
  if ($c -is [byte[]]) { $c = [Text.Encoding]::ASCII.GetString($c) }
  return [string]$c
}
if (-not $Base) {
  # the newest release from the redirect of /releases/latest (no API: its hourly limit runs out on shared networks)
  $tag = $null
  try {
    $r = Invoke-WebRequest "https://github.com/$Repo/releases/latest" -Method Head -UseBasicParsing
    $u = if ($r.BaseResponse.ResponseUri) { $r.BaseResponse.ResponseUri.AbsoluteUri } else { $r.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
    if ($u -match '/releases/tag/([^/?#]+)') { $tag = $Matches[1] }
  } catch { }
  if (-not $tag) {
    $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
    if ($rel) { $tag = $rel.tag_name }
  }
  if (-not $tag) { throw "no release found in $Repo yet" }
  $Base = "https://github.com/$Repo/releases/download/$tag"
}
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw "no build for $($env:PROCESSOR_ARCHITECTURE) yet (Windows x64 only)" }
$file = 'flelink-x86_64-pc-windows-msvc.zip'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("flelink-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Write-Host "==> Downloading $file"
  Invoke-WebRequest "$Base/$file" -OutFile "$tmp\$file"
  $want = ((Get-Text "$Base/$file.sha256").Trim() -split '\s+')[0].ToLower()
  $got = (Get-FileHash "$tmp\$file" -Algorithm SHA256).Hash.ToLower()
  if ($want -ne $got) { throw "checksum mismatch (wanted $want, got $got); nothing was installed" }
  Write-Host "==> Checksum ok"
  Expand-Archive "$tmp\$file" -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
  Copy-Item "$tmp\flelink.exe" "$BinDir\flelink.exe" -Force
  Write-Host "==> Installed $BinDir\flelink.exe"
  if (-not (Test-Path "$BinDir\fle.exe")) { Copy-Item "$tmp\flelink.exe" "$BinDir\fle.exe" -Force; Write-Host "==> Installed $BinDir\fle.exe (short name)" }
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (($userPath -split ';') -notcontains $BinDir) { [Environment]::SetEnvironmentVariable('Path', "$userPath;$BinDir", 'User'); Write-Host "==> Added $BinDir to your PATH (open a new terminal)" }
  # the desktop app: the same Studio in a window
  $app = $env:FLE_APP
  if (-not $app) {
    $app = 'no'
    if ([Environment]::UserInteractive) {
      $a = Read-Host "`nAlso install the desktop app (the Studio in its own window)? [y/N]"
      if ($a -match '^(y|yes|j|ja)$') { $app = 'yes' }
    }
  }
  if ($app -eq 'yes') {
    $version = ($Base -split '/v')[-1]
    $setup = "FLE.Link_${version}_x64-setup.exe"
    Write-Host "==> Downloading the desktop app ($setup)"
    Invoke-WebRequest "$Base/$setup" -OutFile "$tmp\$setup"
    $sums = (Get-Text "$Base/SHA256SUMS") -split "`n"
    $line = $sums | Where-Object { $_ -match ("\s" + [regex]::Escape($setup) + "\s*$") } | Select-Object -First 1
    $want = if ($line) { ($line.Trim() -split '\s+')[0].ToLower() } else { '' }
    $got = (Get-FileHash "$tmp\$setup" -Algorithm SHA256).Hash.ToLower()
    if (-not $want -or $want -ne $got) { throw "checksum mismatch for $setup; the app was not installed (the program is)" }
    Write-Host "==> Checksum ok, starting the app's setup (unsigned: choose More info, then Run anyway)"
    Start-Process -FilePath "$tmp\$setup" -Wait
  }
  Write-Host "`nDone. Start with:  flelink   (terminal)   |   flelink studio   (browser)   |   flelink system   (what this machine can do)"
} finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
