# kitr icon

`kitr.svg` is original artwork for kitr: a rounded terminal window with a cat
face, the three kitty-inspired traffic-light dots and a green prompt. It is
licensed under GPL-3.0-or-later with the rest of the project.

The numbered PNGs are rendered from the SVG at the standard hicolor sizes.
Regenerate them after editing the SVG:

```sh
for size in 16 24 32 48 64 128 256 512; do
    rsvg-convert -w "$size" -h "$size" kitr.svg -o "kitr-$size.png"
done
```

`make install` installs every available size plus the scalable SVG under
`/usr/share/icons/hicolor/`.
