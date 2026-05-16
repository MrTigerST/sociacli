# sociacli

Tiny social CLI for friends. Send messages with interactive buttons, drop
game-invite links, walkie-talkie over push-to-talk, get bottom-right
notifications — all driven by a background daemon that's invisible until
something happens. Extend it with sandboxed Lua plugins.

```
┌──────────────┐    WebSocket signaling    ┌──────────────────────────┐
│  CLI (Rust)  │ ◀────────────────────────▶│ sociacli signaling host  │
│  + overlay   │                            └──────────────────────────┘
└──────┬───────┘            ▲ relays SDP / ICE + offline alerts
       │
       │  WebRTC P2P (data channel + Opus audio)
       ▼
┌──────────────┐
│  CLI (Rust)  │
│  + overlay   │
└──────────────┘
```

The signaling server is hosted for you at
`https://sociacli-server.mrtigerst.dev`. It only carries opaque signaling
(SDP/ICE) plus an offline-action queue — no message content sits on it
after delivery.

---

## Quick start

### 1. Install

| OS       | How                                                                                                          |
|----------|--------------------------------------------------------------------------------------------------------------|
| Windows  | Download `sociacli-setup-<ver>.exe` → run it. Keep the **Ctrl+Alt+Shift+F hotkey**, **PATH**, **autostart** and **kill-on-uninstall** boxes ticked. |
| macOS    | Download `sociacli-<ver>-aarch64-apple-darwin.dmg` → drag the app into Applications. Intel Macs run it through Rosetta 2. |
| Linux    | `sudo apt install ./sociacli_<ver>_amd64.deb`                                                                |

### 2. Open the prompt

```
sociacli
```

First run will:

1. Ask whether to register a new account or sign in.
2. Walk you through a tiny **setup wizard**: pick the *open* hotkey, the
   *PTT* hotkey, and your default PTT friend.
3. Install the autostart entry for the **background daemon** so
   notifications, PTT and global hotkeys keep working when the REPL is
   closed and after every login.

Inside the REPL you can type the same subcommands as on the command line,
without the `sociacli` prefix.

### 3. Add a friend, send a message

```
sociacli(alice)> friend add bob
sociacli(alice)> friend list
* [pending_out] bob — 1f3e…b22
```

Once Bob accepts (`friend accept alice`):

```
sociacli(alice)> msg bob "hey, you around?"
→ bob: Message — hey, you around?
```

Bob's bottom-right overlay pops up with the message; sound plays; his
REPL also prints `[Message] alice: ...`. Notifications stay until he
clicks `×` (configurable — see [Overlay notifications](#overlay-notifications)).

### 4. Talk

The PTT hotkey is **hold-to-talk** by default. After the setup wizard the
combo is bound and a default PTT friend is set; just hold the combo and
talk.

To change the target:

```
sociacli ptt bob              # arm: HOLD the PTT hotkey to talk with bob
sociacli ptt bob --continuous # start an always-on session immediately
sociacli ptt-stop             # end the active session
```

---

## The interactive prompt

Bare `sociacli` drops you into a REPL. Every line is parsed as a
subcommand (no `sociacli` prefix needed):

```
sociacli(alice)> friend list
sociacli(alice)> msg bob hello
sociacli(alice)> invite bob https://example.com/lobby/42 -t "Lobby open"
sociacli(alice)> settings show
sociacli(alice)> ptt-stop
sociacli(alice)> help
sociacli(alice)> exit
```

`Ctrl+C` is ignored (so it doesn't accidentally kill a long-running PTT).
`exit` / `quit` / `:q` / `Ctrl+D` leave the REPL — the **daemon keeps
running in the background**.

---

## Messages with action buttons

Attach up to four buttons to a message; the recipient clicks one and the
chosen reply comes back to you as a regular message.

```
sociacli(alice)> msg bob "lunch?" --button "Yes=in 5" --button "No=later"
```

Bob sees:

```
┌─ Message — alice ──────────────────────────┐
│ lunch?                                     │
│                                            │
│ [ Yes ]   [ No ]                         │
└────────────────────────────────────────────┘
```

Bob clicks **Yes** → alice receives `[Message] bob: Reply — in 5`.

The format is `--button "Label=reply text"` — the *label* is what Bob
sees, the *reply text* is what comes back. If you omit the `=` part the
label is used as the reply.

---

## Game invites

`invite` sends a richer card with **Accept** / **Decline** buttons and
keyboard shortcuts (`A` / `D`) while the overlay window is focused.

```
sociacli(alice)> invite bob https://example.com/lobby/42 -t "Lobby open"
```

On Bob's overlay:

- **Accept** opens the URL and sends `accepted` back to alice.
- **Decline** dismisses + sends `declined` back.
- `A` / `D` while the overlay has focus do the same.

---

## Push-to-talk

`cpal` captures your OS default microphone (pure Rust — **no ffmpeg, no
device names, no setup**), encodes Opus, ships it over a P2P WebRTC
audio track straight to the receiver. The receiver hears it with the
"walkie-talkie" 300–3400 Hz band-pass and start/end beeps; both ends are
optional.

### Hotkey mode (default)

```
sociacli ptt bob              # set bob as the PTT target
# now HOLD the PTT hotkey anywhere on the desktop to talk to bob
```

### Continuous mode

```
sociacli ptt bob --continuous # always-on
sociacli ptt-stop             # end it
```

### Tune the colour

```
sociacli settings radio-effect off   # disable the 300–3400 Hz band-pass
sociacli settings radio-beeps  off   # disable start/end sine beeps
sociacli settings show               # see all current values
```

---

## Overlay notifications

The bottom-right window is **completely hidden** when there are no
unread cards — no transparent rectangle, no taskbar entry. The OS window
is materialised the moment an event arrives and hidden again when the
last card is dismissed.

- **Close** a card with the `×` button.
- **Clear all** with the *Clear all* button or `Esc`.
- Cards stack and scroll; the newest is on top.
- Sender names show as **usernames**, not user IDs (the daemon keeps a
  friend-list cache for the lookup).

Settings:

```
sociacli settings notify-auto-dismiss on        # fade cards after N ms
sociacli settings notify-auto-dismiss-ms 4000   # … instead of staying
sociacli settings notify-show-list off          # only ever show the
                                                # most-recent card
```

(Changes take effect on next daemon restart — `settings background off`
then `on` is the shortest path.)

---

## Background daemon

Everything that needs to keep running when no REPL is open lives in a
tiny `sociacli listen` process. It's wired up automatically:

- **First `sociacli` launch** registers the daemon with the per-user
  autostart mechanism for your OS — `HKCU\Run` on Windows,
  `systemd --user` on Linux, LaunchAgent on macOS — and starts it now.
- **Every subsequent `sociacli` launch** reuses the running daemon
  (detected via the overlay IPC port).
- **The Windows installer** also installs the autostart entry and
  launches the daemon post-install, so even users who never open the
  REPL get notifications.

Master switch:

```
sociacli settings background off       # stop daemon + remove autostart
sociacli settings background on        # reinstall + restart
```

Or manage the autostart entry directly:

```
sociacli service install
sociacli service uninstall
sociacli service status
```

---

## Global hotkeys

`sociacli listen` registers OS-level hotkeys from your config:

| Action | Default              | What it does                                                  |
|--------|----------------------|---------------------------------------------------------------|
| `open` | `Ctrl+Alt+Shift+F`   | Flashes/raises the overlay window.                            |
| `ptt`  | `Ctrl+Alt+Shift+K`   | **Hold** to transmit to the default PTT friend; release stops. |

```
sociacli shortcut set open Ctrl+Alt+Shift+KeyF
sociacli shortcut set ptt  Ctrl+Alt+Shift+KeyK
sociacli shortcut list
sociacli shortcut clear ptt
```

Key combos use [`global-hotkey`](https://crates.io/crates/global-hotkey)
syntax: modifiers (`Ctrl`, `Alt`, `Shift`, `Super`) joined to a single
code key (`KeyA`–`KeyZ`, `Digit0`–`Digit9`, `F1`…`F12`, `Space`, …).

---

## Presence + Do Not Disturb

Three states friends can see:

| State    | When                                                            |
|----------|-----------------------------------------------------------------|
| online   | Your background daemon is connected to the server (default).    |
| dnd      | You explicitly set Do Not Disturb. Filters incoming events.     |
| offline  | No daemon connected (automatic — set by the server, not by you).|

```
sociacli presence                       # print current state
sociacli presence dnd                   # go DND (friends see "do not disturb")
sociacli presence invisible             # stay connected but appear offline
sociacli presence online                # back online + visible
```

**Invisible mode (sender opt-out of broadcasting that you're online):**
Friends see you as `offline` exactly as if your daemon were down. They
can't send you messages, invites, plugin actions, or PTT — the server
replies `recipient offline` to them. Flip back with `presence online`
or `presence dnd`.

**Friend-online popup (receiver opt-in):**
By default a friend going online / offline only updates your friend
list silently — no card pops. Turn it on:

```
sociacli settings notify-friend-presence on
```

Combined: a friend on `presence invisible` never triggers an online
card on your side, no matter how you've configured the receiver flag —
the server never tells you they came online in the first place.

While in DND every action kind is dropped by default — no overlay card,
no sound, no REPL line. PTT offers from friends are silently rejected
too. Pick what gets through:

```
sociacli dnd show                       # see the per-kind block map
sociacli dnd allow message              # let friends still DM you
sociacli dnd allow game-invite
sociacli dnd block ptt                  # belt-and-braces (default anyway)
sociacli dnd allow all                  # DND drops nothing (kept silent on overlay)
sociacli dnd block all                  # reset to "block everything"
```

Kinds: `message`, `game-invite`, `ptt`, `plugin`, `custom`, or `all`.

**Sender-side gating:** when you try to send to a friend who's offline
(or in DND), `msg` / `invite` / `ptt` bail with a clear error instead
of vanishing into a queue:

```
sociacli(alice)> msg mauro "hey"
error: can't reach mauro: mauro is offline
```

The REPL prompt grows a tag when you're in DND so it's obvious:

```
sociacli(alice · do not disturb)>
```

The `friend list` output marks each friend's state too:

```
[online        ] [accepted] mauro — 6abeaa8c…
[do not disturb] [accepted] zoe   — 4e1a772d…
[offline       ] [accepted] bob   — 1f3eb22e…
```

---

## Friends + permissions

```
sociacli friend add bob          # send a request
sociacli friend accept alice     # accept by username OR by id
sociacli friend remove bob
sociacli friend list

sociacli perm list
sociacli perm set bob ptt off    # block bob's incoming PTT
sociacli perm set bob message on
```

Permission changes also push a *Permission changed* card to the other
side's overlay.

---

## Lua plugins

Drop a `<plugin>/manifest.toml` + `main.lua` into the plugins folder and
invoke it locally or send it to a friend.

```
sociacli plugin install ./examples/plugins/dice
sociacli plugin list
sociacli plugin run dice --args '{"sides": 20}'
sociacli plugin send bob dice --args '{"sides": 6}'
```

Bob's overlay shows an *AUTHORIZATION REQUIRED* card the first time;
he authorises with `sociacli plugin trust alice dice` and from then on
your plugin invocations run in his sandbox automatically.

Sandbox: `mlua` with only `TABLE | STRING | MATH | UTF8` loaded — no
`io`, no `os.execute`, no raw filesystem. The SDK exposes `notify`,
`log`, `send`, `message`, `invite`, `action`, `store`, `fs` (scoped),
`http`, `hash`, `base64`, `json`, `uuid`, `now`, `sleep`.

---

## Auto-update

On REPL / daemon start sociacli polls GitHub Releases of
`MrTigerST/sociacli` at most every 24 h. Newer build → message:

```
[update] sociacli 0.2.0 available (current 0.1.7) — run `sociacli update`
```

Manual:

```
sociacli update --check         # report only
sociacli update                 # download + atomically swap the binary
sociacli auto-update off        # silence the daily nag
```

Only `v*.*.*` tagged releases are picked up. The rolling `auto`
pre-release built from every push to `main` is ignored.

---

## Settings cheatsheet

```
sociacli settings show
sociacli settings radio-effect on|off
sociacli settings radio-beeps  on|off
sociacli settings background   on|off
sociacli settings notify-auto-dismiss on|off
sociacli settings notify-auto-dismiss-ms 6000
sociacli settings notify-show-list on|off
```

---

## Building from source

```
git clone https://github.com/MrTigerST/sociacli
cd sociacli
cargo build --release -p sociacli
cargo build --release -p sociacli-overlay
# binaries at target/release/sociacli(.exe) and target/release/sociacli-overlay(.exe)
```

Requirements:

- Rust stable (`rustup install stable`).
- CMake + a C compiler (vendored libopus + Lua build via `mlua`).
- Linux: `sudo apt install libasound2-dev libopus-dev libssl-dev pkg-config build-essential cmake libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`
- macOS: `brew install opus pkg-config`

The Tauri overlay binary is optional — the CLI works fine headless if
`sociacli-overlay(.exe)` isn't alongside it; the daemon just won't
spawn a window.

### Repo layout

```
sociacli/
├── cli/                    # the `sociacli` binary
│   └── src/
│       ├── main.rs         # clap commands, REPL, daemon, IPC routing
│       ├── config.rs       # %APPDATA%\sociacli\sociacli\config\config.json
│       ├── api.rs          # HTTP register/login
│       ├── protocol.rs     # WS frame types
│       ├── signaling.rs    # WS connect + send/recv channels
│       ├── overlay_ipc.rs  # local WS fanout + IPC commands
│       ├── peers.rs        # WebRTC peer manager (data + audio)
│       ├── ptt.rs          # PTT pipeline (cpal → audiopus → WebRTC track)
│       ├── plugin.rs       # Lua sandbox + SDK
│       ├── service.rs      # cross-platform autostart install
│       └── updater.rs      # self-update from GitHub Releases
├── overlay/                # Tauri 2 overlay window
│   ├── src-tauri/          # frameless, bottom-right, hidden-until-needed
│   └── ui/                 # vanilla HTML/JS notification cards
├── examples/plugins/       # sample Lua plugins
├── installer/              # Inno Setup script + macOS dmg builder
└── .github/workflows/      # cross-platform release pipeline
```

### Release pipeline

`.github/workflows/release.yml`:

| Trigger                  | Tag / release           | Picked up by `sociacli update`? |
|--------------------------|-------------------------|---------------------------------|
| Push a `v*.*.*` tag      | `vX.Y.Z` (latest)       | **Yes**                         |
| Push to `main`           | `auto` (rolling pre)    | No (non-semver, ignored)        |
| `workflow_dispatch`      | Uses the version input  | Only if it parses as semver     |

Builds:

- **Windows x64** — Inno Setup → `sociacli-setup-<ver>.exe`
- **Linux x64**   — `cargo-deb` → `sociacli_<ver>_amd64.deb`
- **macOS arm64** — `hdiutil` → `sociacli-<ver>-aarch64-apple-darwin.dmg`

Raw archives (`sociacli-<triple>.{zip,tar.gz}`) ship alongside so
`sociacli update` can fetch them.

---

## Status

Works: friends, presence, messages (with custom buttons + replies),
game invites (with Accept/Decline + A/D shortcuts), per-action
permissions, Lua plugins with per-(friend, plugin) authorisation,
push-to-talk (hold-to-talk + continuous modes, radio EQ + beeps),
cross-platform autostart, self-update, Inno Setup installer that
adds the binary to PATH and kills all sociacli processes on
uninstall, hidden-until-needed Tauri overlay with solid cards,
scroll, clear-all, and per-card close.

Roadmap:

- Reply chaining (current implementation: a clicked button sends back
  a `message` with the reply text; no thread state).
- Hold-to-talk by polling key state instead of OS hotkey events (more
  reliable on Linux with certain WMs).
- Group invites + multi-party PTT (currently 1:1).
