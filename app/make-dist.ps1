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

$ROOT       = Split-Path -Parent $PSCommandPath
$EXE_RS     = Join-Path $ROOT 'target\release\popyachsa-airplay.exe'
$UPDATER_RS = Join-Path $ROOT 'target\release\updater.exe'
$UCRT_BIN  = 'C:\msys64\ucrt64\bin'
$UCRT_LIB  = 'C:\msys64\ucrt64\lib'
$CORE_DLL  = 'C:\msys64\home\me\UxPlay\build\uxplay-core.dll'
$DNSSD_DLL = 'C:\Work\GITLAB\popyachsa-airplay\uxplay\dnssd.dll'

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

# 2b. GIO modules (the TLS/HTTPS backend) are ALSO loaded dynamically — GLib loads
#     them BY NAME out of lib\gio\modules\, never through an import table — so, like
#     the GStreamer plugins above, they are INVISIBLE to the dep-walk in step 3 and
#     must be copied explicitly. Without libgioopenssl/libgiognutls, GLib+libsoup
#     have no TLS backend and souphttpsrc fails every https:// GET. That broke
#     AirPlay *video*: YouTube/Photos/Safari hand us an HLS master whose fragments
#     live on https:// CDNs (e.g. googlevideo.com), and with no TLS the demux logs
#     "Couldn't download fragments" — the user just sees a frozen picture, no error.
#     GENERAL RULE for this bundle: anything loaded dynamically by name rather than
#     via an import table (GStreamer plugins, GIO modules, …) is invisible to the
#     walk and needs its own explicit copy. Don't add a third such directory here
#     without copying it too.
$gioDst = Join-Path $DIST 'lib\gio\modules'
New-Item -ItemType Directory -Force -Path $gioDst | Out-Null
Copy-Item "$UCRT_LIB\gio\modules\libgioopenssl.dll" $gioDst -Force
Copy-Item "$UCRT_LIB\gio\modules\libgiognutls.dll"  $gioDst -Force
$gioModules = Get-ChildItem "$gioDst\*.dll"
Write-Host "[dist] copied $($gioModules.Count) GIO modules (TLS/HTTPS backend)"

# 3. Walk DLL deps recursively from the MinGW artefacts AND every plugin (the
#    plugins pull in extra codec/runtime DLLs that uxplay-core.dll does not).
#    The MSVC tray exe loads uxplay-core.dll dynamically, so its own import table
#    is system-only — nothing to harvest there.
# $ErrorActionPreference='Stop' does NOT turn a native command's non-zero exit
# into a terminating error on Windows PowerShell 5.1, so $LASTEXITCODE has to be
# read by hand: a failed objdump reads as "this image imports nothing", and the
# walk then bundles a dist\ that is missing every DLL below that image.
function Get-DllImports([string]$path) {
    $out = & "$UCRT_BIN\objdump.exe" -p $path 2>$null
    if ($LASTEXITCODE -ne 0) { throw "objdump failed (exit $LASTEXITCODE) on $path — the dep walk would silently skip its imports" }
    $out |
        Select-String -Pattern '^\s+DLL Name:\s+(.+)$' |
        ForEach-Object { $_.Matches[0].Groups[1].Value.Trim() }
}

$queue = New-Object System.Collections.Generic.Queue[string]
$seen  = New-Object System.Collections.Generic.HashSet[string]

$seed_imgs = @($CORE_DLL, $DNSSD_DLL) + ($plugins | ForEach-Object { $_.FullName }) + ($gioModules | ForEach-Object { $_.FullName })
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

# 4. Licence. GPL-3 §4 requires the licence text to travel WITH the binaries, and
#    this tree is both the portable zip and the installer payload (the .nsi does
#    File /r over it), so putting COPYING here covers both Windows artifacts.
#    installer\LICENSE.txt is only the wizard's summary page — it is never installed.
$COPYING = Join-Path $ROOT '..\packaging\shared\COPYING'
if (-not (Test-Path $COPYING)) { throw "missing $COPYING (GPL-3 text must ship with the binaries)" }
Copy-Item $COPYING (Join-Path $DIST 'COPYING.txt')

# 5. README
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
    COPYING.txt              — GNU General Public License v3 (this program's license)
    config.json              — created on first run at %APPDATA%\PopyachsaAirPlay\

Open source — see About in the tray menu for credits + license details.
"@ | Set-Content -Encoding UTF8 (Join-Path $DIST 'README.txt')

# 6. Total size + zip. Archive the FOLDER itself (not its contents) so it
#    extracts into a PopyachsaAirPlay\ folder instead of dumping loose files
#    into wherever the user unzips.
$sz = (Get-ChildItem -Recurse $DIST | Measure-Object Length -Sum).Sum / 1MB
Write-Host ('[dist] total bundle size: {0:N1} MB' -f $sz)
$zip = Join-Path (Split-Path -Parent $DIST) 'PopyachsaAirPlay.zip'
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $DIST -DestinationPath $zip
$zmb = (Get-Item $zip).Length / 1MB
Write-Host ('[zip] {0} = {1:N1} MB (extracts into a PopyachsaAirPlay\ folder)' -f $zip, $zmb)
