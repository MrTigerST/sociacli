use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Message,
    GameInvite,
    Ptt,
    Custom,
    Plugin,
}

impl ActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::Message => "message",
            ActionKind::GameInvite => "game_invite",
            ActionKind::Ptt => "ptt",
            ActionKind::Custom => "custom",
            ActionKind::Plugin => "plugin",
        }
    }
}

// ---- client -> server ----
#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg<'a> {
    FriendAdd {
        username: &'a str,
    },
    FriendAccept {
        username: &'a str,
    },
    FriendRemove {
        username: &'a str,
    },
    FriendList,
    PermUpdate {
        friend_id: &'a str,
        action: ActionKind,
        allowed: bool,
    },
    PermList,
    Signal {
        to: &'a str,
        payload: serde_json::Value,
    },
    SendAction {
        to: &'a str,
        action: ActionKind,
        title: &'a str,
        body: &'a str,
        data: serde_json::Value,
    },
    SetPresence {
        /// `"online"` | `"dnd"` | `"offline"`. `"offline"` is "invisible
        /// mode" — daemon stays connected, friends see you as offline.
        state: &'a str,
    },
}

// ---- server -> client ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friend {
    pub id: String,
    pub username: String,
    pub status: String,
    pub online: bool,
    /// Self-reported presence: `"online"` (default), `"dnd"`, or absent /
    /// `"offline"` when the friend's daemon isn't connected.
    #[serde(default)]
    pub presence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Perm {
    pub friend_id: String,
    pub action: String,
    pub allowed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    HelloOk {
        me: Me,
    },
    Error {
        message: String,
    },
    Friends {
        friends: Vec<Friend>,
    },
    Perms {
        perms: Vec<Perm>,
    },
    PermChanged {
        by: String,
        action: String,
        allowed: bool,
    },
    Presence {
        friend_id: String,
        online: bool,
        /// `"online"` | `"dnd"` | `"offline"`. Older servers may omit it.
        #[serde(default)]
        state: Option<String>,
    },
    Signal {
        from: String,
        payload: serde_json::Value,
    },
    Action {
        from: String,
        action: String,
        title: String,
        body: String,
        #[serde(default)]
        data: serde_json::Value,
    },
    /// Operator-set announcement pushed to every client on connect when the
    /// server is in `mode=message`. Rendered as a sticky overlay card.
    Announcement {
        title: String,
        body: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct Me {
    pub id: String,
    pub username: String,
}
