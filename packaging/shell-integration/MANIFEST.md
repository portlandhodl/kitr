# Shell integration manifest

Files below are byte-for-byte copies of kitty's shell-integration scripts.
They are licensed under GPL-3.0-or-later; see `NOTICE` and the repository root
`LICENSE`. `SHA256SUMS` is verified by `scripts/check_licenses.py`.

| Path | Upstream path | Purpose |
|---|---|---|
| `bash/kitty.bash` | `shell-integration/bash/kitty.bash` | bash prompt marks, title, cwd, completions |
| `zsh/.zshenv` | `shell-integration/zsh/.zshenv` | zsh startup shim |
| `zsh/kitty.zsh` | `shell-integration/zsh/kitty.zsh` | zsh manual entry point |
| `zsh/kitty-integration` | `shell-integration/zsh/kitty-integration` | zsh integration functions |
| `fish/vendor_conf.d/kitty-shell-integration.fish` | `shell-integration/fish/vendor_conf.d/kitty-shell-integration.fish` | fish integration |

Copies were taken from the kitty checkout used by this port and are not
modified. Upstream copyright and license terms are in `NOTICE`.
