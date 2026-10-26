# AUR publishing

`tiny-poe2smoother-bin` installs the Linux tarball from the GitHub release without compiling.
`packaging/aur-bin/` holds the same `PKGBUILD` and `.SRCINFO` that are on the AUR.

Publish the GitHub release for the tag first and wait for its Linux tarball. The checksums
come from that asset, so they can't be filled in before the release exists.

Use an AUR account with a registered SSH key. From the project root, create a checkout if
one has not already been prepared:

```sh
git clone ssh://aur@aur.archlinux.org/tiny-poe2smoother-bin.git build/aur-bin
```

For each release, set `pkgver` to the new version and `pkgrel=1` in
`packaging/aur-bin/PKGBUILD`, then:

```sh
git -C build/aur-bin pull
cp packaging/aur-bin/PKGBUILD build/aur-bin/
cd build/aur-bin
updpkgsums
makepkg --printsrcinfo > .SRCINFO
makepkg --verifysource --force
git add PKGBUILD .SRCINFO
git commit -m "Update to 0.7.0"
git push origin master
cp PKGBUILD .SRCINFO ../../packaging/aur-bin/
```

`updpkgsums` is provided by `pacman-contrib`. Commit the updated `packaging/aur-bin/` files
in this repo too, so they keep matching the AUR.
