# Flatpak packaging skeleton

The manifest is deliberately a skeleton so `make package` can validate it
offline. Two source entries must be generated before a real build:

1. `flatpak-cargo-generator.py rust/Cargo.lock -o packaging/flatpak/cargo-sources.json`
2. Replace the `type: dir` source with the release tarball or a git source.

`make package` checks the manifest keys, the staged install tree and the
launcher, but it does not download a runtime or cargo sources.
