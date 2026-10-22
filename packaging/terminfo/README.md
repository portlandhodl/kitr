# kitr terminfo

`kitty.terminfo` is kitty's generated terminfo source, vendored unmodified from
the upstream kitty checkout. The entry name is `xterm-kitty`; kitr uses the
same name as upstream so applications get the same capabilities.

## Build and install

```sh
make terminfo
make install-terminfo PREFIX=/usr/local
```

`make terminfo` runs `tic -x -o build/terminfo packaging/terminfo/kitty.terminfo`
and then populates both the `x/` and legacy `78/` lookup directories. It also
compares the result with `../terminfo/x/xterm-kitty` when a C kitty source
checkout is present and prints whether the files are byte-identical.

## Byte identity

In this repository's checkout, a GNU ncurses `tic -x` compile of
`packaging/terminfo/kitty.terminfo` is **byte-identical** to the precompiled
C kitty files `terminfo/x/xterm-kitty` and `terminfo/78/xterm-kitty`
(SHA-256 `75a5836628e596ab1c236aeff22a298558ed50e2301248f30b8e236e8e52aabd`,
3721 bytes). `tic` versions differ across distributions and may produce
different but functionally equivalent compiled databases, so the comparison
is informative rather than guaranteed on every host.

## Fallback

If the compiled entry is not installed where the system can find it, run kitr
with `TERM=xterm-256color`. The installed `kitr` launcher wrapper detects a
missing `xterm-kitty` entry and falls back automatically; set
`KITR_TERM=xterm-256color` to force the fallback.
