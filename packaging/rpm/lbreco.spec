Name:           lbreco
Version:        %{lbreco_version}
Release:        1%{?dist}
Summary:        Find your next 50 songs from ListenBrainz
License:        MIT
URL:            https://listenbrainz.org
BuildArch:      x86_64
Requires:       gtk4
Requires:       libadwaita
Requires:       webkitgtk6.0

%description
lbreco reads a ListenBrainz listening history and recommends real tracks
from artists in the listener's lane. An optional OpenAI-compatible model can
re-rank and explain the shortlist. Kept songs can be exported as CSV, JSON or
M3U for Spotify, YouTube Music, TuneMyMusic or Soundiiz.

%install
install -Dm755 %{lbreco_binary} %{buildroot}%{_bindir}/lbreco
install -Dm644 %{lbreco_desktop} %{buildroot}%{_datadir}/applications/io.github.lbreco.LBreco.desktop
install -Dm644 %{lbreco_icon} %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.lbreco.LBreco.svg
install -Dm644 %{lbreco_png64} %{buildroot}%{_datadir}/icons/hicolor/64x64/apps/io.github.lbreco.LBreco.png
install -Dm644 %{lbreco_png128} %{buildroot}%{_datadir}/icons/hicolor/128x128/apps/io.github.lbreco.LBreco.png
install -Dm644 %{lbreco_png256} %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/io.github.lbreco.LBreco.png
install -Dm644 %{lbreco_metainfo} %{buildroot}%{_datadir}/metainfo/io.github.lbreco.LBreco.metainfo.xml

%files
%{_bindir}/lbreco
%{_datadir}/applications/io.github.lbreco.LBreco.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.lbreco.LBreco.svg
%{_datadir}/icons/hicolor/64x64/apps/io.github.lbreco.LBreco.png
%{_datadir}/icons/hicolor/128x128/apps/io.github.lbreco.LBreco.png
%{_datadir}/icons/hicolor/256x256/apps/io.github.lbreco.LBreco.png
%{_datadir}/metainfo/io.github.lbreco.LBreco.metainfo.xml

%changelog
* Wed Nov 18 2026 lbreco contributors - 0.1.0-1
- Initial package.
