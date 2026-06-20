# Linux packaging (drafts)

Drafts to speed up the next packaging session. **AUR** (`aur/PKGBUILD`) and
**Flatpak** (`flatpak/`) here; `.deb`/`.rpm` + signed repos come next. Shared
`.desktop` + AppStream metainfo in `shared/`.

App-id (reverse-DNS, used for the desktop file / icon / metainfo basenames, as
Flathub requires): **`com.popyachsa.AirPlay`**.

## File layout (all native packages)

The engine resolves `uxplay-core.so` *next to the running binary* first
(`engine_linux::core_so_path`), so co-locate them and expose a `/usr/bin` symlink:

```
/usr/lib/popyachsa-airplay/popyachsa-airplay      # real ELF
/usr/lib/popyachsa-airplay/uxplay-core.so         # dlopen'd next to it
/usr/bin/popyachsa-airplay -> ../lib/popyachsa-airplay/popyachsa-airplay
/usr/share/applications/com.popyachsa.AirPlay.desktop
/usr/share/icons/hicolor/256x256/apps/com.popyachsa.AirPlay.png
/usr/share/metainfo/com.popyachsa.AirPlay.metainfo.xml
```

`current_exe()` follows the symlink to the real path, so the next-to-binary lookup
finds the `.so`. **Flatpak** instead puts the `.so` in `/app/lib` (on the Flatpak
loader path) and lets the bare-name fallback resolve it.

## ✅ Log path — fixed

`engine_log_dir()` used to return `<exe-dir>/logs` — root-owned on a system
install, read-only under Flatpak — and `redirect_stdio_to_log()` was a no-op on
Linux (engine output wasn't captured at all). Now on Linux/macOS `engine_log_dir()`
returns the user-writable `config::log_dir()`
(`~/.local/share/PopyachsaAirPlay/logs`), and the redirect `dup2`s stdout/stderr
onto `engine.log` there (the dlopen'd engine shares those fds, so UxPlay/GStreamer/
dnssd output is captured too). Windows keeps the portable `<exe-dir>/logs`.
Verified at runtime: `engine.log` lands in `~/.local/share/PopyachsaAirPlay/logs`.

## TODOs marked in the drafts

- **AUR**: it's a `-bin` PKGBUILD over a release tarball that isn't published yet —
  fill the real `source=` URL + `sha256sums`, and ship a tarball with the layout
  above. Regenerate `.SRCINFO` (`makepkg --printsrcinfo > .SRCINFO`). Publish under
  the maintainer's AUR account.
- **Flatpak**: wire real `sources` (the UxPlay fork + `clean-full-vs-v1.73.6.diff`
  for `uxplay-core`; the app repo + vendored cargo deps / `cargo-sources.json` for
  `--offline`). Decide Flathub (public submission, reviewed) vs a self-hosted remote.
  Validate the mDNS-over-Avahi finish-arg on real hardware.

## Dependency sources

UxPlay's dep sets + GTK3/appindicator/libxdo for the tray, Avahi for mDNS, and the
system GStreamer (incl. plugins-bad + libav for H.264/H.265).
