# sociacli overlay

Frameless, always-on-top, bottom-right notification window written in Tauri 2
+ vanilla HTML/JS. Connects to the local `sociacli listen` daemon over
`ws://127.0.0.1:8923` and renders any `Action` event as a fading card,
playing the user-chosen notification sound.

## Build prerequisites

- Rust stable
- Tauri CLI: `cargo install tauri-cli --version "^2"`
- WebView2 (preinstalled on Windows 11)

## Run

```powershell
cd overlay
cargo tauri dev
```

For a release build:

```powershell
cargo tauri build
```

Tauri will complain about missing icons on a real bundle build. For dev,
`bundle.active` is `false` in `tauri.conf.json` so icons are optional. To
ship a bundle, generate them once:

```powershell
cargo tauri icon path\to\source.png
```

## Custom notification sound

```powershell
sociacli notify-sound "C:\sounds\ping.wav"
```

The daemon broadcasts that path to the overlay on connect, and the overlay
loads it via `asset://`. To actually allow that URL scheme to read from
arbitrary disk paths in a release build, add an asset scope in
`tauri.conf.json`:

```json
"app": {
  "security": {
    "assetProtocol": {
      "enable": true,
      "scope": ["**"]
    }
  }
}
```

When no sound is set the overlay synthesizes a short beep with WebAudio so
the user still gets feedback.

## Layout

```
overlay/
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   └── src/main.rs        # window setup, bottom-right positioning
└── ui/
    ├── index.html
    ├── styles.css
    └── main.js            # WS client, card rendering, sound
```
