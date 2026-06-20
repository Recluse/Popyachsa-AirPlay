<#
.SYNOPSIS
    Build a portable Popyachsa AirPlay distribution folder (Plan B / in-process).

.DESCRIPTION
    Collects the tray .exe (Rust), the embeddable engine uxplay-core.dll, our
    dnssd.dll shim, every GStreamer/GLib/OpenSSL runtime DLL those need (walked
    recursively from uxplay-core.dll + dnssd.dll — the MSVC tray exe loads the
    DLL at runtime so its own imports are system-only), and the GStreamer plugin
    directory. Drops the lot into .\dist\PopyachsaAirPlay\.

    The dnssd.dll shim MUST sit next to the exe (Windows resolves the System32
    Bonjour dnssd.dll before PATH otherwise) — this script places it there.

    Output can be zipped and handed to another tester — no MSYS2 / GStreamer
    install on their side, no admin / installer needed.

.NOTES
    Requires PowerShell on a host with MSYS2 UCRT64 GStreamer at C:\msys64\ucrt64
    and the uxplay-core.dll already built (cmake -DBUILD_CORE_DLL=ON; ninja).
#>
$ErrorActionPreference = 'Stop'

$ROOT       = Split-Path -Parent $PSCommandPath          # the app/ crate dir
$WS         = Split-Path -Parent $ROOT                   # the workspace root
# cargo's release output (workspace target/). Override $env:TARGET_DIR if elsewhere.
$TARGET     = if ($env:TARGET_DIR) { $env:TARGET_DIR } else { Join-Path $WS 'target\release' }
$EXE_RS     = Join-Path $TARGET 'popyachsa-airplay.exe'
$UPDATER_RS = Join-Path $TARGET 'updater.exe'
# MSYS2 UCRT64 GStreamer runtime (see BUILD.md). Override $env:UCRT64 for a non-default install.
$UCRT      = if ($env:UCRT64) { $env:UCRT64 } else { 'C:\msys64\ucrt64' }
$UCRT_BIN  = Join-Path $UCRT 'bin'
$UCRT_LIB  = Join-Path $UCRT 'lib'
# Engine DLL built from the third_party/uxplay submodule (BUILD.md). Override $env:CORE_DLL.
$CORE_DLL  = if ($env:CORE_DLL) { $env:CORE_DLL } else { Join-Path $WS 'third_party\uxplay\build\uxplay-core.dll' }
# dnssd shim from the airplay-dnssd-shim repo; place it here or set $env:DNSSD_DLL.
$DNSSD_DLL = if ($env:DNSSD_DLL) { $env:DNSSD_DLL } else { Join-Path $ROOT 'dnssd.dll' }

foreach ($f in @($EXE_RS, $UPDATER_RS, $CORE_DLL, $DNSSD_DLL)) {
    if (-not (Test-Path $f)) { throw "missing required file: $f" }
}

$DIST      = Join-Path $ROOT 'dist\PopyachsaAirPlay'
if (Test-Path $DIST) { Remove-Item -Recurse -Force $DIST }
New-Item -ItemType Directory -Path $DIST | Out-Null

Write-Host "[dist] base = $DIST"

# 1. Top-level binaries (engine DLL + shim sit NEXT TO the exe on purpose).
Copy-Item $EXE_RS     $DIST\popyachsa-airplay.exe
Copy-Item $UPDATER_RS $DIST\updater.exe
Copy-Item $CORE_DLL   $DIST\uxplay-core.dll
Copy-Item $DNSSD_DLL  $DIST\dnssd.dll
# gst-inspect-1.0.exe: the installer runs it post-install to pre-build the
# GStreamer plugin registry, so the very first launch advertises instantly
# (no on-launch 241-plugin scan). Its deps are harvested by the walk below.
$GST_INSPECT = Join-Path $UCRT_BIN 'gst-inspect-1.0.exe'
if (Test-Path $GST_INSPECT) { Copy-Item $GST_INSPECT $DIST\gst-inspect-1.0.exe }

# 2. GStreamer plugins are loaded dynamically — copy the whole plugin dir FIRST,
#    so the dep-walk below can also harvest each plugin's own dependencies.
$plugDst = Join-Path $DIST 'lib\gstreamer-1.0'
New-Item -ItemType Directory -Force -Path $plugDst | Out-Null
Copy-Item "$UCRT_LIB\gstreamer-1.0\*.dll" $plugDst
# Drop plugins irrelevant to AirPlay that only emit scary "failed to load"
# warnings (missing optional deps) on every registry build. codec2json =
# codec-metadata→JSON, unused for H.264/H.265/audio playback.
Remove-Item "$plugDst\libgstcodec2json.dll" -ErrorAction SilentlyContinue
$plugins = Get-ChildItem "$plugDst\*.dll"
Write-Host "[dist] copied $($plugins.Count) GStreamer plugins"

# 3. Walk DLL deps recursively from the MinGW artefacts AND every plugin (the
#    plugins pull in extra codec/runtime DLLs that uxplay-core.dll does not).
#    The MSVC tray exe loads uxplay-core.dll dynamically, so its own import table
#    is system-only — nothing to harvest there.
function Get-DllImports([string]$path) {
    & "$UCRT_BIN\objdump.exe" -p $path 2>$null |
        Select-String -Pattern '^\s+DLL Name:\s+(.+)$' |
        ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() }
}

$queue = New-Object System.Collections.Generic.Queue[string]
$seen  = New-Object System.Collections.Generic.HashSet[string]

$seed_imgs = @($CORE_DLL, $DNSSD_DLL) + ($plugins | ForEach-Object { $_.FullName })
if (Test-Path $GST_INSPECT) { $seed_imgs += $GST_INSPECT }
foreach ($img in $seed_imgs) {
    foreach ($d in (Get-DllImports $img)) { [void]$queue.Enqueue($d) }
}

while ($queue.Count -gt 0) {
    $dllName = $queue.Dequeue()
    if (-not $seen.Add($dllName.ToLower())) { continue }
    $src = Join-Path $UCRT_BIN $dllName
    if (-not (Test-Path $src)) { continue }   # system DLL (kernel32 etc.) — skip
    Copy-Item $src $DIST -Force
    foreach ($d in (Get-DllImports $src)) { [void]$queue.Enqueue($d) }
}
Write-Host "[dist] bundled $($seen.Count) runtime DLLs (incl. plugin deps)"

# 4. README
@"
Popyachsa AirPlay — portable Windows build (in-process engine).

Run:
    .\popyachsa-airplay.exe

A tray icon appears in the notification area. The receiver advertises
"Popyachsa AirPlay" on your network; the video window pops up (and the tray
icon turns green) when a device connects, and hides again on disconnect.
Alt+Enter / F toggle fullscreen, Esc exits it. Right-click the tray for the menu.

Files:
    popyachsa-airplay.exe    — tray app + host window (Rust)
    updater.exe              — self-update helper (downloads + verifies new builds)
    uxplay-core.dll          — embeddable AirPlay engine (patched UxPlay, GPL-3.0)
    dnssd.dll                — embedded mDNS shim (must sit next to the exe)
    *.dll                    — GStreamer + GLib + OpenSSL runtime
    lib\gstreamer-1.0\       — GStreamer plugins
    config.json              — created on first run at %APPDATA%\PopyachsaAirPlay\

Open source — see About in the tray menu for credits + license details.
"@ | Set-Content -Encoding UTF8 (Join-Path $DIST 'README.txt')

# 5. Total size + zip. Archive the FOLDER itself (not its contents) so it
#    extracts into a PopyachsaAirPlay\ folder instead of dumping loose files
#    into wherever the user unzips.
$sz = (Get-ChildItem -Recurse $DIST | Measure-Object Length -Sum).Sum / 1MB
Write-Host ('[dist] total bundle size: {0:N1} MB' -f $sz)
$zip = Join-Path (Split-Path -Parent $DIST) 'PopyachsaAirPlay.zip'
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $DIST -DestinationPath $zip
$zmb = (Get-Item $zip).Length / 1MB
Write-Host ('[zip] {0} = {1:N1} MB (extracts into a PopyachsaAirPlay\ folder)' -f $zip, $zmb)
