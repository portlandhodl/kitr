# Icon placeholder

No kitr artwork is bundled here. The desktop file references the icon name
`kitr`, so a desktop environment will look for an installed icon named
`kitr.png` or `kitr.svg` in the hicolor theme.

To package an icon, add one of these files and rerun `make install`:

```
packaging/icons/kitr.png   -> /usr/share/icons/hicolor/256x256/apps/kitr.png
packaging/icons/kitr.svg   -> /usr/share/icons/hicolor/scalable/apps/kitr.svg
```

`make install` installs either file if it exists and prints a warning when no
icon is present. Do not copy kitty's logo under the kitr name without deciding
whether the trademark and attribution are appropriate.
