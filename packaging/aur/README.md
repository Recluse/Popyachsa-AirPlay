# AUR — `popyachsa-airplay-bin`

`PKGBUILD` + `.SRCINFO` are ready and validated (`makepkg --printsrcinfo` parses
cleanly; the source tarball is live at `dl.airplay.popyachsa.com` and its sha256
matches the PKGBUILD). It's a binary package over the release tarball.

## Publish (under the maintainer's AUR account)

AUR push needs **your** SSH key on https://aur.archlinux.org (My Account → SSH keys).

```bash
git clone ssh://aur@aur.archlinux.org/popyachsa-airplay-bin.git
cp PKGBUILD .SRCINFO popyachsa-airplay-bin/
cd popyachsa-airplay-bin
git add PKGBUILD .SRCINFO
git commit -m "popyachsa-airplay-bin 0.2.7-1"
git push
```

The package appears at https://aur.archlinux.org/packages/popyachsa-airplay-bin .
Arch users then install with any AUR helper, e.g. `yay -S popyachsa-airplay-bin`.

## On each new release

1. Build + publish the new tarball (the `publish-linux` CI distributes
   `popyachsa-airplay-<ver>-x86_64.tar.gz`).
2. In `PKGBUILD`: bump `pkgver` (+ `pkgrel=1`) and `sha256sums` to the new tarball.
3. Regenerate: `makepkg --printsrcinfo > .SRCINFO`.
4. Commit + push to the AUR repo. `yay -Syu` then upgrades users.
