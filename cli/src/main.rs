mod api;
mod config;
mod overlay_ipc;
mod peers;
mod plugin;
mod protocol;
mod ptt;
mod service;
mod signaling;
mod updater;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use config::Config;
use protocol::{ActionKind, ClientMsg, ServerMsg};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "sociacli",
    version,
    about = "Mini social CLI over P2P",
    long_about = "Run with a subcommand for one-shot mode, or with no args to drop into an interactive `sociacli>` prompt.",
    disable_help_subcommand = false
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set or print the signaling server URL.
    Server { url: Option<String> },
    /// Create an account on the signaling server.
    Register { username: String },
    /// Log in and persist the auth token locally.
    Login { username: String },
    /// Clear local auth.
    Logout,
    /// Print local config path + current identity.
    Whoami,
    /// Friend operations.
    Friend {
        #[command(subcommand)]
        op: FriendCmd,
    },
    /// Per-friend permission flags.
    Perm {
        #[command(subcommand)]
        op: PermCmd,
    },
    /// Send a chat message to a friend. Pass `--button label=reply` once per
    /// button (max 4) to attach action buttons that the receiver can click
    /// to send the `reply` value back to you.
    Msg {
        friend: String,
        #[arg(short, long, default_value = "Message")]
        title: String,
        /// `--button "Yes=yes" --button "No=no"`. Reply text is what comes
        /// back as a `message` action from the recipient when they click.
        #[arg(short, long = "button")]
        buttons: Vec<String>,
        body: Vec<String>,
    },
    /// Send a game invite link to a friend.
    Invite {
        friend: String,
        url: String,
        #[arg(short, long, default_value = "Game invite")]
        title: String,
    },
    /// Push-to-talk. Default: sets `<friend>` as the PTT target and reminds
    /// you to HOLD the PTT hotkey to talk. Pass `--continuous` to start an
    /// immediate, always-on transmit until `ptt-stop`.
    Ptt {
        friend: String,
        #[arg(long)]
        continuous: bool,
    },
    /// End the active PTT session.
    PttStop,
    /// Set the default friend the PTT hotkey will dial when no recent target
    /// is known. `none` clears it.
    PttFriend { friend: String },
    /// Manage which microphone PTT captures from.
    Mic {
        #[command(subcommand)]
        op: MicCmd,
    },
    /// Show / set your own presence — `online` or `dnd`. Offline is implicit
    /// (no daemon = offline; friends see this automatically).
    Presence {
        /// Omit to print the current value.
        state: Option<String>,
    },
    /// Toggle per-action DND filters (what gets dropped while `presence` is
    /// `dnd`). All on by default — DND blocks everything.
    Dnd {
        #[command(subcommand)]
        op: DndCmd,
    },
    /// Pick the local notification sound (path to wav/ogg/mp3).
    NotifySound { path: PathBuf },
    /// Run the daemon: WS, WebRTC, IPC to overlay. Keep open for notifications.
    Listen,
    /// Check GitHub Releases for a newer build and replace this binary.
    Update {
        /// Only report whether an update is available; don't download.
        #[arg(long)]
        check: bool,
    },
    /// Toggle the daily background update check.
    AutoUpdate {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Lua plugin operations.
    Plugin {
        #[command(subcommand)]
        op: PluginCmd,
    },
    /// Global hotkey management (registered by the `listen` daemon).
    Shortcut {
        #[command(subcommand)]
        op: ShortcutCmd,
    },
    /// Tune client-side settings (radio EQ, beeps, …).
    Settings {
        #[command(subcommand)]
        op: SettingsCmd,
    },
    /// Manage the background `listen` daemon's autostart entry (per-user;
    /// HKCU\Run on Windows, systemd-user on Linux, LaunchAgent on macOS).
    Service {
        #[command(subcommand)]
        op: ServiceCmd,
    },
    /// Control the running background daemon (start / stop / restart /
    /// status). `restart` is what you want after editing shortcuts or
    /// notification settings.
    Daemon {
        #[command(subcommand)]
        op: DaemonCmd,
    },
    /// Print the build version + commit SHA of this binary. Handy for
    /// confirming a fresh installer actually picked up your latest changes.
    Version,
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Register the daemon to run at user login + start it now.
    Install,
    /// Remove the autostart entry + stop the running daemon.
    Uninstall,
    /// Print the current autostart state.
    Status,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start the background daemon (no-op if one is already running).
    Start,
    /// Stop the running daemon (autostart entry kept; next login restarts it).
    Stop,
    /// Stop the running daemon and start a fresh one. Use this after editing
    /// shortcuts / notification settings — the daemon re-reads config on start.
    Restart,
    /// Print whether the daemon is currently running locally.
    Status,
}

#[derive(Subcommand)]
enum DndCmd {
    /// Print the current per-action DND block map.
    Show,
    /// Block <kind> while in DND. Kinds: message, game-invite, ptt, plugin, custom, all.
    Block { kind: String },
    /// Allow <kind> through even while in DND (same kinds as `block`).
    Allow { kind: String },
}

#[derive(Subcommand)]
enum MicCmd {
    /// Print every input device cpal can see (with its ID) + which one is active.
    List,
    /// Use the device with the given ID from `mic list`.
    Set { id: usize },
    /// Fall back to the OS default input device.
    Default,
}

#[derive(Subcommand)]
enum ShortcutCmd {
    /// Show current bindings.
    List,
    /// Bind a key combo to an action. Omit `combo` to RECORD the next key
    /// press (with modifiers) interactively.
    /// Actions: `open` (focus / launch overlay), `ptt` (hold-to-talk).
    Set {
        action: String,
        combo: Option<String>,
    },
    /// Remove the binding for `action`.
    Clear { action: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// Print current settings.
    Show,
    /// Toggle the 300–3400 Hz radio band-pass on incoming PTT audio.
    RadioEffect {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Toggle the synthesized start/end PTT beeps.
    RadioBeeps {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Master switch for the background daemon. Disabling stops the running
    /// daemon, removes the autostart entry, and stops the REPL from spawning
    /// one. Re-enabling re-installs autostart + starts the daemon now.
    Background {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Auto-dismiss overlay notification cards after a delay. When off,
    /// cards stay until the user clicks the close button.
    NotifyAutoDismiss {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Milliseconds before a card auto-dismisses (when enabled).
    NotifyAutoDismissMs { ms: u32 },
    /// Show every incoming card stacked + scrollable. When off, the overlay
    /// only shows the single most recent notification.
    NotifyShowList {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Show a popup card when a friend goes online / offline. Off by default.
    NotifyFriendPresence {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
    /// Show an Accept/Decline overlay card when a new friend request arrives.
    /// On by default.
    NotifyFriendRequests {
        #[arg(value_parser = parse_bool)]
        enabled: bool,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    /// List installed plugins.
    List,
    /// Copy a plugin directory into the local plugins folder.
    Install { path: PathBuf },
    /// Run a plugin locally (calls Lua `run(args)`).
    Run {
        id: String,
        /// JSON-encoded args passed to the Lua `run` function.
        #[arg(short, long, default_value = "{}")]
        args: String,
    },
    /// Send a plugin invocation to a friend.
    Send {
        friend: String,
        id: String,
        #[arg(short, long, default_value = "{}")]
        args: String,
    },
    /// Authorize `plugin_id` invocations coming from `friend_id`.
    Trust { friend_id: String, plugin_id: String },
    /// Revoke a prior `trust` decision.
    Untrust { friend_id: String, plugin_id: String },
    /// List trusted (friend, plugin) pairs.
    Trusted,
}

#[derive(Subcommand)]
enum FriendCmd {
    Add { username: String },
    /// Accept an incoming friend request, by username.
    Accept { username: String },
    /// Remove a friend — or cancel a pending request you sent — by username.
    Remove { username: String },
    List,
    /// Filter your friend list by a case-insensitive substring of username
    /// (or full UUID match).
    Find { query: String },
}

#[derive(Subcommand)]
enum PermCmd {
    List,
    Set {
        friend_id: String,
        action: ActionKind,
        #[arg(value_parser = parse_bool)]
        allowed: bool,
    },
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "allow" => Ok(true),
        "off" | "false" | "no" | "0" | "deny" | "block" => Ok(false),
        _ => Err(format!("expected on/off, got {s}")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sociacli=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let mut cfg = Config::load()?;

    match cli.cmd {
        None => repl(&mut cfg).await?,
        Some(cmd) => dispatch(cmd, &mut cfg).await?,
    }
    Ok(())
}

async fn dispatch(cmd: Cmd, cfg: &mut Config) -> Result<()> {
    match cmd {
        Cmd::Server { url } => {
            if let Some(u) = url {
                cfg.server_url = u;
                cfg.save()?;
            }
            println!("server: {}", cfg.server_url);
        }
        Cmd::Register { username } => {
            let pw = rpassword::prompt_password("password: ")?;
            let r = api::register(&cfg.server_url, &username, &pw).await?;
            cfg.user_id = Some(r.id.clone());
            cfg.username = Some(r.username.clone());
            cfg.token = Some(r.token);
            cfg.save()?;
            println!("registered + logged in as {} ({})", r.username, r.id);
        }
        Cmd::Login { username } => {
            let pw = rpassword::prompt_password("password: ")?;
            let r = api::login(&cfg.server_url, &username, &pw).await?;
            cfg.user_id = Some(r.id.clone());
            cfg.username = Some(r.username.clone());
            cfg.token = Some(r.token);
            cfg.save()?;
            println!("logged in as {} ({})", r.username, r.id);
        }
        Cmd::Logout => {
            cfg.token = None;
            cfg.user_id = None;
            cfg.username = None;
            cfg.save()?;
            println!("logged out");
        }
        Cmd::Whoami => {
            println!("config: {}", Config::path()?.display());
            println!("server: {}", cfg.server_url);
            println!(
                "user:   {}",
                cfg.username.as_deref().unwrap_or("(not logged in)")
            );
            if let Some(s) = &cfg.notify_sound {
                println!("sound:  {}", s.display());
            }
        }
        Cmd::NotifySound { path } => {
            anyhow::ensure!(path.exists(), "no such file: {}", path.display());
            cfg.notify_sound = Some(path);
            cfg.save()?;
            println!("notification sound set");
        }
        Cmd::Friend { op } => friend_cmd(&*cfg, op).await?,
        Cmd::Perm { op } => perm_cmd(&*cfg, op).await?,
        Cmd::Msg { friend, title, buttons, body } => {
            let body = body.join(" ");
            let parsed_buttons: Vec<serde_json::Value> = buttons
                .iter()
                .take(4)
                .filter_map(|b| {
                    let (label, reply) = b.split_once('=')?;
                    Some(serde_json::json!({
                        "label": label.trim(),
                        "reply": reply.trim(),
                    }))
                })
                .collect();
            let data = if parsed_buttons.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::json!({ "buttons": parsed_buttons })
            };
            send_action(
                &*cfg,
                &friend,
                ActionKind::Message,
                &title,
                &body,
                data,
            )
            .await?;
        }
        Cmd::Invite { friend, url, title } => {
            send_action(
                &*cfg,
                &friend,
                ActionKind::GameInvite,
                &title,
                &url,
                serde_json::json!({ "url": url }),
            )
            .await?;
        }
        Cmd::Ptt { friend, continuous } => {
            if continuous {
                // Legacy "transmit forever" mode — starts a session right now.
                if daemon_is_running() {
                    ptt::play_blip(cfg.notify_sound.as_deref());
                    let reply = ipc_request(
                        serde_json::json!({"cmd": "ptt-start", "friend": friend}),
                        true,
                    )
                    .await;
                    match reply {
                        Some(v) if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) => {
                            println!(
                                "PTT live with {friend} (continuous) — `ptt-stop` to end."
                            );
                        }
                        Some(v) if v.get("error").is_some() => {
                            anyhow::bail!("{}", v["error"].as_str().unwrap_or("ptt error"));
                        }
                        _ => anyhow::bail!("daemon didn't acknowledge ptt-start"),
                    }
                } else {
                    let friend_id = resolve_friend_oneshot(&*cfg, &friend).await?;
                    ptt::play_blip(cfg.notify_sound.as_deref());
                    run_daemon(&*cfg, Some(PttPlan { friend_id })).await?;
                }
            } else {
                // Validate up front so `ptt <name>` reports the same problems
                // as `--continuous` (unknown name, not a mutual friend, offline
                // / DND) instead of silently arming a target that can never
                // connect.
                let friend_id = resolve_friend_oneshot(&*cfg, &friend).await?;
                if let Some((reason, _)) = recipient_unreachable(&friend_id, "ptt").await {
                    anyhow::bail!("can't start PTT with {friend}: {reason}");
                }
                // Default: arm the PTT hotkey to target this friend.
                cfg.default_ptt_friend = Some(friend.clone());
                cfg.save()?;
                if daemon_is_running() {
                    let _ = ipc_request(
                        serde_json::json!({"cmd": "set-ptt-friend", "friend": friend}),
                        true,
                    )
                    .await;
                }
                let combo = cfg
                    .shortcuts
                    .get("ptt")
                    .map(|s| s.as_str())
                    .unwrap_or("(no PTT hotkey bound — `shortcut set ptt <combo>`)");
                println!("PTT target: {friend}. HOLD `{combo}` to talk.");
                println!("(Pass `--continuous` to start an always-on session instead.)");
            }
        }
        Cmd::PttStop => {
            if daemon_is_running() {
                let _ = ipc_request(serde_json::json!({"cmd": "ptt-stop"}), true).await;
                println!("PTT stopped");
            } else {
                println!("no daemon — nothing to stop");
            }
        }
        Cmd::Mic { op } => mic_cmd(cfg, op)?,
        Cmd::Presence { state } => presence_cmd(cfg, state).await?,
        Cmd::Dnd { op } => dnd_cmd(cfg, op)?,
        Cmd::PttFriend { friend } => {
            if friend == "none" {
                cfg.default_ptt_friend = None;
            } else {
                cfg.default_ptt_friend = Some(friend.clone());
            }
            cfg.save()?;
            println!(
                "default PTT friend: {}",
                cfg.default_ptt_friend.as_deref().unwrap_or("(none)")
            );
        }
        Cmd::Listen => run_daemon(&*cfg, None).await?,
        Cmd::Update { check } => {
            if check {
                let res = tokio::task::spawn_blocking(updater::latest_if_newer).await??;
                match res {
                    Some(v) => println!("update available: {v} (current {})", updater::current_version()),
                    None => println!("up to date ({})", updater::current_version()),
                }
            } else {
                let status = tokio::task::spawn_blocking(updater::apply_latest).await??;
                match status {
                    self_update::Status::UpToDate(v) => println!("already on latest ({v})"),
                    self_update::Status::Updated(v) => {
                        sync_installed_version(&v);
                        println!("updated to {v}. Restart sociacli to use the new binary.")
                    }
                }
            }
        }
        Cmd::AutoUpdate { enabled } => {
            cfg.auto_update_check = enabled;
            cfg.save()?;
            println!(
                "auto-update check {}",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        Cmd::Plugin { op } => plugin_cmd(cfg, op).await?,
        Cmd::Shortcut { op } => shortcut_cmd(cfg, op)?,
        Cmd::Settings { op } => settings_cmd(cfg, op)?,
        Cmd::Service { op } => service_cmd(cfg, op)?,
        Cmd::Daemon { op } => daemon_cmd(op).await?,
        Cmd::Version => {
            println!(
                "sociacli v{} (build {})",
                env!("CARGO_PKG_VERSION"),
                option_env!("GITHUB_SHA").unwrap_or("local"),
            );
        }
    }

    Ok(())
}

async fn presence_cmd(cfg: &mut Config, state: Option<String>) -> Result<()> {
    let Some(state) = state else {
        println!("presence: {}", pretty_presence(&cfg.presence));
        return Ok(());
    };
    // Accept full words or the wire keywords; normalise to wire.
    let wire = match state.to_lowercase().as_str() {
        "online" => "online",
        "dnd" | "do-not-disturb" | "donotdisturb" | "do_not_disturb" => "dnd",
        // "invisible" = stay connected but appear offline to friends. On the
        // wire this is the same as a regular "offline" SetPresence.
        "invisible" | "offline" | "appear-offline" => "offline",
        other => anyhow::bail!(
            "presence must be `online`, `dnd`, or `invisible` (got `{other}`)."
        ),
    };
    cfg.presence = wire.to_string();
    cfg.save()?;
    if daemon_is_running() {
        let _ = ipc_request(
            serde_json::json!({"cmd": "set-presence", "state": wire}),
            true,
        )
        .await;
    }
    println!("presence → {}", pretty_presence(wire));
    Ok(())
}

fn pretty_presence(s: &str) -> &'static str {
    match s {
        "dnd" => "do not disturb",
        "offline" => "invisible (friends see you as offline)",
        _ => "online",
    }
}

fn dnd_kind_field<'a>(cfg: &'a mut Config, kind: &str) -> Result<&'a mut bool> {
    Ok(match kind {
        "message" | "msg" => &mut cfg.dnd_block_message,
        "game-invite" | "game_invite" | "invite" => &mut cfg.dnd_block_game_invite,
        "ptt" => &mut cfg.dnd_block_ptt,
        "plugin" => &mut cfg.dnd_block_plugin,
        "custom" => &mut cfg.dnd_block_custom,
        other => anyhow::bail!(
            "unknown kind `{other}` — use one of: message, game-invite, ptt, plugin, custom, all"
        ),
    })
}

fn dnd_cmd(cfg: &mut Config, op: DndCmd) -> Result<()> {
    match op {
        DndCmd::Show => {
            println!("DND filter (active when `presence == dnd`):");
            println!("  message:     {}", if cfg.dnd_block_message { "BLOCKED" } else { "allowed" });
            println!("  game_invite: {}", if cfg.dnd_block_game_invite { "BLOCKED" } else { "allowed" });
            println!("  ptt:         {}", if cfg.dnd_block_ptt { "BLOCKED" } else { "allowed" });
            println!("  plugin:      {}", if cfg.dnd_block_plugin { "BLOCKED" } else { "allowed" });
            println!("  custom:      {}", if cfg.dnd_block_custom { "BLOCKED" } else { "allowed" });
        }
        DndCmd::Block { kind } => {
            if kind == "all" {
                cfg.dnd_block_message = true;
                cfg.dnd_block_game_invite = true;
                cfg.dnd_block_ptt = true;
                cfg.dnd_block_plugin = true;
                cfg.dnd_block_custom = true;
            } else {
                *dnd_kind_field(cfg, &kind)? = true;
            }
            cfg.save()?;
            println!("DND block `{kind}` → on");
            println!("(run `daemon restart` to apply)");
        }
        DndCmd::Allow { kind } => {
            if kind == "all" {
                cfg.dnd_block_message = false;
                cfg.dnd_block_game_invite = false;
                cfg.dnd_block_ptt = false;
                cfg.dnd_block_plugin = false;
                cfg.dnd_block_custom = false;
            } else {
                *dnd_kind_field(cfg, &kind)? = false;
            }
            cfg.save()?;
            println!("DND block `{kind}` → off (still receivable while in DND)");
            println!("(run `daemon restart` to apply)");
        }
    }
    Ok(())
}

fn mic_cmd(cfg: &mut Config, op: MicCmd) -> Result<()> {
    match op {
        MicCmd::List => {
            let active = cfg.ptt_input_device.as_deref().unwrap_or("(default)");
            println!("active: {active}");
            println!("available:");
            match ptt::list_input_devices() {
                Ok(ds) if ds.is_empty() => println!("  (no input devices)"),
                Ok(ds) => {
                    for (i, d) in ds.iter().enumerate() {
                        let marker = if cfg
                            .ptt_input_device
                            .as_deref()
                            .map(|p| p == d.as_str())
                            .unwrap_or(false)
                        {
                            "*"
                        } else {
                            " "
                        };
                        println!("  {marker} [{i}] {d}");
                    }
                    println!("\nselect with: `sociacli mic set <id>`");
                }
                Err(e) => println!("  error: {e:#}"),
            }
        }
        MicCmd::Set { id } => {
            let devices = ptt::list_input_devices()?;
            let Some(name) = devices.get(id).cloned() else {
                anyhow::bail!(
                    "no input device with id {id} — `sociacli mic list` shows {} device(s)",
                    devices.len()
                );
            };
            cfg.ptt_input_device = Some(name.clone());
            cfg.save()?;
            println!("PTT mic [{id}] → {name}");
            println!("(run `daemon restart` to apply)");
        }
        MicCmd::Default => {
            cfg.ptt_input_device = None;
            cfg.save()?;
            println!("PTT mic reset to OS default");
            println!("(run `daemon restart` to apply)");
        }
    }
    Ok(())
}

/// Interactive key-combo recorder. Puts the terminal in raw mode, reads the
/// next key press, returns a string in `global-hotkey` syntax (e.g.
/// `Ctrl+Alt+Shift+KeyF`). Raw mode is always restored on exit.
fn record_combo() -> Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    println!("Press the new combo now (Esc to cancel)…");
    enable_raw_mode().context("enable raw mode")?;
    let result = (|| -> Result<String> {
        loop {
            let ev = event::read().context("read terminal event")?;
            let Event::Key(KeyEvent { code, modifiers, kind, .. }) = ev else {
                continue;
            };
            // Some terminals emit Press + Release; ignore the latter.
            if kind == KeyEventKind::Release {
                continue;
            }
            if code == KeyCode::Esc {
                anyhow::bail!("cancelled");
            }
            let main = match code {
                KeyCode::Char(' ') => Some("Space".to_string()),
                KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                    Some(format!("Key{}", c.to_ascii_uppercase()))
                }
                KeyCode::Char(c) if c.is_ascii_digit() => Some(format!("Digit{c}")),
                KeyCode::F(n) if (1..=24).contains(&n) => Some(format!("F{n}")),
                KeyCode::Tab => Some("Tab".to_string()),
                KeyCode::Backspace => Some("Backspace".to_string()),
                KeyCode::Enter => Some("Enter".to_string()),
                KeyCode::Up => Some("ArrowUp".to_string()),
                KeyCode::Down => Some("ArrowDown".to_string()),
                KeyCode::Left => Some("ArrowLeft".to_string()),
                KeyCode::Right => Some("ArrowRight".to_string()),
                _ => None,
            };
            let Some(main) = main else {
                // Pure modifier press, or a key we can't map — keep listening.
                continue;
            };
            let mut parts: Vec<&str> = Vec::new();
            if modifiers.contains(KeyModifiers::CONTROL) {
                parts.push("Ctrl");
            }
            if modifiers.contains(KeyModifiers::ALT) {
                parts.push("Alt");
            }
            if modifiers.contains(KeyModifiers::SHIFT) {
                parts.push("Shift");
            }
            if modifiers.contains(KeyModifiers::SUPER) {
                parts.push("Super");
            }
            parts.push(&main);
            return Ok(parts.join("+"));
        }
    })();
    // ALWAYS restore the terminal — even on Err.
    let _ = disable_raw_mode();
    println!();
    let combo = result?;
    println!("captured: {combo}");
    Ok(combo)
}

fn shortcut_cmd(cfg: &mut Config, op: ShortcutCmd) -> Result<()> {
    match op {
        ShortcutCmd::List => {
            if cfg.shortcuts.is_empty() {
                println!("(no shortcuts — set with `sociacli shortcut set <action> <combo>`)");
            }
            for (a, k) in &cfg.shortcuts {
                println!("{a:10} {k}");
            }
            println!(
                "\nActions: open, ptt. Examples:\n  sociacli shortcut set open Ctrl+Alt+Shift+KeyF\n  sociacli shortcut set ptt  Ctrl+Alt+KeyT"
            );
        }
        ShortcutCmd::Set { action, combo } => {
            anyhow::ensure!(
                matches!(action.as_str(), "open" | "ptt"),
                "unknown action `{action}` — use one of: open, ptt"
            );
            let combo = match combo {
                Some(c) => c,
                None => record_combo()?,
            };
            // Validate parse before saving.
            combo
                .parse::<global_hotkey::hotkey::HotKey>()
                .map_err(|e| anyhow::anyhow!("bad key combo `{combo}`: {e}"))?;
            cfg.shortcuts.insert(action.clone(), combo.clone());
            cfg.save()?;
            println!("bound {action} → {combo} (run `daemon restart` to apply)");
        }
        ShortcutCmd::Clear { action } => {
            cfg.shortcuts.remove(&action);
            cfg.save()?;
            println!("cleared {action}");
        }
    }
    Ok(())
}

fn settings_cmd(cfg: &mut Config, op: SettingsCmd) -> Result<()> {
    match op {
        SettingsCmd::Show => {
            println!("radio_effect:           {}", cfg.radio_effect);
            println!("radio_beeps:            {}", cfg.radio_beeps);
            println!("auto_update:            {}", cfg.auto_update_check);
            println!("background_enabled:     {}", cfg.background_enabled);
            println!("autostart_installed:    {}", cfg.autostart_installed);
            println!("notify_auto_dismiss:    {}", cfg.notify_auto_dismiss);
            println!("notify_auto_dismiss_ms: {}", cfg.notify_auto_dismiss_ms);
            println!("notify_show_list:       {}", cfg.notify_show_list);
            println!("notify_friend_presence: {}", cfg.notify_friend_presence);
            println!("notify_friend_requests: {}", cfg.notify_friend_requests);
        }
        SettingsCmd::RadioEffect { enabled } => {
            cfg.radio_effect = enabled;
            cfg.save()?;
            println!("radio_effect {}", if enabled { "on" } else { "off" });
        }
        SettingsCmd::RadioBeeps { enabled } => {
            cfg.radio_beeps = enabled;
            cfg.save()?;
            println!("radio_beeps {}", if enabled { "on" } else { "off" });
        }
        SettingsCmd::NotifyAutoDismiss { enabled } => {
            cfg.notify_auto_dismiss = enabled;
            cfg.save()?;
            println!("notify_auto_dismiss {}", if enabled { "on" } else { "off" });
            println!("(run `daemon restart` to apply)");
        }
        SettingsCmd::NotifyAutoDismissMs { ms } => {
            cfg.notify_auto_dismiss_ms = ms;
            cfg.save()?;
            println!("notify_auto_dismiss_ms = {ms}");
            println!("(run `daemon restart` to apply)");
        }
        SettingsCmd::NotifyShowList { enabled } => {
            cfg.notify_show_list = enabled;
            cfg.save()?;
            println!("notify_show_list {}", if enabled { "on" } else { "off" });
            println!("(run `daemon restart` to apply)");
        }
        SettingsCmd::NotifyFriendPresence { enabled } => {
            cfg.notify_friend_presence = enabled;
            cfg.save()?;
            println!(
                "notify_friend_presence {}",
                if enabled { "on" } else { "off" }
            );
            println!("(run `daemon restart` to apply)");
        }
        SettingsCmd::NotifyFriendRequests { enabled } => {
            cfg.notify_friend_requests = enabled;
            cfg.save()?;
            println!(
                "notify_friend_requests {}",
                if enabled { "on" } else { "off" }
            );
            println!("(run `daemon restart` to apply)");
        }
        SettingsCmd::Background { enabled } => {
            cfg.background_enabled = enabled;
            cfg.save()?;
            if enabled {
                match service::install() {
                    Ok(msg) => {
                        cfg.autostart_installed = true;
                        cfg.save()?;
                        println!("background on — {msg}");
                    }
                    Err(e) => println!("background on (autostart setup failed: {e:#})"),
                }
                // Make sure a daemon is actually running right now.
                ensure_daemon_running();
            } else {
                let _ = service::uninstall();
                stop_running_daemon();
                cfg.autostart_installed = false;
                cfg.save()?;
                println!("background off — autostart removed + running daemon stopped");
            }
        }
    }
    Ok(())
}

async fn daemon_cmd(op: DaemonCmd) -> Result<()> {
    match op {
        DaemonCmd::Start => {
            if daemon_is_running() {
                println!("daemon already running");
            } else {
                ensure_daemon_running();
                wait_for_daemon();
                println!("daemon started");
            }
        }
        DaemonCmd::Stop => {
            if daemon_is_running() {
                stop_running_daemon();
                println!("daemon stopped");
            } else {
                println!("no daemon running");
            }
        }
        DaemonCmd::Restart => {
            if daemon_is_running() {
                stop_running_daemon();
                // Give the OS a beat to release the IPC port + pidfile.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            ensure_daemon_running();
            wait_for_daemon();
            println!("daemon restarted");
        }
        DaemonCmd::Status => {
            println!(
                "daemon: {}",
                if daemon_is_running() { "running" } else { "not running" }
            );
        }
    }
    Ok(())
}

/// Block briefly until the freshly spawned daemon binds the IPC port —
/// makes the `started`/`restarted` message true at the time it prints.
fn wait_for_daemon() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if daemon_is_running() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn service_cmd(cfg: &mut Config, op: ServiceCmd) -> Result<()> {
    match op {
        ServiceCmd::Install => {
            let msg = service::install()?;
            cfg.autostart_installed = true;
            cfg.save()?;
            println!("{msg}");
            ensure_daemon_running();
        }
        ServiceCmd::Uninstall => {
            let msg = service::uninstall()?;
            cfg.autostart_installed = false;
            cfg.save()?;
            stop_running_daemon();
            println!("{msg}");
        }
        ServiceCmd::Status => {
            println!("{}", service::status()?);
            println!("daemon running: {}", daemon_is_running());
        }
    }
    Ok(())
}

/// Snapshot of daemon-side state the REPL queries over IPC. Updated whenever
/// the signaling server pushes a fresh `Friends` / `Perms` frame.
#[derive(Default)]
struct DaemonState {
    friends: Vec<serde_json::Value>,
    perms: Vec<serde_json::Value>,
    /// Last friend a PTT session targeted — used as default by the hotkey
    /// arm + IPC ptt-start when no friend is passed.
    last_ptt_friend: Option<String>,
    /// `Some` while a PTT transmit thread is running. Flipping the inner bool
    /// to `true` makes the encode loop exit cleanly within ~100 ms.
    ptt_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Current peer manager. Rebuilt on every WS reconnect — the IPC PTT
    /// handler reads this to dial / fetch peers.
    manager: Option<Arc<peers::PeerManager>>,
    /// Overlay IPC handle, so async tasks (e.g. a PTT dial that fails after the
    /// initial ack) can surface a card instead of failing silently.
    overlay: Option<overlay_ipc::OverlayIpc>,
    /// Preferred input device for PTT capture (substring match). `None` →
    /// fall back to cpal's host default.
    ptt_input_device: Option<String>,
    /// `"online"` | `"dnd"`. Updated via IPC `set-presence`. Filters incoming
    /// Action / Signal frames against `dnd_block_*` when set to `"dnd"`.
    presence: String,
    dnd_block_message: bool,
    dnd_block_game_invite: bool,
    dnd_block_ptt: bool,
    dnd_block_plugin: bool,
    dnd_block_custom: bool,
    notify_friend_presence: bool,
    notify_friend_requests: bool,
    /// False until the first `Friends` frame is processed. Lets us suppress
    /// "new friend request" overlay cards for requests that already existed
    /// when the daemon started (only freshly-arriving ones pop a card).
    friends_seen: bool,
    /// One-shot ack waiters keyed by sequence — the oldest gets resolved
    /// when a `ServerMsg::Error` arrives. If nothing arrives within the
    /// timeout the caller treats it as success (server stays silent on
    /// happy paths for friend_add/accept/remove/perm_update).
    pending_acks: std::collections::VecDeque<tokio::sync::oneshot::Sender<String>>,
    /// Waiters for the next `ServerMsg::Friends` frame. Drained on receipt.
    pending_friends: std::collections::VecDeque<tokio::sync::oneshot::Sender<Vec<serde_json::Value>>>,
    /// Waiters for the next `ServerMsg::Perms` frame.
    pending_perms: std::collections::VecDeque<tokio::sync::oneshot::Sender<Vec<serde_json::Value>>>,
    /// Current outbound signaling channel — replaced on every WS reconnect.
    /// IPC handlers read this rather than capturing `sig.tx` by clone, so
    /// after a reconnect they don't send into a dead channel.
    signal_tx: Option<mpsc::UnboundedSender<String>>,
}

type SharedState = Arc<tokio::sync::Mutex<DaemonState>>;

fn reply(reply_tx: &mpsc::UnboundedSender<String>, value: serde_json::Value) {
    if let Ok(s) = serde_json::to_string(&value) {
        let _ = reply_tx.send(s);
    }
}

/// Translate inbound IPC commands into outbound signaling frames + per-client
/// replies. Eliminates each CLI invocation from opening (and kicking) a WS of
/// its own with the same token.
/// Send a frame, register a one-shot ack waiter, await up to 800ms. If the
/// server returns an Error within that window, surfaces it as `Err(msg)`.
/// Silence ⇒ success.
async fn server_call_ack(
    signal_tx: &mpsc::UnboundedSender<String>,
    state: &SharedState,
    frame: serde_json::Value,
) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    state.lock().await.pending_acks.push_back(tx);
    let s = match serde_json::to_string(&frame) {
        Ok(s) => s,
        Err(_) => return Err("could not serialize request".into()),
    };
    if signal_tx.send(s).is_err() {
        // Outbound pump dropped tx_rx → WS sink is dead. Pop our waiter
        // back so it doesn't leak, then surface the failure to the caller.
        let _ = state.lock().await.pending_acks.pop_back();
        return Err("daemon not connected to server (WS closed)".into());
    }
    tracing::info!(?frame, "ipc: sent frame, awaiting ack");
    match tokio::time::timeout(std::time::Duration::from_millis(1500), rx).await {
        Ok(Ok(msg)) => {
            tracing::info!(%msg, "ipc: server returned error");
            Err(msg)
        }
        Ok(Err(_)) => {
            tracing::warn!("ipc: ack channel cancelled");
            Ok(())
        }
        Err(_) => {
            // Timeout. Pop our waiter back so a later unrelated Error
            // doesn't accidentally resolve to it.
            let _ = state.lock().await.pending_acks.pop_back();
            tracing::info!("ipc: no error within 1500ms — treating as success");
            Ok(())
        }
    }
}

/// Surface a PTT failure as an overlay card. The hotkey HOLD path has no reply
/// channel and `ptt-start` acks before ICE finishes, so without this a failed
/// dial is completely silent to the user.
async fn push_ptt_error(state: &SharedState, friend: &str, msg: &str) {
    let ov = state.lock().await.overlay.clone();
    if let Some(ov) = ov {
        ov.send(overlay_ipc::OverlayEvent::new(
            "action",
            serde_json::json!({
                "from": "",
                "from_username": "PTT",
                "action": "custom",
                "title": format!("PTT failed — {friend}"),
                "body": msg,
                "data": {},
            }),
        ));
    }
}

/// Best-effort pre-warm of the default PTT friend's WebRTC peer so a hotkey
/// HOLD transmits instantly instead of racing a cold dial. No-op unless the
/// friend is a mutual, online friend with no live peer yet.
async fn maybe_prewarm_ptt(state: &SharedState) {
    let (manager, raw) = {
        let g = state.lock().await;
        (g.manager.clone(), g.last_ptt_friend.clone())
    };
    let (Some(manager), Some(raw)) = (manager, raw) else { return };
    // Resolve username → id (accept a uuid as-is).
    let id = if uuid::Uuid::parse_str(&raw).is_ok() {
        Some(raw.clone())
    } else {
        let g = state.lock().await;
        g.friends.iter().find_map(|f| {
            let u = f.get("username").and_then(|v| v.as_str())?;
            let i = f.get("id").and_then(|v| v.as_str())?;
            if u == raw { Some(i.to_string()) } else { None }
        })
    };
    let Some(id) = id else { return };
    let ready = {
        let g = state.lock().await;
        let f = g.friends.iter().find(|f| {
            f.get("id").and_then(|v| v.as_str()) == Some(id.as_str())
        });
        let accepted =
            f.and_then(|f| f.get("status")).and_then(|v| v.as_str()) == Some("accepted");
        let online =
            f.and_then(|f| f.get("online")).and_then(|v| v.as_bool()).unwrap_or(false);
        accepted && online
    };
    if !ready || manager.peer(&id).await.is_some() {
        return;
    }
    if let Err(e) = manager.dial(&id).await {
        tracing::debug!(?e, "ptt connect-prewarm dial failed");
    }
}

async fn handle_ipc_cmd(
    signal_tx: &mpsc::UnboundedSender<String>,
    state: &SharedState,
    cmd: overlay_ipc::IpcCmd,
) {
    let kind = cmd.req.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    let req_id = cmd.req.get("id").cloned().unwrap_or(serde_json::Value::Null);
    match kind {
        "send-action" => {
            let to = cmd.req.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let action = cmd.req.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let title = cmd.req.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let body = cmd.req.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let data = cmd.req.get("data").cloned().unwrap_or(serde_json::json!({}));
            match server_call_ack(
                signal_tx,
                state,
                serde_json::json!({
                    "t": "send_action", "to": to, "action": action,
                    "title": title, "body": body, "data": data,
                }),
            )
            .await
            {
                Ok(()) => reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true})),
                Err(msg) => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": msg}),
                ),
            }
        }
        "friend-list" => {
            // Ask the server for fresh data + wait for the response. Falls
            // back to whatever's cached if the server doesn't answer in time.
            let (tx, rx) = tokio::sync::oneshot::channel();
            state.lock().await.pending_friends.push_back(tx);
            let _ = signal_tx.send(
                serde_json::to_string(&serde_json::json!({"t": "friend_list"}))
                    .unwrap_or_default(),
            );
            let snapshot = match tokio::time::timeout(
                std::time::Duration::from_millis(1500),
                rx,
            )
            .await
            {
                Ok(Ok(list)) => list,
                _ => state.lock().await.friends.clone(),
            };
            reply(
                &cmd.reply,
                serde_json::json!({"reply": req_id, "friends": snapshot}),
            );
        }
        "friend-add" => {
            let username = cmd.req.get("username").and_then(|v| v.as_str()).unwrap_or("");
            match server_call_ack(
                signal_tx,
                state,
                serde_json::json!({"t": "friend_add", "username": username}),
            )
            .await
            {
                Ok(()) => reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true})),
                Err(msg) => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": msg}),
                ),
            }
        }
        "friend-accept" => {
            let username = cmd.req.get("username").and_then(|v| v.as_str()).unwrap_or("");
            match server_call_ack(
                signal_tx,
                state,
                serde_json::json!({"t": "friend_accept", "username": username}),
            )
            .await
            {
                Ok(()) => reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true})),
                Err(msg) => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": msg}),
                ),
            }
        }
        "friend-remove" => {
            let username = cmd.req.get("username").and_then(|v| v.as_str()).unwrap_or("");
            match server_call_ack(
                signal_tx,
                state,
                serde_json::json!({"t": "friend_remove", "username": username}),
            )
            .await
            {
                Ok(()) => reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true})),
                Err(msg) => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": msg}),
                ),
            }
        }
        "perm-list" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            state.lock().await.pending_perms.push_back(tx);
            let _ = signal_tx.send(
                serde_json::to_string(&serde_json::json!({"t": "perm_list"})).unwrap_or_default(),
            );
            let snapshot = match tokio::time::timeout(
                std::time::Duration::from_millis(1500),
                rx,
            )
            .await
            {
                Ok(Ok(list)) => list,
                _ => state.lock().await.perms.clone(),
            };
            reply(
                &cmd.reply,
                serde_json::json!({"reply": req_id, "perms": snapshot}),
            );
        }
        "perm-set" => {
            let friend_id = cmd
                .req
                .get("friend_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let action = cmd.req.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let allowed = cmd.req.get("allowed").and_then(|v| v.as_bool()).unwrap_or(false);
            match server_call_ack(
                signal_tx,
                state,
                serde_json::json!({
                    "t": "perm_update",
                    "friend_id": friend_id,
                    "action": action,
                    "allowed": allowed,
                }),
            )
            .await
            {
                Ok(()) => reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true})),
                Err(msg) => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": msg}),
                ),
            }
        }
        "resolve-friend" => {
            let ident = cmd.req.get("ident").and_then(|v| v.as_str()).unwrap_or("");
            if uuid::Uuid::parse_str(ident).is_ok() {
                reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "id": ident}),
                );
                return;
            }
            let snapshot = state.lock().await.friends.clone();
            let found = snapshot.iter().find_map(|f| {
                let username = f.get("username").and_then(|v| v.as_str())?;
                let id = f.get("id").and_then(|v| v.as_str())?;
                if username == ident { Some(id.to_string()) } else { None }
            });
            match found {
                Some(id) => reply(&cmd.reply, serde_json::json!({"reply": req_id, "id": id})),
                None => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": format!("no friend named {ident}")}),
                ),
            }
        }
        "ptt-start" => {
            let friend_arg = cmd.req.get("friend").and_then(|v| v.as_str()).map(str::to_string);
            // Hotkey hold sends no `friend` → fall back to last/default PTT
            // friend (which may still be an unresolved username from config).
            let raw = match friend_arg {
                Some(f) => Some(f),
                None => state.lock().await.last_ptt_friend.clone(),
            };
            let Some(raw) = raw else {
                reply(&cmd.reply, serde_json::json!({"reply": req_id, "error": "no PTT friend (set with `ptt-friend <name>` or pass one)"}));
                return;
            };
            // Resolve username → id via cache (accept a uuid as-is). Applies to
            // both the explicit arg and the last/default-friend fallback.
            let target = if uuid::Uuid::parse_str(&raw).is_ok() {
                Some(raw.clone())
            } else {
                let snapshot = state.lock().await.friends.clone();
                snapshot.iter().find_map(|fr| {
                    let u = fr.get("username").and_then(|v| v.as_str())?;
                    let id = fr.get("id").and_then(|v| v.as_str())?;
                    if u == raw { Some(id.to_string()) } else { None }
                })
            };
            let Some(friend_id) = target else {
                reply(&cmd.reply, serde_json::json!({"reply": req_id, "error": format!("no friend named `{raw}` in your list")}));
                return;
            };
            // Sender-side gate: only dial when the recipient looks reachable.
            {
                let g = state.lock().await;
                let f = g.friends.iter().find(|f| {
                    f.get("id").and_then(|v| v.as_str()) == Some(friend_id.as_str())
                });
                let status = f.and_then(|f| f.get("status")).and_then(|v| v.as_str()).unwrap_or("");
                if status != "accepted" {
                    let uname = f.and_then(|f| f.get("username")).and_then(|v| v.as_str()).unwrap_or(&friend_id).to_string();
                    reply(&cmd.reply, serde_json::json!({"reply": req_id, "error": format!("{uname} is not a mutual friend — you must both accept first")}));
                    return;
                }
                let online = f.and_then(|f| f.get("online")).and_then(|v| v.as_bool()).unwrap_or(false);
                let presence = f.and_then(|f| f.get("presence")).and_then(|v| v.as_str()).unwrap_or("online");
                if !online {
                    let uname = f.and_then(|f| f.get("username")).and_then(|v| v.as_str()).unwrap_or(&friend_id).to_string();
                    reply(&cmd.reply, serde_json::json!({"reply": req_id, "error": format!("{uname} is offline — can't start PTT")}));
                    return;
                }
                if presence == "dnd" {
                    let uname = f.and_then(|f| f.get("username")).and_then(|v| v.as_str()).unwrap_or(&friend_id).to_string();
                    reply(&cmd.reply, serde_json::json!({"reply": req_id, "error": format!("{uname} is in Do Not Disturb")}));
                    return;
                }
            }
            let (manager, mic, already_running) = {
                let g = state.lock().await;
                if g.ptt_stop.is_some() {
                    (None, None, true)
                } else {
                    (g.manager.clone(), g.ptt_input_device.clone(), false)
                }
            };
            if already_running {
                reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true, "already_running": true}));
                return;
            }
            let Some(manager) = manager else {
                reply(&cmd.reply, serde_json::json!({"reply": req_id, "error": "daemon not connected to server yet"}));
                return;
            };
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let mut g = state.lock().await;
                g.ptt_stop = Some(stop.clone());
                g.last_ptt_friend = Some(friend_id.clone());
            }
            let state_for_task = state.clone();
            let friend_for_task = friend_id.clone();
            // Friendly label for failure cards (username if cached, else id).
            let friend_label = {
                let g = state.lock().await;
                g.friends
                    .iter()
                    .find(|f| f.get("id").and_then(|v| v.as_str()) == Some(friend_id.as_str()))
                    .and_then(|f| f.get("username").and_then(|v| v.as_str()))
                    .unwrap_or(&friend_id)
                    .to_string()
            };
            tokio::spawn(async move {
                if manager.peer(&friend_for_task).await.is_none() {
                    if let Err(e) = manager.dial(&friend_for_task).await {
                        tracing::error!(?e, "ipc ptt: dial failed");
                        push_ptt_error(&state_for_task, &friend_label, "couldn't start the call (dial failed)").await;
                        state_for_task.lock().await.ptt_stop = None;
                        return;
                    }
                }
                let Some(peer) = manager.peer(&friend_for_task).await else {
                    state_for_task.lock().await.ptt_stop = None;
                    return;
                };
                // Wait for WebRTC to reach Connected — otherwise the Opus
                // frames we'd write would be dropped on the floor before ICE
                // + DTLS finish.
                use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    // Released the key before ICE/DTLS finished — abort the
                    // dial instead of beeping + transmitting into a closed gate.
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        state_for_task.lock().await.ptt_stop = None;
                        return;
                    }
                    let st = peer.pc.connection_state();
                    if st == RTCPeerConnectionState::Connected {
                        break;
                    }
                    if matches!(
                        st,
                        RTCPeerConnectionState::Failed
                            | RTCPeerConnectionState::Closed
                            | RTCPeerConnectionState::Disconnected
                    ) {
                        tracing::error!(state = ?st, "ptt: peer connection failed before transmit");
                        push_ptt_error(&state_for_task, &friend_label, "couldn't connect — the network blocked the peer-to-peer link (NAT/firewall). A TURN relay may be required.").await;
                        state_for_task.lock().await.ptt_stop = None;
                        return;
                    }
                    if std::time::Instant::now() >= deadline {
                        tracing::error!(state = ?st, "ptt: peer connection did not reach Connected within 10s");
                        push_ptt_error(&state_for_task, &friend_label, "timed out connecting — STUN couldn't punch through. A TURN relay may be required for this friend's network.").await;
                        state_for_task.lock().await.ptt_stop = None;
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                tracing::info!(friend = %friend_for_task, "ptt: peer connected, starting transmit");
                manager.beep_start();
                let p = peer.clone();
                let s = stop.clone();
                let mic_for_task = mic.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    ptt::transmit_blocking_until(p, s, mic_for_task)
                })
                .await;
                manager.beep_end();
                state_for_task.lock().await.ptt_stop = None;
            });
            reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true, "friend_id": friend_id}));
        }
        "friend-state" => {
            let friend_id = cmd.req.get("friend_id").and_then(|v| v.as_str()).unwrap_or("");
            let snapshot = state.lock().await.friends.clone();
            let found = snapshot.iter().find(|f| {
                f.get("id").and_then(|v| v.as_str()) == Some(friend_id)
            });
            match found {
                Some(f) => reply(
                    &cmd.reply,
                    serde_json::json!({
                        "reply": req_id,
                        "username": f.get("username"),
                        "online": f.get("online"),
                        "presence": f.get("presence"),
                        "status": f.get("status"),
                    }),
                ),
                None => reply(
                    &cmd.reply,
                    serde_json::json!({"reply": req_id, "error": "friend not in cache"}),
                ),
            }
        }
        "set-presence" => {
            let new_state = cmd
                .req
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("online")
                .to_string();
            state.lock().await.presence = new_state.clone();
            // Tell the server too so friends see the change.
            let frame = serde_json::json!({"t": "set_presence", "state": new_state});
            if let Ok(s) = serde_json::to_string(&frame) {
                let _ = signal_tx.send(s);
            }
            reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true}));
        }
        "set-ptt-friend" => {
            let friend = cmd.req.get("friend").and_then(|v| v.as_str()).map(str::to_string);
            // Accept either uuid or username — resolve via cache.
            let resolved = match friend {
                Some(f) if uuid::Uuid::parse_str(&f).is_ok() => Some(f),
                Some(f) => {
                    let snapshot = state.lock().await.friends.clone();
                    snapshot.iter().find_map(|fr| {
                        let u = fr.get("username").and_then(|v| v.as_str())?;
                        let id = fr.get("id").and_then(|v| v.as_str())?;
                        if u == f { Some(id.to_string()) } else { None }
                    })
                }
                None => None,
            };
            state.lock().await.last_ptt_friend = resolved.clone();
            // Pre-warm the WebRTC peer so a subsequent hotkey HOLD transmits
            // instantly instead of racing a fresh dial (ICE/DTLS take seconds —
            // longer than a normal push-to-talk hold). Best-effort, background.
            if let Some(fid) = resolved {
                let (manager, ready) = {
                    let g = state.lock().await;
                    let f = g.friends.iter().find(|f| {
                        f.get("id").and_then(|v| v.as_str()) == Some(fid.as_str())
                    });
                    let accepted = f.and_then(|f| f.get("status")).and_then(|v| v.as_str())
                        == Some("accepted");
                    let online = f.and_then(|f| f.get("online")).and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    (g.manager.clone(), accepted && online)
                };
                if ready {
                    if let Some(manager) = manager {
                        tokio::spawn(async move {
                            if manager.peer(&fid).await.is_none() {
                                if let Err(e) = manager.dial(&fid).await {
                                    tracing::debug!(?e, "ptt prewarm dial failed");
                                }
                            }
                        });
                    }
                }
            }
            reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true}));
        }
        "ptt-stop" => {
            let stop = { state.lock().await.ptt_stop.take() };
            if let Some(s) = stop {
                s.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            reply(&cmd.reply, serde_json::json!({"reply": req_id, "ok": true}));
        }
        other => {
            tracing::debug!(%other, "ipc: unknown command");
            reply(
                &cmd.reply,
                serde_json::json!({"reply": req_id, "error": format!("unknown cmd: {other}")}),
            );
        }
    }
}

/// Look up `friend_id` in the running daemon's cached friend list and check
/// whether sending an action of `action_kind` to them would actually land.
/// Returns `Some((reason, friend_username))` when the send should be blocked
/// by the *sender* (no daemon → can't check → returns `None`, defer to the
/// server, which queues for offline delivery).
async fn recipient_unreachable(friend_id: &str, action_kind: &str) -> Option<(String, String)> {
    if !daemon_is_running() {
        return None;
    }
    let reply = ipc_request(
        serde_json::json!({"cmd": "friend-state", "friend_id": friend_id}),
        true,
    )
    .await?;
    let username = reply
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or(friend_id)
        .to_string();
    // Mutual-friend gate: actions and PTT are only allowed between accepted
    // friends. A merely pending relationship (either direction) is refused —
    // the server enforces this too, this just surfaces it clearly + early.
    match reply.get("status").and_then(|v| v.as_str()).unwrap_or("") {
        "accepted" => {}
        "pending_out" => {
            return Some((
                format!("{username} hasn't accepted your friend request yet"),
                username,
            ))
        }
        "pending_in" => {
            return Some((
                format!("{username} sent you a request — accept it first with `friend accept {username}`"),
                username,
            ))
        }
        _ => return Some((format!("you're not friends with {username}"), username)),
    }
    let online = reply.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
    if !online {
        return Some((format!("{username} is offline"), username));
    }
    let presence = reply.get("presence").and_then(|v| v.as_str()).unwrap_or("online");
    if presence == "dnd" {
        // Receiver can still permit individual kinds — but the sender
        // doesn't know their per-kind config. Block on the safe side.
        return Some(
            (format!("{username} is in Do Not Disturb — your {action_kind} would be dropped"),
             username),
        );
    }
    None
}

/// Send a single IPC command to the running daemon and (optionally) wait for
/// its reply. The daemon stamps replies with the same `id` we sent. Returns
/// the matching reply JSON, or `None` if we never got one (daemon down, IPC
/// upgrade failed, timeout, etc.).
async fn ipc_request(mut cmd: serde_json::Value, wait_reply: bool) -> Option<serde_json::Value> {
    if !daemon_is_running() {
        return None;
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(obj) = cmd.as_object_mut() {
        obj.insert("id".into(), serde_json::Value::String(id.clone()));
    }
    let url = format!("ws://127.0.0.1:{}", overlay_ipc::DEFAULT_PORT);
    let Ok((ws, _)) = tokio_tungstenite::connect_async(&url).await else {
        return None;
    };
    use futures_util::{SinkExt, StreamExt};
    let (mut sink, mut stream) = ws.split();
    let Ok(s) = serde_json::to_string(&cmd) else { return None };
    if sink
        .send(tokio_tungstenite::tungstenite::Message::Text(s))
        .await
        .is_err()
    {
        return None;
    }
    if !wait_reply {
        // Tiny grace so daemon picks up the cmd before we drop the socket.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let _ = sink.close().await;
        return Some(serde_json::json!({"ok": true}));
    }
    // 4s window — slightly longer than the longest server wait the daemon
    // does (`friend-list` waits up to 1.5s + spawn + WS round-trips).
    let timeout = tokio::time::sleep(std::time::Duration::from_secs(4));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => {
                let _ = sink.close().await;
                return None;
            }
            frame = stream.next() => {
                let Some(Ok(msg)) = frame else { return None };
                if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        if v.get("reply").and_then(|x| x.as_str()) == Some(id.as_str()) {
                            let _ = sink.close().await;
                            return Some(v);
                        }
                    }
                }
            }
        }
    }
}

/// Convenience: same as `ipc_request(cmd, false)` for fire-and-forget cmds.
async fn send_via_daemon(cmd: serde_json::Value) -> bool {
    ipc_request(cmd, false).await.is_some()
}

/// Stop only the daemon we spawned (via its pidfile) — not the REPL we're
/// currently running in, and not unrelated sociacli processes.
fn stop_running_daemon() {
    let Ok(path) = daemon_pidfile() else { return };
    let Ok(raw) = std::fs::read_to_string(&path) else { return };
    let Ok(pid) = raw.trim().parse::<u32>() else { return };
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .output();
    }
    let _ = std::fs::remove_file(&path);
}

fn daemon_pidfile() -> Result<std::path::PathBuf> {
    let p = Config::path()?;
    let dir = p.parent().context("config has no parent dir")?;
    Ok(dir.join("daemon.pid"))
}

fn write_daemon_pidfile() {
    if let Ok(p) = daemon_pidfile() {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
}

async fn plugin_cmd(cfg: &mut Config, op: PluginCmd) -> Result<()> {
    match op {
        PluginCmd::List => {
            let plugins = plugin::list(&*cfg)?;
            if plugins.is_empty() {
                println!("(no plugins installed in {})", cfg.plugins_dir().display());
            }
            for p in plugins {
                println!(
                    "{} v{}\t{}\t{}",
                    p.manifest.id, p.manifest.version, p.manifest.name, p.manifest.description
                );
            }
        }
        PluginCmd::Install { path } => {
            let p = plugin::install(&*cfg, &path)?;
            println!("installed {} v{} -> {}", p.manifest.id, p.manifest.version, p.dir.display());
        }
        PluginCmd::Run { id, args } => {
            let p = plugin::find(&*cfg, &id)?;
            let parsed: serde_json::Value =
                serde_json::from_str(&args).context("--args must be valid JSON")?;
            let ctx = plugin::RunCtx {
                me: cfg.user_id.clone().unwrap_or_default(),
                username: cfg.username.clone(),
                server_url: cfg.server_url.clone(),
                from: None,
                args: parsed,
                plugin: p,
                signal_tx: None,
                overlay: None,
            };
            // mlua isn't Send across await points; run on a blocking thread.
            tokio::task::spawn_blocking(move || plugin::run(ctx)).await??;
        }
        PluginCmd::Send { friend, id, args } => {
            let p = plugin::find(&*cfg, &id)?;
            let parsed: serde_json::Value =
                serde_json::from_str(&args).context("--args must be valid JSON")?;
            let envelope = plugin::payload_envelope(&p, parsed);
            send_action(
                &*cfg,
                &friend,
                ActionKind::Plugin,
                &p.manifest.name,
                &p.manifest.description,
                envelope,
            )
            .await?;
        }
        PluginCmd::Trust { friend_id, plugin_id } => {
            cfg.plugin_trust
                .entry(plugin_id.clone())
                .or_default()
                .insert(friend_id.clone(), true);
            cfg.save()?;
            println!("trusted: friend {friend_id} may run plugin `{plugin_id}` on this client");
        }
        PluginCmd::Untrust { friend_id, plugin_id } => {
            if let Some(m) = cfg.plugin_trust.get_mut(&plugin_id) {
                m.remove(&friend_id);
                if m.is_empty() {
                    cfg.plugin_trust.remove(&plugin_id);
                }
            }
            cfg.save()?;
            println!("revoked: friend {friend_id} for plugin `{plugin_id}`");
        }
        PluginCmd::Trusted => {
            if cfg.plugin_trust.is_empty() {
                println!("(no trust entries)");
            }
            for (pid, friends) in &cfg.plugin_trust {
                for (fid, &allowed) in friends {
                    if allowed {
                        println!("{pid}\t{fid}");
                    }
                }
            }
        }
    }
    Ok(())
}

async fn handle_incoming_plugin(cfg: &Config, from: &str, data: &serde_json::Value) {
    let meta = data.get("plugin");
    let Some(plugin_id) = meta.and_then(|m| m.get("id")).and_then(|v| v.as_str()) else {
        tracing::warn!(%from, "plugin action without plugin.id");
        return;
    };
    let name = meta.and_then(|m| m.get("name")).and_then(|v| v.as_str()).unwrap_or(plugin_id);
    let version = meta.and_then(|m| m.get("version")).and_then(|v| v.as_str()).unwrap_or("?");
    let description = meta.and_then(|m| m.get("description")).and_then(|v| v.as_str()).unwrap_or("");
    let author = meta.and_then(|m| m.get("author")).and_then(|v| v.as_str()).unwrap_or("");

    if !cfg.is_plugin_trusted(plugin_id, from) {
        println!(
            "[plugin] AUTHORIZATION REQUIRED — {from} sent plugin `{plugin_id}` ({name} v{version} by {author}).\n          {description}\n          Authorize with:  sociacli plugin trust {from} {plugin_id}"
        );
        return;
    }

    // Trusted. Try to invoke the local copy if installed with matching id.
    let plugin = match plugin::find(cfg, plugin_id) {
        Ok(p) => p,
        Err(e) => {
            println!(
                "[plugin] trusted but not installed locally: {plugin_id} — {e}. Install with `sociacli plugin install <path>`."
            );
            return;
        }
    };
    let args = data.get("args").cloned().unwrap_or(serde_json::json!({}));
    let me = cfg.user_id.clone().unwrap_or_default();
    let username = cfg.username.clone();
    let server_url = cfg.server_url.clone();
    let from_owned = from.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let ctx = plugin::RunCtx {
            me,
            username,
            server_url,
            from: Some(from_owned),
            args,
            plugin,
            signal_tx: None, // TODO: thread the daemon's signaling tx through
            overlay: None,   // TODO: thread the daemon's overlay handle through
        };
        plugin::run(ctx)
    })
    .await;
    if let Ok(Err(e)) = result {
        tracing::error!(?e, "plugin run failed");
    }
}

/// Best-effort, non-blocking notice if a newer release exists. Caps frequency
/// to once per 24 h via `cfg.last_update_check`.
async fn maybe_notify_update(cfg: &mut Config, overlay: Option<overlay_ipc::OverlayIpc>) {
    if !cfg.auto_update_check {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stale = cfg
        .last_update_check
        .map(|t| now.saturating_sub(t) > 86_400)
        .unwrap_or(true);
    if !stale {
        return;
    }
    let result = tokio::task::spawn_blocking(updater::latest_if_newer).await;
    cfg.last_update_check = Some(now);
    let _ = cfg.save();
    match result {
        Ok(Ok(Some(v))) => {
            println!(
                "[update] sociacli {v} available (current {}) — run `sociacli update`",
                updater::current_version()
            );
            if let Some(ov) = overlay.as_ref() {
                push_update_prompt(ov, false, Some(v));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => tracing::debug!(?e, "update check failed"),
        Err(e) => tracing::debug!(?e, "update check task panicked"),
    }
}

/// One-shot setup wizard that fires the very first time the REPL is opened
/// after a successful login: collects the PTT hotkey + default PTT friend so
/// the user can start talking without reading the docs.
async fn first_run_wizard(
    rl: &mut rustyline::DefaultEditor,
    cfg: &mut Config,
) -> Result<()> {
    println!();
    println!("─── First-time setup ──────────────────────────────────");
    println!("Defaults that come with sociacli:");
    let open_combo: String = cfg
        .shortcuts
        .get("open")
        .cloned()
        .unwrap_or_else(|| "Ctrl+Alt+Shift+KeyF".into());
    let ptt_combo: String = cfg
        .shortcuts
        .get("ptt")
        .cloned()
        .unwrap_or_else(|| "Ctrl+Alt+Shift+KeyK".into());
    println!("  • Open hotkey : {open_combo}");
    println!("  • PTT hotkey  : {ptt_combo}   (HOLD to talk, release to stop)");
    println!();
    println!("Press Enter to accept each, or type a new combo (e.g. Ctrl+Alt+KeyT).");

    let new_open = rl.readline(&format!("> open hotkey [{open_combo}]: "))?;
    let new_open = new_open.trim();
    if !new_open.is_empty() {
        if new_open.parse::<global_hotkey::hotkey::HotKey>().is_ok() {
            cfg.shortcuts.insert("open".into(), new_open.into());
        } else {
            println!("(invalid combo, keeping default)");
        }
    }
    let new_ptt = rl.readline(&format!("> PTT hotkey  [{ptt_combo}]: "))?;
    let new_ptt = new_ptt.trim();
    if !new_ptt.is_empty() {
        if new_ptt.parse::<global_hotkey::hotkey::HotKey>().is_ok() {
            cfg.shortcuts.insert("ptt".into(), new_ptt.into());
        } else {
            println!("(invalid combo, keeping default)");
        }
    }

    let friend = rl.readline(
        "> default PTT friend (their username, or blank to skip): ",
    )?;
    let friend = friend.trim();
    if !friend.is_empty() {
        cfg.default_ptt_friend = Some(friend.to_string());
    }

    cfg.first_run_done = true;
    cfg.save()?;
    println!();
    println!(
        "Done. PTT: HOLD `{}` to talk{}.",
        cfg.shortcuts.get("ptt").map(|s| s.as_str()).unwrap_or("?"),
        cfg.default_ptt_friend
            .as_deref()
            .map(|f| format!(" with {f}"))
            .unwrap_or_default()
    );
    println!("Change later with: `shortcut set ptt <combo>` or `ptt-friend <name>`.");
    println!("───────────────────────────────────────────────────────");
    println!();
    // Restart daemon so the new hotkey bindings take effect.
    if cfg.background_enabled {
        stop_running_daemon();
        ensure_daemon_running();
    }
    Ok(())
}

/// First-run interactive flow: ask the user whether they want to register
/// or sign in, then run that path. Persists the resulting token to `cfg`.
async fn first_run_auth(
    rl: &mut rustyline::DefaultEditor,
    cfg: &mut Config,
) -> Result<()> {
    println!();
    println!("Welcome to sociacli — you're not signed in yet.");
    println!("Server: {}", cfg.server_url);
    println!("  1) Register a new account");
    println!("  2) Sign in to an existing account");
    println!("  3) Skip for now");

    let choice = loop {
        let line = rl.readline("> choice [1/2/3]: ")?;
        match line.trim() {
            "1" | "register" | "r" => break 1,
            "2" | "login" | "l" => break 2,
            "3" | "skip" | "s" | "" => break 3,
            other => println!("pick 1, 2, or 3 (got `{other}`)"),
        }
    };
    if choice == 3 {
        println!("ok — run `register <name>` or `login <name>` whenever.");
        return Ok(());
    }

    let username = loop {
        let line = rl.readline("> username: ")?;
        let u = line.trim();
        if !u.is_empty() {
            break u.to_string();
        }
        println!("username can't be empty");
    };
    let password = rpassword::prompt_password("> password: ")?;

    let r = if choice == 1 {
        api::register(&cfg.server_url, &username, &password).await?
    } else {
        api::login(&cfg.server_url, &username, &password).await?
    };
    cfg.user_id = Some(r.id.clone());
    cfg.username = Some(r.username.clone());
    cfg.token = Some(r.token);
    cfg.save()?;
    println!(
        "{} as {} ({})",
        if choice == 1 { "registered + logged in" } else { "signed in" },
        r.username,
        r.id
    );
    Ok(())
}

async fn repl(cfg: &mut Config) -> Result<()> {
    use rustyline::error::ReadlineError;
    // Update check runs in the background — REPL banner shows immediately
    // and the result (if any) is printed when it lands.
    {
        let mut cfg_clone = cfg.clone();
        tokio::spawn(async move { maybe_notify_update(&mut cfg_clone, None).await });
    }
    // Background daemon: opt-out via `sociacli settings background off`.
    if cfg.background_enabled {
        // First-run autostart setup — register with HKCU\Run / systemd-user /
        // LaunchAgent so the daemon comes back at next login without action.
        if !cfg.autostart_installed {
            match service::install() {
                Ok(msg) => {
                    cfg.autostart_installed = true;
                    let _ = cfg.save();
                    tracing::info!("autostart installed: {msg}");
                }
                Err(e) => tracing::warn!(?e, "autostart install failed (continuing)"),
            }
        }
        // Ensure a daemon is alive *right now* so the REPL session also
        // receives notifications without waiting for the next login.
        ensure_daemon_running();
        // Mirror the daemon's incoming-action stream into this REPL.
        spawn_repl_action_tap();
    } else {
        println!("(background daemon disabled — no notifications. Enable with `settings background on`.)");
    }
    let mut rl = rustyline::DefaultEditor::new()?;
    let history = Config::path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("history.txt")));
    if let Some(h) = &history {
        let _ = rl.load_history(h);
    }

    println!(
        "sociacli {} — interactive mode. `help`, `exit`, ↑/↓ for history. \
         Try `whoami`, `friend list`, `msg <user> hi`.",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(u) = &cfg.username {
        println!("logged in as {u}");
    } else if let Err(e) = first_run_auth(&mut rl, cfg).await {
        eprintln!("auth aborted: {e:#}");
        println!("you can still browse — try `register <name>` or `login <name>` later");
    }
    if cfg.username.is_some() && !cfg.first_run_done {
        if let Err(e) = first_run_wizard(&mut rl, cfg).await {
            tracing::warn!(?e, "first-run wizard skipped");
        }
    }

    loop {
        let presence_tag = match cfg.presence.as_str() {
            "dnd" => " · do not disturb",
            _ => "",
        };
        let prompt = format!(
            "sociacli{}> ",
            cfg.username
                .as_deref()
                .map(|u| format!("({u}{presence_tag})"))
                .unwrap_or_default()
        );
        match rl.readline(&prompt) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                if matches!(line, "exit" | "quit" | ":q") {
                    break;
                }
                let parts = match shlex::split(line) {
                    Some(p) => p,
                    None => {
                        eprintln!("parse error (unmatched quotes?)");
                        continue;
                    }
                };
                // REPL parser: drop the `sociacli` prefix from clap's usage /
                // error / help output so users see the same syntax they type.
                let cmd_def = Cli::command()
                    .no_binary_name(true)
                    .bin_name("")
                    .name("");
                match cmd_def.clone().try_get_matches_from(&parts) {
                    Ok(matches) => match Cli::from_arg_matches(&matches) {
                        Ok(parsed) => {
                            if let Some(cmd) = parsed.cmd {
                                if let Err(e) = dispatch(cmd, cfg).await {
                                    eprintln!("error: {e:#}");
                                }
                            }
                        }
                        Err(e) => {
                            let _ = e.print();
                        }
                    },
                    Err(e) => {
                        let _ = e.print();
                    }
                }
            }
            Err(ReadlineError::Interrupted) => continue, // Ctrl+C
            Err(ReadlineError::Eof) => break,            // Ctrl+D
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }
    if let Some(h) = &history {
        let _ = rl.save_history(h);
    }
    Ok(())
}

// ---- one-shot signaling helpers ----

async fn open_and_wait_hello(cfg: &Config) -> Result<signaling::Signaling> {
    let mut sig = signaling::connect(cfg).await?;
    // Bound the wait: if the server accepts the socket but never sends HelloOk
    // (e.g. it silently kicked this connection because the daemon holds the
    // same token), `rx.recv()` would otherwise block forever and the command
    // would "load endlessly".
    let deadline = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                anyhow::bail!("timed out waiting for server hello (is the daemon holding the connection? try `daemon restart`)");
            }
            maybe = sig.rx.recv() => {
                match maybe {
                    Some(ServerMsg::HelloOk { .. }) => break,
                    Some(_) => continue,
                    None => anyhow::bail!("connection closed before server hello"),
                }
            }
        }
    }
    Ok(sig)
}

async fn resolve_friend_oneshot(cfg: &Config, ident: &str) -> Result<String> {
    if uuid::Uuid::parse_str(ident).is_ok() {
        return Ok(ident.to_string());
    }
    // Preferred path: ask the daemon's cached friend list.
    if let Some(reply) = ipc_request(
        serde_json::json!({"cmd": "resolve-friend", "ident": ident}),
        true,
    )
    .await
    {
        if let Some(id) = reply.get("id").and_then(|v| v.as_str()) {
            return Ok(id.to_string());
        }
        if let Some(err) = reply.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("{err}");
        }
    }
    // Fallback: no daemon — open our own short-lived WS.
    let mut sig = open_and_wait_hello(cfg).await?;
    signaling::send(&sig.tx, &ClientMsg::FriendList)?;
    while let Some(msg) = sig.rx.recv().await {
        match msg {
            ServerMsg::Friends { friends } => {
                for f in friends {
                    if f.username == ident {
                        return Ok(f.id);
                    }
                }
                anyhow::bail!("no friend named {ident}");
            }
            ServerMsg::Error { message } => anyhow::bail!("server: {message}"),
            _ => continue,
        }
    }
    anyhow::bail!("ws closed before friend list arrived");
}

/// Filter the friend list by a case-insensitive username substring (or a
/// full UUID match). Uses the IPC friend-list path so the result reflects
/// the most recent server snapshot.
async fn friend_find(cfg: &Config, query: &str) -> Result<()> {
    let friends: Vec<serde_json::Value> = if daemon_is_running() {
        match ipc_request(
            serde_json::json!({"cmd": "friend-list"}),
            true,
        )
        .await
        {
            Some(reply) => reply
                .get("friends")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        }
    } else {
        let mut sig = open_and_wait_hello(cfg).await?;
        signaling::send(&sig.tx, &ClientMsg::FriendList)?;
        let timeout = tokio::time::sleep(Duration::from_millis(800));
        tokio::pin!(timeout);
        let mut out = Vec::new();
        loop {
            tokio::select! {
                _ = &mut timeout => break,
                maybe = sig.rx.recv() => {
                    let Some(msg) = maybe else { break };
                    if let ServerMsg::Friends { friends } = msg {
                        out = friends
                            .into_iter()
                            .filter_map(|f| serde_json::to_value(f).ok())
                            .collect();
                        break;
                    }
                }
            }
        }
        out
    };

    let q = query.to_lowercase();
    let hits: Vec<&serde_json::Value> = friends
        .iter()
        .filter(|f| {
            let u = f.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("");
            u.to_lowercase().contains(&q) || id == query
        })
        .collect();

    if hits.is_empty() {
        println!("(no matches for `{query}`)");
        return Ok(());
    }
    for f in hits {
        let online = f.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
        let presence = f.get("presence").and_then(|v| v.as_str()).unwrap_or("offline");
        let label = match (online, presence) {
            (true, "dnd") => "do not disturb",
            (true, _)     => "online        ",
            (false, _)    => "offline       ",
        };
        println!(
            "[{label}] [{}] {} — {}",
            f.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
            f.get("username").and_then(|v| v.as_str()).unwrap_or("?"),
            f.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
        );
    }
    Ok(())
}

async fn friend_cmd(cfg: &Config, op: FriendCmd) -> Result<()> {
    // `friend find <q>` is purely client-side: filter the cached list.
    if let FriendCmd::Find { query } = &op {
        return friend_find(cfg, query).await;
    }
    // accept/remove resolve usernames server-side now (no client-side cache
    // lookup), so they work even when the daemon's friend cache is stale.
    // Preferred path: daemon does the WS work + replies with snapshot.
    if daemon_is_running() {
        let cmd = match &op {
            FriendCmd::Add { username } => {
                serde_json::json!({"cmd": "friend-add", "username": username})
            }
            FriendCmd::Accept { username } => {
                serde_json::json!({"cmd": "friend-accept", "username": username})
            }
            FriendCmd::Remove { username } => {
                serde_json::json!({"cmd": "friend-remove", "username": username})
            }
            FriendCmd::List => serde_json::json!({"cmd": "friend-list"}),
            FriendCmd::Find { .. } => unreachable!("handled above"),
        };
        let want_list = matches!(op, FriendCmd::List);
        if let Some(reply) = ipc_request(cmd, true).await {
            if let Some(err) = reply.get("error").and_then(|v| v.as_str()) {
                anyhow::bail!("server: {err}");
            }
            if want_list {
                let friends = reply
                    .get("friends")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if friends.is_empty() {
                    println!("(no friends)");
                }
                for f in friends {
                    let online = f.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
                    let presence = f.get("presence").and_then(|v| v.as_str()).unwrap_or("offline");
                    let label = match (online, presence) {
                        (true, "dnd") => "do not disturb",
                        (true, _)     => "online        ",
                        (false, _)    => "offline       ",
                    };
                    println!(
                        "[{label}] [{}] {} — {}",
                        f.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
                        f.get("username").and_then(|v| v.as_str()).unwrap_or("?"),
                        f.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                    );
                }
            } else {
                println!("ok");
            }
            return Ok(());
        }
    }
    // Fallback: no daemon — open our own short-lived WS.
    let mut sig = open_and_wait_hello(cfg).await?;
    match op {
        FriendCmd::Add { username } => signaling::send(
            &sig.tx,
            &ClientMsg::FriendAdd { username: &username },
        )?,
        FriendCmd::Accept { username } => {
            signaling::send(&sig.tx, &ClientMsg::FriendAccept { username: &username })?
        }
        FriendCmd::Remove { username } => {
            signaling::send(&sig.tx, &ClientMsg::FriendRemove { username: &username })?
        }
        FriendCmd::List => signaling::send(&sig.tx, &ClientMsg::FriendList)?,
        FriendCmd::Find { .. } => unreachable!("handled above"),
    }
    // Bound the wait — `add` / `accept` / `remove` don't echo Friends back on
    // success, so we time out after a short grace instead of hanging.
    let timeout = tokio::time::sleep(Duration::from_millis(500));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => {
                println!("ok");
                break;
            }
            maybe = sig.rx.recv() => {
                let Some(msg) = maybe else { break };
                match msg {
                    ServerMsg::Friends { friends } => {
                        if friends.is_empty() {
                            println!("(no friends)");
                        }
                        for f in friends {
                            let dot = if f.online { "*" } else { " " };
                            println!("{dot} [{}] {} — {}", f.status, f.username, f.id);
                        }
                        break;
                    }
                    ServerMsg::Error { message } => anyhow::bail!("server: {message}"),
                    _ => continue,
                }
            }
        }
    }
    Ok(())
}

async fn perm_cmd(cfg: &Config, op: PermCmd) -> Result<()> {
    if daemon_is_running() {
        let cmd = match &op {
            PermCmd::List => serde_json::json!({"cmd": "perm-list"}),
            PermCmd::Set { friend_id, action, allowed } => serde_json::json!({
                "cmd": "perm-set",
                "friend_id": friend_id,
                "action": action.as_str(),
                "allowed": allowed,
            }),
        };
        let want_list = matches!(op, PermCmd::List);
        if let Some(reply) = ipc_request(cmd, true).await {
            if let Some(err) = reply.get("error").and_then(|v| v.as_str()) {
                anyhow::bail!("server: {err}");
            }
            if want_list {
                let perms = reply.get("perms").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                if perms.is_empty() {
                    println!("(defaults: all friends allowed for all actions)");
                }
                for p in perms {
                    println!(
                        "{} {} {}",
                        if p.get("allowed").and_then(|v| v.as_bool()).unwrap_or(false) {
                            "allow"
                        } else {
                            "block"
                        },
                        p.get("action").and_then(|v| v.as_str()).unwrap_or("?"),
                        p.get("friend_id").and_then(|v| v.as_str()).unwrap_or("?"),
                    );
                }
            } else {
                println!("ok");
            }
            return Ok(());
        }
    }
    let mut sig = open_and_wait_hello(cfg).await?;
    match op {
        PermCmd::List => signaling::send(&sig.tx, &ClientMsg::PermList)?,
        PermCmd::Set {
            friend_id,
            action,
            allowed,
        } => signaling::send(
            &sig.tx,
            &ClientMsg::PermUpdate {
                friend_id: &friend_id,
                action,
                allowed,
            },
        )?,
    }
    let timeout = tokio::time::sleep(Duration::from_millis(500));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => {
                println!("ok");
                break;
            }
            maybe = sig.rx.recv() => {
                let Some(msg) = maybe else { break };
                match msg {
                    ServerMsg::Perms { perms } => {
                        if perms.is_empty() {
                            println!("(defaults: all friends allowed for all actions)");
                        }
                        for p in perms {
                            println!(
                                "{} {} {}",
                                if p.allowed { "allow" } else { "block" },
                                p.action,
                                p.friend_id
                            );
                        }
                        break;
                    }
                    ServerMsg::Error { message } => anyhow::bail!("server: {message}"),
                    _ => continue,
                }
            }
        }
    }
    Ok(())
}

async fn send_action(
    cfg: &Config,
    friend_ident: &str,
    action: ActionKind,
    title: &str,
    body: &str,
    data: serde_json::Value,
) -> Result<()> {
    let id = resolve_friend_oneshot(cfg, friend_ident).await?;
    // Friend-state gate: only allow sending when the recipient is reachable
    // (online and not DND-blocking this action kind).
    if let Some((reason, _)) = recipient_unreachable(&id, action.as_str()).await {
        anyhow::bail!("can't reach {friend_ident}: {reason}");
    }
    let action_str = action.as_str();
    // Preferred path: route through the running daemon so we don't kick its
    // WS by opening one of our own with the same token.
    let cmd = serde_json::json!({
        "cmd": "send-action",
        "to": id,
        "action": action_str,
        "title": title,
        "body": body,
        "data": data.clone(),
    });
    if daemon_is_running() {
        if let Some(reply) = ipc_request(cmd, true).await {
            if let Some(err) = reply.get("error").and_then(|v| v.as_str()) {
                anyhow::bail!("server: {err}");
            }
            println!("→ {friend_ident}: {title} — {body}");
            return Ok(());
        }
    }
    // Fallback: no daemon — open our own short-lived WS.
    let mut sig = open_and_wait_hello(cfg).await?;
    signaling::send(
        &sig.tx,
        &ClientMsg::SendAction {
            to: &id,
            action,
            title,
            body,
            data,
        },
    )?;
    let timeout = tokio::time::sleep(Duration::from_millis(300));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => break,
            Some(m) = sig.rx.recv() => {
                if let ServerMsg::Error { message } = m {
                    anyhow::bail!("server: {message}");
                }
            }
        }
    }
    println!("→ {friend_ident}: {title} — {body}");
    Ok(())
}

// ---- long-lived daemon (listen + ptt share this) ----

struct PttPlan {
    friend_id: String,
}

/// Connect to the signaling server, retrying forever with capped backoff.
/// Used for both the daemon's first connect and every reconnect — a transient
/// failure (no network at boot, server restart, same-token kick) must never
/// kill the daemon, or the REPL loses its IPC peer.
/// Outcome of the daemon's connect attempts.
enum ConnOutcome {
    /// Connected — drive this signaling session.
    Connected(signaling::Signaling),
    /// Server rejected us with 426: this build is below `min_client_version`.
    /// Carries the target version (best-effort) for the update card. The daemon
    /// must NOT keep dialing — retrying can't succeed until the user updates, so
    /// we stop here (no polling) and let the gated loop serve the Update button.
    NeedsUpdate(Option<String>),
}

/// Connect, retrying transient failures (no network at boot, server restart,
/// same-token kick) with capped backoff. A 426 (mandatory update) short-circuits
/// to `NeedsUpdate` instead of looping — there is nothing to retry until the
/// user updates, so we never poll in that state.
async fn connect_with_retry(cfg: &Config) -> ConnOutcome {
    let mut backoff = std::time::Duration::from_millis(500);
    loop {
        match signaling::connect(cfg).await {
            Ok(s) => return ConnOutcome::Connected(s),
            Err(e) if e.to_string().contains("(426)") => {
                let latest = tokio::task::spawn_blocking(updater::latest_if_newer)
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .flatten();
                return ConnOutcome::NeedsUpdate(latest);
            }
            Err(e) => {
                tracing::warn!(?e, "signaling connect failed — retrying in {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
            }
        }
    }
}

/// Parked state entered when a mandatory update blocks the server connection.
/// Stops dialing entirely (no polling) and keeps the daemon alive only to serve
/// the overlay's "Update now" button — a successful self-update + restart brings
/// up a new build that can connect.
async fn gated_loop(
    overlay: overlay_ipc::OverlayIpc,
    mut cmd_rx: overlay_ipc::CmdRx,
) -> Result<()> {
    tracing::warn!("mandatory update required — connection parked until updated");
    while let Some(cmd) = cmd_rx.recv().await {
        if cmd.req.get("cmd").and_then(|v| v.as_str()) == Some("self-update") {
            handle_self_update(&overlay, &cmd.reply).await;
        }
        // Every other command needs the server, which we can't reach — ignore.
    }
    Ok(())
}

/// Keep Windows "Installed apps" (Add/Remove Programs) in sync after a
/// self-update. `sociacli update` swaps the .exe in place but never touches the
/// registry, so the Inno uninstall entry's `DisplayVersion` would stay frozen
/// at whatever the installer last wrote. Patch it to the new version. Only
/// updates a key that already exists (never creates a phantom ARP entry), and
/// checks both HKCU (per-user install) and HKLM (admin install). No-op off
/// Windows.
fn sync_installed_version(version: &str) {
    #[cfg(windows)]
    {
        // Inno AppId `{{8B5C2D9A-…}}` → uninstall subkey `{8B5C2D9A-…}_is1`.
        const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{8B5C2D9A-5C1A-4FBC-9C0F-0E9D1F8C3B11}_is1";
        for root in ["HKCU", "HKLM"] {
            let full = format!("{root}\\{KEY}");
            let exists = std::process::Command::new("reg")
                .args(["query", &full, "/v", "DisplayVersion"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if exists {
                let _ = std::process::Command::new("reg")
                    .args(["add", &full, "/v", "DisplayVersion", "/t", "REG_SZ", "/d", version, "/f"])
                    .output();
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = version;
    }
}

/// Push an "update" card to the overlay. `mandatory` cards have no dismiss and
/// block until the user updates; advisory cards add a "Later" button. Both
/// carry an "Update now" button that triggers the daemon's self-update.
fn push_update_prompt(
    overlay: &overlay_ipc::OverlayIpc,
    mandatory: bool,
    latest: Option<String>,
) {
    let current = updater::current_version();
    let (title, body) = if mandatory {
        (
            "Update required",
            match &latest {
                Some(v) => format!(
                    "sociacli {v} is required to keep connecting (you have {current})."
                ),
                None => format!(
                    "A newer sociacli is required to keep connecting (you have {current})."
                ),
            },
        )
    } else {
        (
            "Update available",
            match &latest {
                Some(v) => format!("sociacli {v} is available (you have {current})."),
                None => format!("A sociacli update is available (you have {current})."),
            },
        )
    };
    overlay.send(overlay_ipc::OverlayEvent::new(
        "update",
        serde_json::json!({
            "mandatory": mandatory,
            "current": current,
            "latest": latest,
            "title": title,
            "body": body,
        }),
    ));
}

/// Run a self-update in response to the overlay's "Update now" button, pushing
/// progress / result cards back to the overlay (and an IPC reply to the caller).
async fn handle_self_update(
    overlay: &overlay_ipc::OverlayIpc,
    reply: &mpsc::UnboundedSender<String>,
) {
    overlay.send(overlay_ipc::OverlayEvent::new(
        "update",
        serde_json::json!({
            "mandatory": false, "status": "updating",
            "title": "Updating…", "body": "Downloading the latest sociacli…",
        }),
    ));
    let res = tokio::task::spawn_blocking(updater::apply_latest).await;
    let (title, body, ok) = match res {
        Ok(Ok(self_update::Status::Updated(v))) => {
            sync_installed_version(&v);
            (
                "Update complete",
                format!("Updated to {v}. Restart sociacli to apply."),
                true,
            )
        }
        Ok(Ok(self_update::Status::UpToDate(v))) => {
            ("Already up to date", format!("You're on the latest ({v})."), true)
        }
        Ok(Err(e)) => ("Update failed", format!("{e}"), false),
        Err(e) => ("Update failed", format!("update task panicked: {e}"), false),
    };
    overlay.send(overlay_ipc::OverlayEvent::new(
        "update",
        serde_json::json!({
            "mandatory": false, "status": "done",
            "title": title, "body": body,
        }),
    ));
    let _ = reply.send(
        serde_json::json!({"ok": ok, "body": body}).to_string(),
    );
}

async fn run_daemon(cfg: &Config, ptt: Option<PttPlan>) -> Result<()> {
    use overlay_ipc::OverlayEvent;

    // The IPC port bind is our single-instance lock. If it fails, another
    // daemon already owns it — exit now (before writing the pidfile or opening
    // a signaling WS) instead of starting a second same-token connection that
    // would ping-pong the server's kick logic into an endless ws_open/ws_close
    // storm.
    let (overlay, mut cmd_rx) = match overlay_ipc::spawn(overlay_ipc::DEFAULT_PORT).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(?e, "another sociacli daemon already running — exiting");
            return Ok(());
        }
    };
    write_daemon_pidfile();

    // Background update check: clone the config snapshot we'll fire-and-forget
    // through; persistence happens inside `maybe_notify_update`.
    {
        let mut cfg_clone = cfg.clone();
        let ov = overlay.clone();
        tokio::spawn(async move {
            maybe_notify_update(&mut cfg_clone, Some(ov)).await;
        });
    }
    let state: SharedState = Arc::new(tokio::sync::Mutex::new(DaemonState::default()));
    state.lock().await.overlay = Some(overlay.clone());
    overlay.send(OverlayEvent::new(
        "config",
        serde_json::json!({
            "notify_sound": cfg.notify_sound,
            "username": cfg.username,
            "user_id": cfg.user_id,
            "notify_auto_dismiss": cfg.notify_auto_dismiss,
            "notify_auto_dismiss_ms": cfg.notify_auto_dismiss_ms,
            "notify_show_list": cfg.notify_show_list,
        }),
    ));

    let fx = peers::AudioFx {
        radio_effect: cfg.radio_effect,
        radio_beeps: cfg.radio_beeps,
    };
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<peers::IncomingAction>();

    // First connect — retry transient failures (no network at boot, etc.) so a
    // cold boot doesn't kill the daemon. A 426 parks us in the gated loop with
    // the Update card instead of dialing forever.
    let mut sig = match connect_with_retry(cfg).await {
        ConnOutcome::Connected(s) => s,
        ConnOutcome::NeedsUpdate(latest) => {
            push_update_prompt(&overlay, true, latest);
            return gated_loop(overlay, cmd_rx).await;
        }
    };
    #[allow(unused_assignments)]
    let ice = peers::PeerManager::ice_from_config(&cfg.ice_servers);
    let mut manager =
        peers::PeerManager::new(sig.tx.clone(), action_tx.clone(), fx, ice.clone());
    {
        let mut g = state.lock().await;
        g.manager = Some(manager.clone());
        g.signal_tx = Some(sig.tx.clone());
        g.last_ptt_friend = cfg.default_ptt_friend.clone();
        g.ptt_input_device = cfg.ptt_input_device.clone();
        g.presence = if cfg.presence.is_empty() { "online".into() } else { cfg.presence.clone() };
        g.dnd_block_message = cfg.dnd_block_message;
        g.dnd_block_game_invite = cfg.dnd_block_game_invite;
        g.dnd_block_ptt = cfg.dnd_block_ptt;
        g.dnd_block_plugin = cfg.dnd_block_plugin;
        g.dnd_block_custom = cfg.dnd_block_custom;
        g.notify_friend_presence = cfg.notify_friend_presence;
        g.notify_friend_requests = cfg.notify_friend_requests;
    }
    // Bring up the Tauri overlay window once (if its binary is alongside us).
    spawn_overlay_if_present();
    tracing::info!(
        "sociacli daemon v{} (build {}) — listening, Ctrl+C to stop",
        env!("CARGO_PKG_VERSION"),
        option_env!("GITHUB_SHA").unwrap_or("local"),
    );

    // OS-level hotkeys (cfg.shortcuts → channel). Kept alive for the daemon's lifetime.
    let (hotkey_tx, mut hotkey_rx) = mpsc::unbounded_channel::<HotkeyEvent>();
    let _hotkeys = match register_hotkeys(&cfg.shortcuts, hotkey_tx) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(?e, "global hotkey registration failed");
            None
        }
    };

    // PTT bootstrap target (from `sociacli ptt <friend>` CLI invocation —
    // used only for the legacy one-shot path that drives this daemon).
    if let Some(plan) = ptt {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            if let Err(e) = manager.dial(&plan.friend_id).await {
                tracing::error!(?e, "dial failed");
                return;
            }
            // give ICE + data channel time to negotiate before pushing audio
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Some(peer) = manager.peer(&plan.friend_id).await else {
                tracing::error!("no peer after dial");
                return;
            };
            manager.beep_start();
            let peer_for_tx = peer.clone();
            let res = tokio::task::spawn_blocking(move || ptt::transmit_blocking(peer_for_tx)).await;
            manager.beep_end();
            if let Ok(Err(e)) = res {
                tracing::error!(?e, "ptt transmit ended");
            }
        });
    }

    loop {
        tokio::select! {
            msg = sig.rx.recv() => {
                let Some(msg) = msg else {
                    // WS closed (kicked by a re-login or transient failure).
                    // Reconnect with backoff so incoming notifications resume.
                    // A 426 here (server raised min_client_version mid-session)
                    // parks us in the gated loop instead of looping.
                    tracing::warn!("signaling closed — reconnecting");
                    sig = match connect_with_retry(cfg).await {
                        ConnOutcome::Connected(s) => s,
                        ConnOutcome::NeedsUpdate(latest) => {
                            push_update_prompt(&overlay, true, latest);
                            return gated_loop(overlay, cmd_rx).await;
                        }
                    };
                    manager =
                        peers::PeerManager::new(sig.tx.clone(), action_tx.clone(), fx, ice.clone());
                    {
                        let mut g = state.lock().await;
                        g.manager = Some(manager.clone());
                        g.signal_tx = Some(sig.tx.clone());
                    }
                    tracing::info!("signaling reconnected");
                    continue;
                };
                match msg {
                    ServerMsg::HelloOk { me } => {
                        tracing::info!("hello: {} ({})", me.username, me.id);
                        overlay.send(OverlayEvent::new(
                            "hello",
                            serde_json::json!({ "id": me.id, "username": me.username }),
                        ));
                        // Pre-warm caches so REPL `resolve-friend` etc. has
                        // data immediately after the daemon reconnects.
                        let _ = sig.tx.send(
                            serde_json::to_string(&serde_json::json!({"t":"friend_list"}))
                                .unwrap_or_default(),
                        );
                        let _ = sig.tx.send(
                            serde_json::to_string(&serde_json::json!({"t":"perm_list"}))
                                .unwrap_or_default(),
                        );
                        // Publish our presence so friends see online vs dnd
                        // vs offline (invisible mode).
                        let presence = state.lock().await.presence.clone();
                        let wire = match presence.as_str() {
                            "" => "online",
                            other => other,
                        };
                        let _ = sig.tx.send(
                            serde_json::to_string(&serde_json::json!({
                                "t":"set_presence",
                                "state": wire,
                            }))
                            .unwrap_or_default(),
                        );
                    }
                    ServerMsg::Action { from, action, title, body, data } => {
                        // DND filter — drop the frame entirely before any UI
                        // surface sees it.
                        {
                            let g = state.lock().await;
                            if g.presence == "dnd" {
                                let blocked = match action.as_str() {
                                    "message" => g.dnd_block_message,
                                    "game_invite" => g.dnd_block_game_invite,
                                    "ptt" => g.dnd_block_ptt,
                                    "plugin" => g.dnd_block_plugin,
                                    "custom" => g.dnd_block_custom,
                                    _ => g.dnd_block_custom,
                                };
                                if blocked {
                                    tracing::info!(%action, %from, "dnd: dropped incoming action");
                                    continue;
                                }
                            }
                        }
                        let from_username = {
                            let g = state.lock().await;
                            g.friends
                                .iter()
                                .find_map(|f| {
                                    let id = f.get("id").and_then(|v| v.as_str())?;
                                    let u = f.get("username").and_then(|v| v.as_str())?;
                                    if id == from { Some(u.to_string()) } else { None }
                                })
                                .unwrap_or_else(|| from.clone())
                        };
                        println!("[{action}] {from_username}: {title} — {body}");
                        overlay.send(OverlayEvent::new("action", serde_json::json!({
                            "from": from.clone(),
                            "from_username": from_username,
                            "action": action.clone(),
                            "title": title.clone(),
                            "body": body.clone(),
                            "data": data.clone(),
                        })));
                        if action == "plugin" {
                            handle_incoming_plugin(cfg, &from, &data).await;
                        }
                    }
                    ServerMsg::Signal { from, payload } => {
                        // While in DND with PTT blocked, drop incoming WebRTC
                        // negotiation entirely — caller's offer just times out.
                        {
                            let g = state.lock().await;
                            if g.presence == "dnd" && g.dnd_block_ptt {
                                tracing::info!(%from, "dnd: dropped incoming WebRTC signal");
                                continue;
                            }
                        }
                        let manager = Arc::clone(&manager);
                        tokio::spawn(async move {
                            if let Err(e) = manager.handle_signal(&from, payload).await {
                                tracing::warn!(?e, "signal handling failed");
                            }
                        });
                    }
                    ServerMsg::Presence { friend_id, online, state: pstate } => {
                        let s = pstate.clone().unwrap_or_else(|| if online { "online".into() } else { "offline".into() });
                        tracing::info!(%friend_id, online, state = %s, "presence");
                        // Patch the cached friend, compute "did online flip?"
                        // for the popup decision, and resolve username.
                        let (was_online, want_popup, friend_username) = {
                            let mut g = state.lock().await;
                            let mut prev_online = false;
                            let mut username = friend_id.clone();
                            for f in g.friends.iter_mut() {
                                if f.get("id").and_then(|v| v.as_str()) == Some(friend_id.as_str()) {
                                    prev_online = f.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
                                    if let Some(u) = f.get("username").and_then(|v| v.as_str()) {
                                        username = u.to_string();
                                    }
                                    if let Some(obj) = f.as_object_mut() {
                                        obj.insert("online".into(), serde_json::Value::Bool(online));
                                        obj.insert("presence".into(), serde_json::Value::String(s.clone()));
                                    }
                                }
                            }
                            let popup = g.notify_friend_presence && prev_online != online;
                            (prev_online, popup, username)
                        };
                        // Re-push the friends sticky event for the overlay.
                        let friends_copy = state.lock().await.friends.clone();
                        overlay.send(OverlayEvent::new(
                            "friends",
                            serde_json::json!({ "friends": friends_copy }),
                        ));
                        overlay.send(OverlayEvent::new("presence", serde_json::json!({
                            "friend_id": friend_id, "online": online, "state": s,
                        })));
                        if want_popup {
                            let (title, body) = if online {
                                ("Friend online", format!("{friend_username} is now online"))
                            } else {
                                ("Friend offline", format!("{friend_username} went offline"))
                            };
                            overlay.send(OverlayEvent::new("action", serde_json::json!({
                                "from": friend_id,
                                "from_username": friend_username,
                                "action": "custom",
                                "title": title,
                                "body": body,
                                "data": {},
                            })));
                        }
                        let _ = was_online;
                    }
                    ServerMsg::PermChanged { by, action, allowed } => {
                        tracing::info!(%by, %action, allowed, "permission changed");
                        overlay.send(OverlayEvent::new("perm_changed", serde_json::json!({
                            "by": by, "action": action, "allowed": allowed,
                        })));
                    }
                    ServerMsg::Announcement { title, body } => {
                        tracing::info!(%title, %body, "server announcement");
                        println!("[server] {title} — {body}");
                        overlay.send(OverlayEvent::new("action", serde_json::json!({
                            "from": "server",
                            "from_username": "server",
                            "action": "custom",
                            "title": format!("Server: {title}"),
                            "body": body,
                            "data": {},
                        })));
                    }
                    ServerMsg::Friends { friends } => {
                        tracing::info!(count = friends.len(), "friend list updated");
                        // Cache for REPL resolve / friend-list queries.
                        let serialized: Vec<serde_json::Value> = friends
                            .iter()
                            .filter_map(|f| serde_json::to_value(f).ok())
                            .collect();
                        // Detect freshly-arrived incoming requests: a friend with
                        // status `pending_in` that wasn't `pending_in` before. On
                        // the first frame after connect we only seed the cache (no
                        // cards) so pre-existing requests don't all pop at once.
                        let (waiters, new_requests): (Vec<_>, Vec<serde_json::Value>) = {
                            let mut g = state.lock().await;
                            let was_pending: std::collections::HashSet<String> = g
                                .friends
                                .iter()
                                .filter(|f| f.get("status").and_then(|v| v.as_str()) == Some("pending_in"))
                                .filter_map(|f| f.get("id").and_then(|v| v.as_str()).map(String::from))
                                .collect();
                            let fresh: Vec<serde_json::Value> = if g.friends_seen && g.notify_friend_requests {
                                serialized
                                    .iter()
                                    .filter(|f| f.get("status").and_then(|v| v.as_str()) == Some("pending_in"))
                                    .filter(|f| {
                                        f.get("id")
                                            .and_then(|v| v.as_str())
                                            .map(|id| !was_pending.contains(id))
                                            .unwrap_or(false)
                                    })
                                    .cloned()
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            g.friends = serialized.clone();
                            g.friends_seen = true;
                            (g.pending_friends.drain(..).collect(), fresh)
                        };
                        // Resolve everyone who was waiting on this frame.
                        for tx in waiters {
                            let _ = tx.send(serialized.clone());
                        }
                        // Pop an Accept/Decline overlay card per new request.
                        for f in &new_requests {
                            let uname = f.get("username").and_then(|v| v.as_str()).unwrap_or("?");
                            let uid = f.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            println!("[friend request] {uname} wants to add you — `friend accept {uname}`");
                            overlay.send(OverlayEvent::new(
                                "friend_request",
                                serde_json::json!({ "username": uname, "id": uid }),
                            ));
                        }
                        // Sticky push so the overlay can resolve user_id → username.
                        overlay.send(OverlayEvent::new(
                            "friends",
                            serde_json::json!({ "friends": serialized }),
                        ));
                        // Now that the cache is fresh, warm the default PTT
                        // friend's peer so a hotkey HOLD transmits instantly
                        // (no cold dial racing the key release).
                        let st = state.clone();
                        tokio::spawn(async move { maybe_prewarm_ptt(&st).await; });
                    }
                    ServerMsg::Perms { perms } => {
                        let serialized: Vec<serde_json::Value> = perms
                            .iter()
                            .filter_map(|p| serde_json::to_value(p).ok())
                            .collect();
                        let waiters: Vec<_> = {
                            let mut g = state.lock().await;
                            g.perms = serialized.clone();
                            g.pending_perms.drain(..).collect()
                        };
                        for tx in waiters {
                            let _ = tx.send(serialized.clone());
                        }
                    }
                    // Error handler is below in the wildcard, but we also
                    // pop a pending ack so the REPL caller sees the cause.
                    // (Done inline rather than here so we keep the existing
                    // tracing::error! call too.)
                    ServerMsg::Error { message } => {
                        tracing::error!(%message, "server error");
                        // Resolve the oldest pending IPC ack with this error.
                        let waiter = state.lock().await.pending_acks.pop_front();
                        if let Some(tx) = waiter {
                            let _ = tx.send(message);
                        }
                    }
                }
            }
            Some(act) = action_rx.recv() => {
                println!("[p2p] {}: {}", act.from, act.frame);
                overlay.send(OverlayEvent::new("action", serde_json::json!({
                    "from": act.from,
                    "action": act.frame.get("action").cloned().unwrap_or(serde_json::Value::String("custom".into())),
                    "title": act.frame.get("title").cloned().unwrap_or(serde_json::Value::String("".into())),
                    "body": act.frame.get("body").cloned().unwrap_or(serde_json::Value::String("".into())),
                    "data": act.frame.get("data").cloned().unwrap_or(serde_json::json!({})),
                })));
            }
            Some(cmd) = cmd_rx.recv() => {
                let kind = cmd.req.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
                if kind == "self-update" {
                    // Self-update must work even while disconnected (a mandatory
                    // 426 update is exactly the disconnected case), so it doesn't
                    // need signal_tx — handle it directly with the overlay handle.
                    let ov = overlay.clone();
                    let reply = cmd.reply.clone();
                    tokio::spawn(async move { handle_self_update(&ov, &reply).await; });
                } else {
                    // Spawn so this loop stays free to process the server's
                    // response — handle_ipc_cmd awaits a one-shot ack that's
                    // resolved by *this* loop's ServerMsg::Error arm.
                    let st = state.clone();
                    tokio::spawn(async move {
                        // Fetch the *current* outbound signaling channel: if
                        // we'd captured `sig.tx` at spawn time, a reconnect
                        // between spawns would leave us writing into a dead
                        // sender.
                        let tx = st.lock().await.signal_tx.clone();
                        if let Some(tx) = tx {
                            handle_ipc_cmd(&tx, &st, cmd).await;
                        }
                    });
                }
            }
            Some(ev) = hotkey_rx.recv() => {
                match ev.action.as_str() {
                    "open" if ev.pressed => {
                        overlay.send(OverlayEvent::new("focus", serde_json::json!({})));
                        if let Err(e) = open_self_window() {
                            tracing::warn!(?e, "open hotkey: launch failed");
                        }
                    }
                    "ptt" => {
                        // Reuse the IPC path — exact same start/stop logic.
                        let kind = if ev.pressed { "ptt-start" } else { "ptt-stop" };
                        let (dummy_tx, _) = mpsc::unbounded_channel::<String>();
                        let fake_cmd = overlay_ipc::IpcCmd {
                            req: serde_json::json!({"cmd": kind, "id": ""}),
                            reply: dummy_tx,
                        };
                        handle_ipc_cmd(&sig.tx, &state, fake_cmd).await;
                    }
                    other => tracing::debug!(%other, pressed = ev.pressed, "hotkey: ignored"),
                }
            }
        }
    }
    tracing::warn!("signaling stream closed");
    Ok(())
}

/// Hotkey event delivered to the daemon's main loop. `pressed = true` on key
/// down, `false` on key up — required for hold-to-talk PTT.
#[derive(Debug, Clone)]
struct HotkeyEvent {
    action: String,
    pressed: bool,
}

/// Parse + register every `cfg.shortcuts` entry. The returned `GlobalHotKeyManager`
/// must be held for the lifetime of the daemon — dropping it un-registers everything.
fn register_hotkeys(
    shortcuts: &std::collections::BTreeMap<String, String>,
    tx: mpsc::UnboundedSender<HotkeyEvent>,
) -> Result<global_hotkey::GlobalHotKeyManager> {
    use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager};
    let mgr = GlobalHotKeyManager::new().context("init global hotkey manager")?;
    let mut id_to_action: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for (action, combo) in shortcuts {
        let hk: HotKey = combo
            .parse()
            .map_err(|e| anyhow::anyhow!("bad combo `{combo}` for `{action}`: {e}"))?;
        mgr.register(hk).context("register hotkey")?;
        id_to_action.insert(hk.id(), action.clone());
        tracing::info!(%action, %combo, id = hk.id(), "hotkey registered");
    }
    std::thread::Builder::new()
        .name("sociacli-hotkeys".into())
        .spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            for ev in rx {
                if let Some(action) = id_to_action.get(&ev.id) {
                    let pressed = ev.state == global_hotkey::HotKeyState::Pressed;
                    let _ = tx.send(HotkeyEvent {
                        action: action.clone(),
                        pressed,
                    });
                }
            }
        })
        .ok();
    Ok(mgr)
}

/// Best-effort: start the Tauri overlay window if its binary is alongside
/// `sociacli(.exe)`. No-op if the binary isn't installed (e.g. CLI-only build)
/// or if the spawn fails — the daemon still works headless.
fn spawn_overlay_if_present() {
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let candidate = if cfg!(windows) {
        dir.join("sociacli-overlay.exe")
    } else {
        dir.join("sociacli-overlay")
    };
    if !candidate.exists() {
        tracing::debug!("overlay binary not found at {}", candidate.display());
        return;
    }
    let mut cmd = std::process::Command::new(&candidate);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    match cmd.spawn() {
        Ok(_) => tracing::info!("overlay window spawned"),
        Err(e) => tracing::warn!(?e, "overlay spawn failed"),
    }
}

/// Probe the overlay IPC port. `true` if some sociacli process already owns it.
fn daemon_is_running() -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr: SocketAddr = ([127, 0, 0, 1], overlay_ipc::DEFAULT_PORT).into();
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(60)).is_ok()
}

/// If no daemon is up, spawn `sociacli listen` as a detached background
/// process so notifications, PTT signaling, and global hotkeys work without
/// the user lifting a finger. No-op if a daemon already owns the IPC port.
fn ensure_daemon_running() {
    if daemon_is_running() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("listen")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW — no console, no flicker, fully
        // independent from this REPL.
        cmd.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    match cmd.spawn() {
        Ok(_) => tracing::info!("started background sociacli listen daemon"),
        Err(e) => tracing::warn!(?e, "failed to spawn background daemon"),
    }
}

/// Connect to the local daemon's overlay IPC and mirror incoming `action`
/// events into the REPL's stdout so the user sees friend messages, game
/// invites, etc. without having to leave the prompt. Reconnects on disconnect.
fn spawn_repl_action_tap() {
    tokio::spawn(async move {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let url = format!("ws://127.0.0.1:{}", overlay_ipc::DEFAULT_PORT);
        loop {
            // Give the freshly spawned daemon a beat to bind.
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let ws = match tokio_tungstenite::connect_async(&url).await {
                Ok((w, _)) => w,
                Err(_) => continue, // daemon not up yet → retry
            };
            let (_w, mut r) = ws.split();
            while let Some(Ok(msg)) = r.next().await {
                let Message::Text(t) = msg else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
                    continue;
                };
                if v.get("kind").and_then(|k| k.as_str()) != Some("action") {
                    continue;
                }
                let p = &v["payload"];
                println!(
                    "\n[{}] {}: {} — {}",
                    p["action"].as_str().unwrap_or("?"),
                    p["from"].as_str().unwrap_or("?"),
                    p["title"].as_str().unwrap_or(""),
                    p["body"].as_str().unwrap_or(""),
                );
            }
            // disconnected → loop reconnects
        }
    });
}

/// Best-effort "bring the GUI to the front". This fires from inside the
/// daemon's hotkey thread, so a `listen` is already running — spawning a
/// second one would just fail to bind the IPC port. The overlay focus event
/// has already been emitted by the caller; here we only spawn a fresh REPL
/// terminal so the user gets something to type into, and only if no other
/// sociacli process is currently sharing this user's overlay IPC port.
fn open_self_window() -> Result<()> {
    use std::net::{SocketAddr, TcpStream};
    // ~50 ms probe — if another sociacli already owns the overlay port the
    // user-facing window is up, so we skip the (slow) process spawn.
    let addr: SocketAddr = ([127, 0, 0, 1], overlay_ipc::DEFAULT_PORT).into();
    if TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(50)).is_ok() {
        // Daemon's own listener answers this — but that's also the signal
        // that the overlay is reachable. Caller already pushed a `focus`
        // event; nothing more to do.
        tracing::debug!("open hotkey: daemon up, focus event dispatched");
        return Ok(());
    }
    let exe = std::env::current_exe().context("current_exe")?;
    std::process::Command::new(exe)
        .arg("listen")
        .spawn()
        .map(|_| ())
        .context("spawn sociacli listen")
}
