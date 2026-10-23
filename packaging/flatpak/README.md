# Flatpak packaging

`io.github.portlandhodl.kitr.yml` is a real, reproducible manifest for the
workspace in this repository. Its `cargo-sources.json` sibling is generated
from `Cargo.lock` and contains one archive per registry crate plus an inline
`cargo/config.toml` that redirects crates.io at `cargo/vendor`.

Regenerate and verify it with:

```sh
make flatpak-sources
make flatpak-sources-check
```

The manifest uses the workspace directory as a local source, so a build from a
checkout is:

```sh
flatpak-builder --user --install --force-clean build-flatpak \
  packaging/flatpak/io.github.portlandhodl.kitr.yml
```

CI generates the sources, verifies that the checked-in JSON is current, and
runs `flatpak-builder` when the runner has the required runtime and SDK. The
build is network-free after the sources are vendored.
