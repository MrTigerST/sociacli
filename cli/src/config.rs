use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default = "default_server")]
    pub server_url: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub token: Option<String>,
    /// Path to user-picked notification sound (wav/ogg/mp3).
    pub notify_sound: Option<PathBuf>,
    /// Background check for new releases on REPL/daemon start.
    #[serde(default = "default_auto_update")]
    pub auto_update_check: bool,
    /// Unix-seconds timestamp of the last successful update check.
    #[serde(default)]
    pub last_update_check: Option<u64>,
    /// Override the plugins directory. Default: `<config-dir>/plugins`.
    #[serde(default)]
    pub plugins_dir: Option<PathBuf>,
    /// Per-plugin per-friend authorization. `plugin_trust[plugin_id][friend_id] = true`
    /// means incoming `plugin` actions from that friend for that plugin id may
    /// run without prompting.
    #[serde(default)]
    pub plugin_trust: std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    /// Apply a 300–3400 Hz band-pass to incoming PTT audio (the "radio" colour).
    #[serde(default = "default_true")]
    pub radio_effect: bool,
    /// Play short sine beeps at the start and end of a PTT transmission.
    #[serde(default = "default_true")]
    pub radio_beeps: bool,
    /// WebRTC ICE servers for PTT. STUN punches most NATs; add a TURN entry
    /// (with `username`/`credential`) to RELAY audio for peers behind symmetric
    /// NAT / CGNAT that STUN alone can't connect — that's the usual reason PTT
    /// works for some friends but not others.
    #[serde(default = "default_ice_servers")]
    pub ice_servers: Vec<IceServer>,
    /// Global hotkeys. Action → key combo (e.g. `"open" -> "Ctrl+Alt+Shift+KeyF"`).
    /// Registered by the `listen` daemon. Supported actions: `open`, `ptt`.
    #[serde(default = "default_shortcuts")]
    pub shortcuts: std::collections::BTreeMap<String, String>,
    /// Master switch for the background daemon: receiving messages / PTT /
    /// global hotkeys / overlay notifications. When `false`, the REPL will
    /// NOT auto-spawn a `listen` process and `sociacli service install` is
    /// skipped on first run.
    #[serde(default = "default_true")]
    pub background_enabled: bool,
    /// Set once `sociacli service install` succeeds — used by the REPL to
    /// avoid prompting / re-running the setup on every launch.
    #[serde(default)]
    pub autostart_installed: bool,
    /// Friend the PTT hotkey opens a session with when no recent PTT target
    /// is known. Set on first run; can be changed via
    /// `sociacli ptt-friend <name>`.
    #[serde(default)]
    pub default_ptt_friend: Option<String>,
    /// True once the first-run wizard has prompted for hotkeys / PTT friend.
    #[serde(default)]
    pub first_run_done: bool,
    /// If true, overlay notification cards fade out after
    /// `notify_auto_dismiss_ms`. If false (default), cards stay until the
    /// user dismisses them with the close button.
    #[serde(default)]
    pub notify_auto_dismiss: bool,
    /// Milliseconds before auto-dismiss (only used when `notify_auto_dismiss`
    /// is true).
    #[serde(default = "default_dismiss_ms")]
    pub notify_auto_dismiss_ms: u32,
    /// If true (default), the overlay accumulates every received card in a
    /// scrollable stack. If false, only the most recent card is shown.
    #[serde(default = "default_true")]
    pub notify_show_list: bool,
    /// Preferred cpal input device name for PTT. `None` → use the OS
    /// default. Listed via `sociacli mic list`, set via `sociacli mic set`.
    #[serde(default)]
    pub ptt_input_device: Option<String>,
    /// User-facing presence state. `"online"` (default) or `"dnd"`. Offline
    /// is implicit — set automatically by the server when no daemon is
    /// connected. Change with `sociacli presence online|dnd`.
    #[serde(default = "default_presence")]
    pub presence: String,
    /// Per-action DND filter. When `presence == "dnd"` and the matching flag
    /// is `true` (default for all), incoming actions of that kind are
    /// dropped on the receiver before they reach the overlay / REPL.
    #[serde(default = "default_true")]
    pub dnd_block_message: bool,
    #[serde(default = "default_true")]
    pub dnd_block_game_invite: bool,
    #[serde(default = "default_true")]
    pub dnd_block_ptt: bool,
    #[serde(default = "default_true")]
    pub dnd_block_plugin: bool,
    #[serde(default = "default_true")]
    pub dnd_block_custom: bool,
    /// Render an overlay card when a friend's presence transitions (offline
    /// → online or online → offline). Off by default — the friend list still
    /// updates either way; this only controls the popup.
    #[serde(default)]
    pub notify_friend_presence: bool,
    /// Render an overlay card with Accept / Decline buttons when a new incoming
    /// friend request arrives. On by default.
    #[serde(default = "default_true")]
    pub notify_friend_requests: bool,
}

fn default_presence() -> String {
    "online".to_string()
}

fn default_dismiss_ms() -> u32 {
    6000
}

fn default_true() -> bool {
    true
}

fn default_shortcuts() -> std::collections::BTreeMap<String, String> {
    let mut m = std::collections::BTreeMap::new();
    m.insert("open".to_string(), "Ctrl+Alt+Shift+KeyF".to_string());
    // Push-to-talk: hold to transmit, release to stop. Space is risky (steals
    // focus everywhere), KeyK with a 3-mod stack is a safe default that no
    // other Windows app claims.
    m.insert("ptt".to_string(), "Ctrl+Alt+Shift+KeyK".to_string());
    m
}

fn default_server() -> String {
    "https://sociacli-server.mrtigerst.dev".to_string()
}

fn default_auto_update() -> bool {
    true
}

/// One WebRTC ICE server. `urls` like `stun:host:3478` or `turn:host:3478`.
/// TURN entries also need `username` + `credential`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

fn default_ice_servers() -> Vec<IceServer> {
    // Public STUN only. STUN can't traverse symmetric NAT / CGNAT — for friends
    // on those networks the operator must add a TURN relay entry here.
    vec![IceServer {
        urls: vec![
            "stun:stun.l.google.com:19302".to_string(),
            "stun:stun1.l.google.com:19302".to_string(),
        ],
        username: None,
        credential: None,
    }]
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("dev", "sociacli", "sociacli")
            .context("cannot resolve project dirs")?;
        let dir = dirs.config_dir().to_path_buf();
        fs::create_dir_all(&dir).ok();
        Ok(dir.join("config.json"))
    }

    pub fn load() -> Result<Self> {
        let p = Self::path()?;
        if !p.exists() {
            return Ok(Self {
                server_url: default_server(),
                radio_effect: true,
                radio_beeps: true,
                ice_servers: default_ice_servers(),
                auto_update_check: true,
                shortcuts: default_shortcuts(),
                background_enabled: true,
                notify_auto_dismiss: false,
                notify_auto_dismiss_ms: default_dismiss_ms(),
                notify_show_list: true,
                ..Default::default()
            });
        }
        let raw = fs::read_to_string(&p)?;
        let cfg: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parse config at {}", p.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let p = Self::path()?;
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(&p, raw)?;
        Ok(())
    }

    pub fn plugins_dir(&self) -> PathBuf {
        if let Some(p) = &self.plugins_dir {
            return p.clone();
        }
        Self::path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("plugins")))
            .unwrap_or_else(|| PathBuf::from("plugins"))
    }

    pub fn is_plugin_trusted(&self, plugin_id: &str, friend_id: &str) -> bool {
        self.plugin_trust
            .get(plugin_id)
            .and_then(|m| m.get(friend_id).copied())
            .unwrap_or(false)
    }

    pub fn ws_url(&self) -> Result<String> {
        let mut u = url::Url::parse(&self.server_url)?;
        let scheme = match u.scheme() {
            "https" => "wss",
            _ => "ws",
        };
        u.set_scheme(scheme).map_err(|_| anyhow::anyhow!("bad scheme"))?;
        u.set_path("/ws");
        let token = self
            .token
            .as_deref()
            .context("not logged in — run `sociacli login`")?;
        u.query_pairs_mut()
            .clear()
            .append_pair("token", token)
            .append_pair("version", env!("CARGO_PKG_VERSION"));
        Ok(u.to_string())
    }
}
