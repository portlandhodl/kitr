# SPDX-License-Identifier: GPL-3.0-or-later
%global debug_package %{nil}
%global _hardened_build 1

Name:           kitr
Version:        0.1.1
Release:        1%{?dist}
Summary:        Fast GPU-accelerated terminal emulator (kitty-compatible Rust port)
License:        GPL-3.0-or-later
URL:            https://github.com/portlandhodl/kitr
Source0:        %{name}-%{version}.tar.gz
BuildRequires:  rust >= 1.89
BuildRequires:  cargo >= 1.89
BuildRequires:  pkgconfig(freetype2)
BuildRequires:  pkgconfig(harfbuzz)
BuildRequires:  pkgconfig(fontconfig)
BuildRequires:  pkgconfig(egl)
BuildRequires:  pkgconfig(gl)
BuildRequires:  pkgconfig(x11)
BuildRequires:  pkgconfig(xi)
BuildRequires:  pkgconfig(xcursor)
BuildRequires:  pkgconfig(libpng)
BuildRequires:  pkgconfig(zlib)
BuildRequires:  pkgconfig(libcrypto)
BuildRequires:  pkgconfig(xkbcommon)
BuildRequires:  pkgconfig(wayland-client)
BuildRequires:  pkgconfig(wayland-egl)
BuildRequires:  ncurses-devel
Requires:       terminfo-base
Provides:       xterm-kitty
Conflicts:      kitty

%description
kitr is a Rust port of the kitty terminal emulator. It implements the kitty
graphics protocol, Unicode placeholder images, live indexed-colour palettes,
split layouts, tabs, sessions, remote control and the common kittens on Linux.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --workspace

%install
rm -rf %{buildroot}
make install DESTDIR=%{buildroot} PREFIX=%{_prefix}

%check
cargo test --workspace --lib

%files
%license LICENSE
%doc NOTICE README.md
%{_bindir}/kitr
%{_bindir}/kitten
%{_libexecdir}/kitr/kitr
%{_libexecdir}/kitr/kitten
%{_datadir}/terminfo/x/xterm-kitty
%{_datadir}/terminfo/78/xterm-kitty
%{_datadir}/kitr/
%{_datadir}/fish/vendor_conf.d/kitty-shell-integration.fish
%{_datadir}/applications/kitr.desktop
%{_datadir}/metainfo/io.github.portlandhodl.kitr.metainfo.xml
%{_mandir}/man1/kitr.1*

%changelog
* Thu Oct 22 2026 kitr contributors <maintainers@portlandhodl.example> - 0.1.1-1
- Fix Debian dependencies and bundle the application icon

* Thu Oct 22 2026 kitr contributors <maintainers@portlandhodl.example> - 0.1.0-1
- Initial distribution package skeleton.
