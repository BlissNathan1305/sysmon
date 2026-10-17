# AUR packages

Two recipes for the Arch User Repository:

- `sysmon-bin`: installs the ready-made binary from the GitHub release (x86_64 and aarch64).
- `sysmon`: builds from the release source with cargo and runs the tests.

Both were built, installed and checked with `namcap` in an `archlinux:latest` container.

## Updating for a new release

1. Push the `vX.Y.Z` tag and wait for the release workflow to finish.
2. In both PKGBUILDs set `pkgver=X.Y.Z` and `pkgrel=1`, then fill in the checksums:
   ```sh
   updpkgsums            # from pacman-contrib, run in each recipe folder
   ```
3. Regenerate the metadata: `makepkg --printsrcinfo > .SRCINFO`.
4. Test: `makepkg -si` in each folder.

## Publishing (needs your own AUR account)

1. Create an account at <https://aur.archlinux.org> and add your SSH public key to it.
2. For each package (first time only):
   ```sh
   git clone ssh://aur@aur.archlinux.org/sysmon-bin.git aur-sysmon-bin
   ```
3. Copy `PKGBUILD` and `.SRCINFO` into that clone, then commit and push:
   ```sh
   git add PKGBUILD .SRCINFO
   git commit -m "Update to X.Y.Z"
   git push
   ```
