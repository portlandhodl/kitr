# SPDX-License-Identifier: GPL-3.0-or-later
#
# Packaging and development entry points for the kitr Cargo workspace.
# Cargo is the only build system dependency; tic and Python 3 are used by the
# optional terminfo and parity/license tooling.

CARGO ?= cargo
PYTHON ?= python3
INSTALL ?= install

PREFIX ?= /usr/local
DESTDIR ?=
BINDIR ?= $(PREFIX)/bin
LIBEXECDIR ?= $(PREFIX)/libexec/kitr
DATADIR ?= $(PREFIX)/share
MANDIR ?= $(DATADIR)/man
TERMINFODIR ?= $(DATADIR)/terminfo
APPDIR ?= $(DATADIR)/applications
METAINFODIR ?= $(DATADIR)/metainfo
ICONDIR ?= $(DATADIR)/icons/hicolor
SHELLINTEGRATIONDIR ?= $(DATADIR)/kitr/shell-integration
FISHVENDORDIR ?= $(DATADIR)/fish/vendor_conf.d

KITTY_SRC ?= ..
TERMINFO_SRC ?= packaging/terminfo/kitty.terminfo
TERMINFO_BUILD ?= build/terminfo
PARITY_THRESHOLD ?= 5.0
PARITY_ARTIFACTS ?= artifacts/screenshot-parity
PARITY_ARGS ?=
DIST ?= dist
VERSION ?= 0.1.0
PARITY_CORPUS ?= scripts/parity_corpus
ARCH ?= $(shell uname -m)

ICON_PNG := $(wildcard packaging/icons/kitr.png)
ICON_SVG := $(wildcard packaging/icons/kitr.svg)

.PHONY: all build test lint fmt clippy install uninstall terminfo \
	install-binary install-terminfo install-shell-integration \
	install-desktop install-man screenshot-parity license-check validate \
	package package-smoke flatpak-sources flatpak-sources-check deb rpm \
	parity-corpus clean help

all: build

build:
	$(CARGO) build --release --workspace

test:
	$(CARGO) test --workspace
	$(CARGO) test --workspace --all-features

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets --no-default-features -- -D warnings
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

lint: fmt clippy

terminfo:
	@command -v tic >/dev/null 2>&1 || { echo "error: tic (ncurses-bin) is required" >&2; exit 1; }
	@test -f "$(TERMINFO_SRC)" || { echo "error: missing $(TERMINFO_SRC)" >&2; exit 1; }
	rm -rf "$(TERMINFO_BUILD)/tic"
	mkdir -p "$(TERMINFO_BUILD)/tic" "$(TERMINFO_BUILD)/x" "$(TERMINFO_BUILD)/78"
	tic -x -o "$(TERMINFO_BUILD)/tic" "$(TERMINFO_SRC)"
	@f=$$(find "$(TERMINFO_BUILD)/tic" -type f -name xterm-kitty | head -n 1); \
	if [ -z "$$f" ]; then echo "error: tic did not produce xterm-kitty" >&2; exit 1; fi; \
	cp "$$f" "$(TERMINFO_BUILD)/x/xterm-kitty"; \
	cp "$$f" "$(TERMINFO_BUILD)/78/xterm-kitty"
	@rm -rf "$(TERMINFO_BUILD)/tic"
	@if [ -f "$(KITTY_SRC)/terminfo/x/xterm-kitty" ]; then \
		if cmp -s "$(TERMINFO_BUILD)/x/xterm-kitty" "$(KITTY_SRC)/terminfo/x/xterm-kitty"; then \
			echo "terminfo: byte-identical to $(KITTY_SRC)/terminfo/x/xterm-kitty"; \
		else \
			echo "terminfo: compiled entry differs from $(KITTY_SRC)/terminfo/x/xterm-kitty (tic versions can differ)"; \
		fi; \
	else \
		echo "terminfo: C kitty compiled entry not found under $(KITTY_SRC); byte comparison skipped"; \
	fi

install-binary: build
	$(INSTALL) -d "$(DESTDIR)$(BINDIR)" "$(DESTDIR)$(LIBEXECDIR)"
	$(INSTALL) -m 755 target/release/kitr "$(DESTDIR)$(LIBEXECDIR)/kitr"
	$(INSTALL) -m 755 target/release/kitten "$(DESTDIR)$(LIBEXECDIR)/kitten"
	$(INSTALL) -m 755 packaging/bin/kitr "$(DESTDIR)$(BINDIR)/kitr"
	$(INSTALL) -m 755 target/release/kitten "$(DESTDIR)$(BINDIR)/kitten"

install-terminfo: terminfo
	$(INSTALL) -d "$(DESTDIR)$(TERMINFODIR)/x" "$(DESTDIR)$(TERMINFODIR)/78"
	$(INSTALL) -m 644 "$(TERMINFO_BUILD)/x/xterm-kitty" "$(DESTDIR)$(TERMINFODIR)/x/xterm-kitty"
	$(INSTALL) -m 644 "$(TERMINFO_BUILD)/78/xterm-kitty" "$(DESTDIR)$(TERMINFODIR)/78/xterm-kitty"

install-shell-integration:
	$(INSTALL) -d \
		"$(DESTDIR)$(SHELLINTEGRATIONDIR)/bash" \
		"$(DESTDIR)$(SHELLINTEGRATIONDIR)/zsh" \
		"$(DESTDIR)$(SHELLINTEGRATIONDIR)/fish/vendor_conf.d" \
		"$(DESTDIR)$(FISHVENDORDIR)"
	$(INSTALL) -m 644 packaging/shell-integration/bash/kitty.bash "$(DESTDIR)$(SHELLINTEGRATIONDIR)/bash/kitty.bash"
	$(INSTALL) -m 644 packaging/shell-integration/zsh/.zshenv "$(DESTDIR)$(SHELLINTEGRATIONDIR)/zsh/.zshenv"
	$(INSTALL) -m 644 packaging/shell-integration/zsh/kitty.zsh "$(DESTDIR)$(SHELLINTEGRATIONDIR)/zsh/kitty.zsh"
	$(INSTALL) -m 644 packaging/shell-integration/zsh/kitty-integration "$(DESTDIR)$(SHELLINTEGRATIONDIR)/zsh/kitty-integration"
	$(INSTALL) -m 644 packaging/shell-integration/fish/vendor_conf.d/kitty-shell-integration.fish "$(DESTDIR)$(SHELLINTEGRATIONDIR)/fish/vendor_conf.d/kitty-shell-integration.fish"
	$(INSTALL) -m 644 packaging/shell-integration/NOTICE "$(DESTDIR)$(SHELLINTEGRATIONDIR)/NOTICE"
	$(INSTALL) -m 644 packaging/shell-integration/MANIFEST.md "$(DESTDIR)$(SHELLINTEGRATIONDIR)/MANIFEST.md"
	$(INSTALL) -m 644 packaging/shell-integration/SHA256SUMS "$(DESTDIR)$(SHELLINTEGRATIONDIR)/SHA256SUMS"
	$(INSTALL) -m 644 packaging/shell-integration/fish/vendor_conf.d/kitty-shell-integration.fish "$(DESTDIR)$(FISHVENDORDIR)/kitty-shell-integration.fish"

install-desktop:
	$(INSTALL) -d "$(DESTDIR)$(APPDIR)" "$(DESTDIR)$(METAINFODIR)"
	$(INSTALL) -m 644 packaging/desktop/kitr.desktop "$(DESTDIR)$(APPDIR)/kitr.desktop"
	$(INSTALL) -m 644 packaging/metainfo/io.github.portlandhodl.kitr.metainfo.xml "$(DESTDIR)$(METAINFODIR)/io.github.portlandhodl.kitr.metainfo.xml"
ifneq ($(ICON_PNG),)
	$(INSTALL) -d "$(DESTDIR)$(ICONDIR)/256x256/apps"
	$(INSTALL) -m 644 "$(ICON_PNG)" "$(DESTDIR)$(ICONDIR)/256x256/apps/kitr.png"
else
	@echo "warning: packaging/icons/kitr.png is absent; desktop icon not installed"
endif
ifneq ($(ICON_SVG),)
	$(INSTALL) -d "$(DESTDIR)$(ICONDIR)/scalable/apps"
	$(INSTALL) -m 644 "$(ICON_SVG)" "$(DESTDIR)$(ICONDIR)/scalable/apps/kitr.svg"
else
	@echo "warning: packaging/icons/kitr.svg is absent; scalable icon not installed"
endif

install-man:
	$(INSTALL) -d "$(DESTDIR)$(MANDIR)/man1"
	$(INSTALL) -m 644 packaging/man/kitr.1 "$(DESTDIR)$(MANDIR)/man1/kitr.1"

install: install-binary install-terminfo install-shell-integration install-desktop install-man
	@echo "installed kitr under $(DESTDIR)$(PREFIX)"

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/kitr" "$(DESTDIR)$(BINDIR)/kitten"
	rm -rf "$(DESTDIR)$(LIBEXECDIR)"
	rm -f "$(DESTDIR)$(TERMINFODIR)/x/xterm-kitty" "$(DESTDIR)$(TERMINFODIR)/78/xterm-kitty"
	rm -rf "$(DESTDIR)$(SHELLINTEGRATIONDIR)"
	rm -f "$(DESTDIR)$(FISHVENDORDIR)/kitty-shell-integration.fish"
	rm -f "$(DESTDIR)$(APPDIR)/kitr.desktop"
	rm -f "$(DESTDIR)$(METAINFODIR)/io.github.portlandhodl.kitr.metainfo.xml"
	rm -f "$(DESTDIR)$(MANDIR)/man1/kitr.1"
	rm -f "$(DESTDIR)$(ICONDIR)/256x256/apps/kitr.png" "$(DESTDIR)$(ICONDIR)/scalable/apps/kitr.svg"
	@echo "removed kitr files under $(DESTDIR)$(PREFIX)"

package-smoke:
	$(PYTHON) scripts/package_smoke.py "$(DIST)/stage"

package: build
	rm -rf "$(DIST)/stage" "$(DIST)/kitr-$(VERSION)-linux-$(ARCH).tar.gz"
	$(MAKE) install DESTDIR="$(abspath $(DIST)/stage)" PREFIX=/usr
	"$(abspath $(DIST)/stage)/usr/libexec/kitr/kitr" --version >/dev/null
	$(PYTHON) scripts/package_smoke.py "$(DIST)/stage"
	mkdir -p "$(DIST)"
	tar -C "$(DIST)/stage" -czf "$(DIST)/kitr-$(VERSION)-linux-$(ARCH).tar.gz" .
	@echo "package: wrote $(DIST)/kitr-$(VERSION)-linux-$(ARCH).tar.gz"

screenshot-parity: build
	$(PYTHON) scripts/screenshot_parity.py \
		--kitr target/release/kitr \
		--threshold "$(PARITY_THRESHOLD)" \
		--artifacts "$(PARITY_ARTIFACTS)" \
		$(PARITY_ARGS)

parity-corpus: build
	$(PYTHON) scripts/screenshot_parity.py \
		--corpus "$(PARITY_CORPUS)" \
		--kitr target/release/kitr \
		--threshold "$(PARITY_THRESHOLD)" \
		--artifacts "$(PARITY_ARTIFACTS)" \
		$(PARITY_ARGS)

flatpak-sources:
	$(PYTHON) scripts/generate_flatpak_cargo_sources.py \
		Cargo.lock -o packaging/flatpak/cargo-sources.json

flatpak-sources-check:
	$(PYTHON) scripts/generate_flatpak_cargo_sources.py \
		Cargo.lock -o packaging/flatpak/cargo-sources.json --check

deb:
	rm -rf "$(DIST)/deb-src"
	mkdir -p "$(DIST)/deb-src/kitr-$(VERSION)"
	tar --exclude=.git --exclude=target --exclude=dist --exclude=artifacts \
		--exclude=build --exclude=__pycache__ -cf - . \
		| tar -C "$(DIST)/deb-src/kitr-$(VERSION)" -xf -
	cp -a packaging/debian "$(DIST)/deb-src/kitr-$(VERSION)/debian"
	cd "$(DIST)/deb-src/kitr-$(VERSION)" && dpkg-buildpackage -us -uc -b -d
	@find "$(DIST)/deb-src" -maxdepth 1 -name '*.deb' -print

rpm:
	VERSION="$(VERSION)" scripts/build_rpm.sh


license-check:
	$(PYTHON) scripts/check_licenses.py

validate:
	@command -v desktop-file-validate >/dev/null 2>&1 || { echo "error: desktop-file-validate is required" >&2; exit 1; }
	@command -v appstreamcli >/dev/null 2>&1 || { echo "error: appstreamcli is required" >&2; exit 1; }
	desktop-file-validate packaging/desktop/kitr.desktop
	appstreamcli validate --no-net packaging/metainfo/io.github.portlandhodl.kitr.metainfo.xml

clean:
	$(CARGO) clean
	rm -rf "$(TERMINFO_BUILD)" "$(PARITY_ARTIFACTS)"

help:
	@printf '%s\n' \
		'Usage: make [TARGET] [PREFIX=/usr/local] [DESTDIR=/tmp/stage]' \
		'' \
		'  build                 cargo build --release --workspace' \
		'  test                  workspace tests, then all-features tests' \
		'  lint                  rustfmt check and CPU/GL clippy' \
		'  terminfo              compile xterm-kitty terminfo under build/' \
		'  install               install binary, terminfo, integration, desktop, man' \
		'  uninstall             remove installed files' \
		'  screenshot-parity     best-effort C kitty vs kitr pixel comparison' \
		'  license-check         verify SPDX headers and notices/manifests' \
		'  validate              validate desktop and AppStream metadata' \
		'  package               stage a /usr install, run smoke checks, write a tarball' \
		'  parity-corpus         run the full C-kitty vs kitr screenshot corpus' \
		'  flatpak-sources       regenerate packaging/flatpak/cargo-sources.json' \
		'  flatpak-sources-check fail if the generated Cargo sources are stale' \
		'  deb                   build a Debian binary package with dpkg-buildpackage' \
		'  rpm                   build a source tarball and RPM with rpmbuild' \
		'  clean                 cargo clean and remove generated artifacts'
