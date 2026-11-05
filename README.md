# MagicLyrics

A small floating lyrics overlay for whatever is playing — Spotify, Apple Music, YouTube / YouTube Music in a browser — on Windows and macOS.

- Synced lyrics that follow the song, minimal translucent look
- Always on top, **never takes focus**, **clicks pass through** to the app underneath
- One small handle (top-right `···`) is the only clickable spot; click it to open the controls
- Controls: move (drag the title), resize (corner grip), toggle album art / play-pause, text size, re-search lyrics, quit
- Remembers window position, size and toggles; caches lyrics on disk so known songs work offline

## How it works

| Piece | Choice | Why |
|---|---|---|
| App shell | **Tauri 2** (Rust + HTML/CSS) | ~10 MB installer, cross-platform, per-feature permission system |
| Now playing (macOS) | System **Now Playing** info via [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter) (BSD-3, bundled) | Same data as Control Center, for any app. Since macOS 15.4 only Apple-signed processes may read it, so the adapter runs inside `/usr/bin/perl`. Unofficial: a macOS update could break it. |
| Now playing (Windows) | Windows **System Media Transport Controls** | Same data as the volume flyout. No Spotify login, no API key, no network. Works for any player that reports to Windows. |
| Browser media (YouTube, YouTube Music…) | Song detector in `src-tauri/src/media/browser.rs` | Browsers report the video title and channel name. Non-songs (vlogs, podcasts, trailers, streams, very short/long videos) are skipped; for songs, artist and title are recovered from `Artist - Song (Official Video)`, `Artist【Song】MV`, `Artist - Topic` / `…VEVO` channels. Bilingual titles (`告白氣球 Love Confession`) are also searched as Chinese-only and English-only. |
| Lyrics (Chinese) | **Mojim 魔鏡歌詞網** (Taiwan) → **LRCLIB** | Simplified results are converted to Traditional |
| Lyrics (other) | **LRCLIB** → **lyrics.ovh** | LRCLIB has the best free time-synced data |
| Cache | `%LOCALAPPDATA%\app.magiclyrics.overlay\lyrics\` | Hits kept forever; misses retried after 24 h |
| Settings | `%APPDATA%\app.magiclyrics.overlay\settings.json` | |

Time-synced lyrics are preferred over plain text from any source. Plain lyrics scroll proportionally through the song.

Not used, on purpose:
- **Spotify's own lyrics**: needs your Spotify session cookie (a login secret) and is against Spotify's terms.
- **KKBOX**: bot-protection challenge; not bypassed.
- **NetEase**: search responses are now encrypted.

> As of 2026-11, `mojim.com`'s public DNS points at a private address, so Mojim is unreachable and Chinese songs fall back to LRCLIB. The Mojim parser is in place and will be used automatically if the site comes back.

### Least privilege

- Installs per-user (no admin).
- The UI can only call MagicLyrics' own 8 commands plus window drag/resize (`src-tauri/capabilities/default.json`); no filesystem, shell or HTTP access from the web layer. Strict CSP.
- Click-through is done by polling the cursor position — no global mouse/keyboard hooks.
- Network: only HTTPS GETs to the lyrics sites above.

## Develop

Requirements: Node 18+, Rust (stable, MSVC), WebView2 (preinstalled on Windows 11).

```bash
npm install
npm run dev        # run with hot reload
npm run build      # release exe + NSIS installer in src-tauri/target/release/bundle/nsis/
```

Tests (`src-tauri/`):

```bash
cargo test                          # offline unit tests
cargo test -- --include-ignored     # + live lookups against LRCLIB/Mojim
```

Set `MAGICLYRICS_DEBUG=1` to log each lyrics provider's result to stderr.

## macOS build

Tauri can't build Mac apps on Windows; build on a Mac or with GitHub Actions.

- **GitHub Actions:** `.github/workflows/build-macos.yml` (Actions tab → *Build macOS* → *Run workflow*, or push a `v*` tag). Download the `.dmg` from the run's *Artifacts*.
- **On a Mac:** needs Xcode Command Line Tools, Rust (`rustup target add aarch64-apple-darwin x86_64-apple-darwin`), Node and `brew install cmake`.

  ```bash
  npm install
  bash scripts/fetch-mediaremote-adapter.sh   # builds the now-playing helper into src-tauri/macos/
  npm run dev
  npm run tauri build -- --target universal-apple-darwin
  ```

The app is ad-hoc signed, not notarized. First launch: open the app, then System Settings → Privacy & Security → *Open Anyway* (or `xattr -dr com.apple.quarantine /Applications/MagicLyrics.app`). Settings and cache live in `~/Library/Application Support/app.magiclyrics.overlay/` and `~/Library/Caches/app.magiclyrics.overlay/`.

## Extending

- **Other players / platforms**: implement `MediaSource` in `src-tauri/src/media/` (e.g. MPRIS on Linux). The Windows backend already works with any player; `preferredApp` in settings picks which one wins when several are active.
- **Other lyric sources**: add a module in `src-tauri/src/lyrics/` and put it in the order list in `lyrics/mod.rs`.
