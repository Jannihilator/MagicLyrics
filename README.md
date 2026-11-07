# MagicLyrics

https://github.com/user-attachments/assets/c2318286-071a-498f-a7c5-6db805d91491

A small floating lyrics overlay for whatever is playing — Spotify, Apple Music, YouTube / YouTube Music in a browser — on Windows and macOS.

- Synced lyrics that follow the song, minimal translucent look
- Adapts to what's behind it: dark panel over light backgrounds, light panel over dark ones (Windows; can also be set by hand)
- Always on top, **never takes focus**, **clicks pass through** to the app underneath
- One small handle (top-right `···`) is the only clickable spot; click it to open the controls
- Controls: move (drag the title), resize (corner grip), toggle album art / title / play-pause, text size, opacity, theme (auto / light / dark), color, sync lyrics, re-search lyrics, quit
- Tray icon (Windows) / menu-bar icon (macOS) to hide/show the overlay or quit
- Remembers window position, size and toggles; caches lyrics on disk so known songs work offline
- Updates itself: a dot on the handle when a new version is out, then **Update** in the controls (or the tray menu) downloads, installs and restarts

## Least privilege

- Installs per-user (no admin).
- The UI can only call MagicLyrics' own commands plus window drag/resize (`src-tauri/capabilities/default.json`); no filesystem, shell or HTTP access from the web layer. Strict CSP.
- Click-through is done by polling the cursor position — no global mouse/keyboard hooks.
- Auto light/dark (Windows): twice a second it averages the brightness of a thin strip of screen pixels just outside the overlay's edges. Only one number is kept; nothing is saved or sent. Not available on macOS.
- Network: only HTTPS GETs to the lyrics sites below, plus GitHub Releases to check for updates (at launch and every 6 hours).
- Updates are only installed if they are signed with the project's update key (public key in `src-tauri/tauri.conf.json`). The web layer can't trigger anything but the app's own check/install commands.

## Install

Download the latest build from [Releases](https://github.com/Jannihilator/MagicLyrics/releases):

- **Windows:** `MagicLyrics_x.y.z_x64-setup.exe`. Windows 10/11 (WebView2 is preinstalled on 11). The installer isn't code-signed, so SmartScreen may warn: *More info* → *Run anyway*.
- **macOS:** `MagicLyrics_x.y.z_universal.dmg` (Apple Silicon + Intel). The app is ad-hoc signed, not notarized. First launch: open the app, then System Settings → Privacy & Security → *Open Anyway* (or `xattr -dr com.apple.quarantine /Applications/MagicLyrics.app`).

### Updating

From v0.2.4 on, MagicLyrics checks for new versions by itself. When one is out, a dot appears on the `···` handle; open the controls and click **Update to vX.Y.Z** (or use *Install Update* in the tray / menu-bar menu). It downloads, checks the signature, installs and restarts. You can also click **Check for updates** any time. Versions before v0.2.4 have to be updated once by downloading from Releases.

## Is this safe?

MagicLyrics is open source, and you don't have to take anyone's word for what's in the download:

- **Built in public.** Every file on the Releases page is built by [GitHub Actions](https://github.com/Jannihilator/MagicLyrics/actions) straight from the tagged source code. Nobody uploads a file by hand, and the build logs are public.
- **Build attestations.** GitHub signs a record of the workflow and commit each file came from. With the [GitHub CLI](https://cli.github.com/) installed, check one like this:
  ```
  gh attestation verify MagicLyrics_x.y.z_x64-setup.exe --repo Jannihilator/MagicLyrics
  ```
- **Checksums.** Each release includes `SHA256SUMS.txt`. Compare it with the hash of the file you downloaded:
  - Windows: `certutil -hashfile MagicLyrics_x.y.z_x64-setup.exe SHA256`
  - macOS: `shasum -a 256 MagicLyrics_x.y.z_universal.dmg`
- **Small footprint.** See [Least privilege](#least-privilege): no admin rights, no global keyboard/mouse hooks, and network traffic only to lyrics sites.

**Why the warnings?** Code-signing certificates cost money every year, so the builds aren't signed. Windows SmartScreen and macOS Gatekeeper warn about any unsigned app, whatever is inside it. The [Install](#install) section shows how to get past the warnings.

## How it works

| Piece | Choice | Why |
|---|---|---|
| App shell | **Tauri 2** (Rust + HTML/CSS) | ~10 MB installer, cross-platform, per-feature permission system |
| Now playing (macOS) | System **Now Playing** info via [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter) (BSD-3, bundled) | Same data as Control Center, for any app. Since macOS 15.4 only Apple-signed processes may read it, so the adapter runs inside `/usr/bin/perl`. Unofficial: a macOS update could break it. |
| Now playing (Windows) | Windows **System Media Transport Controls** | Same data as the volume flyout. No Spotify login, no API key, no network. Works for any player that reports to Windows. |
| Browser media (YouTube, YouTube Music…) | Song detector in `src-tauri/src/media/browser.rs` | Browsers report the video title and channel name. Non-songs (vlogs, podcasts, trailers, streams, very short/long videos) are skipped; for songs, artist and title are recovered from `Artist - Song (Official Video)`, `Artist【Song】MV`, `Artist - Topic` / `…VEVO` channels. Bilingual titles (`告白氣球 Love Confession`) are also searched as Chinese-only and English-only. |
| Lyrics (Chinese) | **Mojim 魔鏡歌詞網** (Taiwan) → **LRCLIB** | Simplified results are converted to Traditional |
| Lyrics (other) | **LRCLIB** → **lyrics.ovh** | LRCLIB has the best free time-synced data |
| Cache | Windows `%LOCALAPPDATA%\app.magiclyrics.overlay\lyrics\` · macOS `~/Library/Caches/app.magiclyrics.overlay/` | Hits kept forever; misses retried after 24 h |
| Settings | Windows `%APPDATA%\app.magiclyrics.overlay\settings.json` · macOS `~/Library/Application Support/app.magiclyrics.overlay/` | |

Time-synced lyrics are preferred over plain text from any source. Plain lyrics scroll proportionally through the song.

> As of 2026-11, `mojim.com`'s public DNS points at a private address, so Mojim is unreachable and Chinese songs fall back to LRCLIB. The Mojim parser is in place and will be used automatically if the site comes back.

## Releasing

Push a tag like `v0.2.4` (after bumping the version in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`). The workflow builds both platforms and publishes a Release with the installers, signed update files (`*.sig`, `*.app.tar.gz`) and `latest.json`, which installed apps read to find the update.

Update signing needs a key pair (separate from Apple/Microsoft code signing). Set up once:

1. `npx tauri signer generate -w $HOME/.tauri/magiclyrics.key` (choose a password; in PowerShell use `$HOME`, since `~` is not expanded for npx).
2. Put the contents of `~/.tauri/magiclyrics.key.pub` in `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`.
3. In the repo's *Settings → Secrets and variables → Actions*, add `TAURI_SIGNING_PRIVATE_KEY` (contents of `~/.tauri/magiclyrics.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Keep a backup of the private key and password. If they're lost, installed copies can't verify new updates and everyone has to reinstall manually. Local `npm run tauri build` doesn't need the key; only CI adds the update files (`--config src-tauri/tauri.updater.conf.json`).

## Extending

- **Other players / platforms**: implement `MediaSource` in `src-tauri/src/media/` (e.g. MPRIS on Linux). The Windows backend already works with any player; when several are active it follows the one that most recently started playing.
- **Other lyric sources**: add a module in `src-tauri/src/lyrics/` and put it in the order list in `lyrics/mod.rs`.
