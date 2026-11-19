# lbreco — find your next 50 songs

A native Linux desktop app that turns your ListenBrainz history into a
**50-track discovery shortlist** and a playlist you can keep.

Give it your ListenBrainz username. It reads your listening history, asks
ListenBrainz's own recommendation APIs for real tracks by artists in your lane,
and — if you want — lets an AI model (any OpenAI-compatible endpoint, including
a local one) curate and explain the final picks.

Works for anyone with a ListenBrainz account. No Navidrome, no local files, no
subscription.

![status](https://img.shields.io/badge/status-beta-orange)

## Three ways to use the shortlist

- **List** — all 50 picks with album, year, reason, MusicBrainz and YouTube
  links. Tick the songs you like and add them to your playlist in one go.
- **Swipe** — one song at a time. The YouTube player loads right inside the
  card and starts playing. Swipe right (or press `→`) to keep it, swipe left
  (or press `←`) to skip. `Backspace` goes back one card.
- **Playlist** — every accepted song

## No downloads

lbreco does **not** download, rip, tag or distribute audio. It only builds a
playlist from metadata, so it stays on the right side of copyright and store
terms. Accepted songs are exported as a CSV, JSON or M3U file, or copied as
plain text:

| Export | What it is for |
| --- | --- |
| **CSV** | `Track name, Artist name, Album name` — the format TuneMyMusic, Soundiiz and most Spotify/YouTube Music importers accept. |
| **JSON** | Full metadata, MusicBrainz recording IDs, and Spotify / YouTube Music search links for each track. |
| **M3U** | YouTube Music search links, opens in VLC or a browser. |
| **Copy** | One `Artist - Title` line per song, ready to paste into a search box or importer. |

Your playlist is saved locally in `~/.config/lbreco/playlist.json` and survives
restarts.

## What you get

- **Real songs, not hallucinated ones.** The AI never invents track names: it
  only re-ranks real recording MBIDs returned by ListenBrainz. Unknown IDs are
  rejected.
- **Grounded in your history.** Top artists, genres, eras, recent listens and
  collaborative-filtering recommendations set the candidates.
- **Two curators.** With AI on, an OpenAI-compatible model picks and explains.
  With AI off, a deterministic diversity-aware curator still returns the
  requested number of picks with reasons.
- **No repeats.** Tracks with any listen timestamp from the collaborative pool
  are skipped, as are your recent listens, previously recommended tracks, hated
  recordings and explicit *do-not-recommend* entries.
- **Honest diversity.** Two tracks per artist is the target; if the pool is too
  narrow, caps relax in stages (artist → album → decade) rather than quietly
  returning fewer songs.

## Requirements

Runtime:

- Linux with GTK 4.18+, libadwaita 1.6+ and WebKitGTK 6.0.
  Arch/CachyOS: `sudo pacman -S gtk4 libadwaita webkitgtk-6.0`
  Debian/Ubuntu: `sudo apt install libwebkitgtk-6.0-4`
  Fedora: `sudo dnf install webkitgtk6.0`

  WebKitGTK powers the inline YouTube player. On Windows, the YouTube button
  opens the search in the default browser instead.

To build from source:

- Rust stable (1.85+) and Cargo.
- GTK 4, libadwaita and WebKitGTK 6.0 development headers
  (`libwebkitgtk-6.0-dev` on Debian, `webkitgtk6.0-devel` on Fedora).

## Build and run

```bash
cargo build --release
./target/release/lbreco
```

### Install for your user

```bash
./install.sh            # builds, then installs to ~/.local
./install.sh --uninstall
```

This adds lbreco to your application menu and installs the icon. The same icon
is embedded in the binary, so the title bar and About dialog show it even when
running from a build directory.

## Setup (first launch)

1. Preferences → **ListenBrainz** → your username. That's all you need.
2. Optional: your user token from <https://listenbrainz.org/settings/> (only
   required for a private profile, and it raises your rate limit).
3. Optional: enable the **AI curator** and point it at any OpenAI-compatible
   endpoint:
   - OpenAI — `https://api.openai.com/v1`, e.g. `gpt-4o-mini`
   - OpenRouter — `https://openrouter.ai/api/v1`
   - Groq — `https://api.groq.com/openai/v1`
   - DeepSeek — `https://api.deepseek.com/v1`
   - Ollama (local) — `http://localhost:11434/v1`
   - LM Studio (local) — `http://localhost:1234/v1`

   AI is optional; without a key the app still produces the full shortlist.

4. Press **Generate picks**. A full run takes roughly 30–60 seconds because it
   respects ListenBrainz's one-request-per-second limit.

## How it works

```
taste profile ──▶ candidate pool ──▶ AI curator ──▶ balanced 50 ──▶ playlist
stats, listens   raw CF + radio +    rank +         caps, history,
                 genre probes        reasons        feedback filters
```

1. **Taste profile** — top artists, recordings, releases, genres, eras and
   recent listens from `listenbrainz.org/1/stats/...` and `/user/{name}/listens`.
2. **Candidates** — up to 600 raw collaborative-filtering recommendations,
   paginated the same way ListenBrainz builds its weekly playlists, plus LB
   Radio seeded by your top artists (`easy` / `medium` / `hard`), plus genre
   probes. Every candidate is a real recording MBID.
3. **Filtering** — collaborative tracks that have ever been listened to are
   dropped (matching ListenBrainz Weekly Exploration), as are your known
   recordings, recent listens, hated feedback and do-not-recommend recordings,
   artists, releases and release groups.
4. **Ranking** — raw CF scores are tiny (~0.06), so they are normalised to the
   pool maximum before ranking; the collaborative source now leads the pool.
5. **Enrichment** — bulk metadata lookup gives title, artist, album, year, tags,
   artist MBID and release MBIDs.
6. **Curation** — the model, when enabled, picks from the candidate list only.
   Then per-artist/per-album/per-decade caps, a mode-dependent exploration quota
   (20% Easy, 40% Medium/Mixed, 65% Hard) and repeat history are applied. Caps
   widen one step at a time only if the pool would otherwise be short.
7. **Repeat guard** — picks are remembered in `~/.cache/lbreco/history.json` for
   the number of days you chose.

## Packaging

`.github/workflows/build.yml` builds:

- Debian package (`.deb`) on Debian trixie
- RPM package on Fedora
- AppImage on Debian trixie
- Flatpak bundle with the GNOME 49 runtime
- Windows `.exe` bundle with GTK DLLs via MSYS2

Push a `v*` tag to attach all of them to a GitHub release. The Flatpak manifest
is `packaging/io.github.lbreco.LBreco.yaml`.

## Privacy

- Your ListenBrainz data is fetched over HTTPS and used only to build the
  playlist.
- Your configuration is stored locally in `~/.config/lbreco/config.json` with
  `0600` permissions, and the playlist next to it.
- The AI endpoint receives a taste summary and the candidate list — nothing
  else. lbreco has no telemetry and no account of its own.

## Tests

```bash
cargo test                                               # unit tests, no network
LBRECO_LIVE_USER=<username> \
  cargo test --test live_pipeline -- --ignored --nocapture   # live end-to-end
```

## License

MIT — see [LICENSE](LICENSE).
