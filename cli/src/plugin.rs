// Lua plugin SDK.
//
// Layout:
//   <config-dir>/plugins/<plugin-id>/
//     plugin.toml      -- manifest
//     main.lua         -- entry script
//     .state.json      -- created on demand by sociacli.store_set
//     ...              -- arbitrary extra files (read via sociacli.read_file)
//
// SDK exposed under the global `sociacli` (see README "Lua plugins" for the
// full surface). Filesystem access is sandboxed to the plugin's own
// directory; HTTP and `send` require the daemon (they no-op in local
// `plugin run`).

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::overlay_ipc::{OverlayEvent, OverlayIpc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_entry() -> String {
    "main.lua".to_string()
}

#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

pub fn list(cfg: &Config) -> Result<Vec<Plugin>> {
    let dir = cfg.plugins_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        let manifest_path = p.join("plugin.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let m: Manifest = toml::from_str(&raw)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        out.push(Plugin {
            manifest: m,
            dir: p,
        });
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(out)
}

pub fn find(cfg: &Config, id: &str) -> Result<Plugin> {
    list(cfg)?
        .into_iter()
        .find(|p| p.manifest.id == id)
        .ok_or_else(|| anyhow!("no plugin with id `{id}` installed"))
}

pub fn install(cfg: &Config, src: &Path) -> Result<Plugin> {
    let manifest_path = src.join("plugin.toml");
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("missing plugin.toml in {}", src.display()))?;
    let m: Manifest = toml::from_str(&raw).context("parse manifest")?;
    let dest = cfg.plugins_dir().join(&m.id);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir(src, &dest)?;
    Ok(Plugin {
        manifest: m,
        dir: dest,
    })
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let p = entry.path();
        let target = dst.join(entry.file_name());
        if p.is_dir() {
            copy_dir(&p, &target)?;
        } else {
            fs::copy(&p, &target)?;
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct RunCtx {
    pub me: String,
    pub username: Option<String>,
    pub server_url: String,
    pub from: Option<String>,
    pub args: serde_json::Value,
    pub plugin: Plugin,
    /// Outbound signaling channel. Present only inside the daemon — local
    /// `plugin run` invocations leave it as None and the SDK's network-y
    /// functions raise a Lua error.
    pub signal_tx: Option<mpsc::UnboundedSender<String>>,
    pub overlay: Option<OverlayIpc>,
}

pub fn run(ctx: RunCtx) -> Result<()> {
    let stdlib = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8;
    let lua = Lua::new_with(stdlib, LuaOptions::default())?;

    let sdk = build_sdk(&lua, &ctx)?;
    lua.globals().set("sociacli", sdk)?;

    let main = ctx.plugin.dir.join(&ctx.plugin.manifest.entry);
    let code = fs::read_to_string(&main)
        .with_context(|| format!("read entry {}", main.display()))?;
    lua.load(&code)
        .set_name(format!("plugin:{}", ctx.plugin.manifest.id))
        .exec()
        .with_context(|| format!("exec {}", main.display()))?;

    let args_lua = json_to_lua(&lua, &ctx.args)?;

    if let Some(f) = lua.globals().get::<Option<mlua::Function>>("run")? {
        f.call::<()>(args_lua.clone())
            .with_context(|| "plugin `run` raised")?;
    }
    if ctx.from.is_some() {
        if let Some(f) = lua.globals().get::<Option<mlua::Function>>("on_message")? {
            let ctx_tbl = lua.create_table()?;
            ctx_tbl.set("from", ctx.from.as_deref().unwrap_or(""))?;
            ctx_tbl.set("me", ctx.me.as_str())?;
            f.call::<()>((ctx_tbl, args_lua))
                .with_context(|| "plugin `on_message` raised")?;
        }
    }
    Ok(())
}

// =========================================================================
// SDK
// =========================================================================

fn build_sdk(lua: &Lua, ctx: &RunCtx) -> Result<Table> {
    let t = lua.create_table()?;

    // ---- identity ----
    t.set("me", ctx.me.as_str())?;
    if let Some(u) = &ctx.username {
        t.set("username", u.as_str())?;
    }
    if let Some(from) = &ctx.from {
        t.set("from", from.as_str())?;
    }

    // ---- manifest snapshot ----
    let pl = lua.create_table()?;
    pl.set("id", ctx.plugin.manifest.id.as_str())?;
    pl.set("name", ctx.plugin.manifest.name.as_str())?;
    pl.set("version", ctx.plugin.manifest.version.as_str())?;
    pl.set("author", ctx.plugin.manifest.author.as_str())?;
    pl.set("description", ctx.plugin.manifest.description.as_str())?;
    let caps = lua.create_table()?;
    for (i, c) in ctx.plugin.manifest.capabilities.iter().enumerate() {
        caps.raw_set(i + 1, c.as_str())?;
    }
    pl.set("capabilities", caps)?;
    pl.set("dir", ctx.plugin.dir.display().to_string())?;
    t.set("plugin", pl)?;

    // ---- config snapshot (read-only) ----
    let cfg_tbl = lua.create_table()?;
    cfg_tbl.set("server_url", ctx.server_url.as_str())?;
    if let Some(u) = &ctx.username {
        cfg_tbl.set("username", u.as_str())?;
    }
    t.set("config", cfg_tbl)?;

    // ---- logging ----
    t.set(
        "log",
        lua.create_function(|_, (level, msg): (String, String)| {
            match level.as_str() {
                "error" => tracing::error!(target: "plugin", "{msg}"),
                "warn" => tracing::warn!(target: "plugin", "{msg}"),
                "debug" => tracing::debug!(target: "plugin", "{msg}"),
                _ => tracing::info!(target: "plugin", "{msg}"),
            }
            Ok(())
        })?,
    )?;

    // ---- notify (overlay if available; stdout fallback) ----
    let overlay = ctx.overlay.clone();
    let plugin_id_n = ctx.plugin.manifest.id.clone();
    let me_n = ctx.me.clone();
    t.set(
        "notify",
        lua.create_function(move |_, (title, body): (String, String)| {
            if let Some(o) = overlay.as_ref() {
                o.send(OverlayEvent::new(
                    "action",
                    serde_json::json!({
                        "from": me_n,
                        "action": "plugin",
                        "title": title,
                        "body": body,
                        "data": { "plugin": { "id": plugin_id_n } },
                    }),
                ));
            } else {
                println!("[plugin notify] {title}: {body}");
            }
            Ok(())
        })?,
    )?;

    // ---- send: plugin-to-plugin over server relay ----
    let tx = ctx.signal_tx.clone();
    let envelope_plugin = ctx.plugin.clone();
    t.set(
        "send",
        lua.create_function(move |_lua, (to, args): (String, mlua::Value)| {
            let Some(tx) = tx.as_ref() else {
                return Err(mlua::Error::external(
                    "sociacli.send needs the daemon — run via on_message or inside `sociacli listen`",
                ));
            };
            let args_json = lua_to_json(args).map_err(mlua::Error::external)?;
            let env = payload_envelope(&envelope_plugin, args_json);
            push_action(tx, &to, "plugin", &envelope_plugin.manifest.name, &envelope_plugin.manifest.description, env)?;
            Ok(())
        })?,
    )?;

    // ---- message / invite / generic action ----
    let tx = ctx.signal_tx.clone();
    t.set(
        "message",
        lua.create_function(move |_, (to, title, body): (String, String, String)| {
            let Some(tx) = tx.as_ref() else {
                return Err(mlua::Error::external("sociacli.message needs the daemon"));
            };
            push_action(tx, &to, "message", &title, &body, serde_json::json!({}))?;
            Ok(())
        })?,
    )?;
    let tx = ctx.signal_tx.clone();
    t.set(
        "invite",
        lua.create_function(move |_, (to, url, title): (String, String, Option<String>)| {
            let Some(tx) = tx.as_ref() else {
                return Err(mlua::Error::external("sociacli.invite needs the daemon"));
            };
            let title = title.unwrap_or_else(|| "Game invite".to_string());
            push_action(
                tx,
                &to,
                "game_invite",
                &title,
                &url,
                serde_json::json!({ "url": url }),
            )?;
            Ok(())
        })?,
    )?;
    let tx = ctx.signal_tx.clone();
    t.set(
        "action",
        lua.create_function(
            move |_, (to, kind, title, body, data): (String, String, String, String, Option<mlua::Value>)| {
                let Some(tx) = tx.as_ref() else {
                    return Err(mlua::Error::external("sociacli.action needs the daemon"));
                };
                let data_json = match data {
                    Some(v) => lua_to_json(v).map_err(mlua::Error::external)?,
                    None => serde_json::json!({}),
                };
                push_action(tx, &to, &kind, &title, &body, data_json)?;
                Ok(())
            },
        )?,
    )?;

    // ---- KV store (per-plugin, file-backed) ----
    let store_path = ctx.plugin.dir.join(".state.json");
    let sp = store_path.clone();
    t.set(
        "store_get",
        lua.create_function(move |lua, key: String| {
            let map = load_store(&sp).unwrap_or_default();
            match map.get(&key) {
                Some(v) => Ok(json_to_lua(lua, v).map_err(mlua::Error::external)?),
                None => Ok(mlua::Value::Nil),
            }
        })?,
    )?;
    let sp = store_path.clone();
    t.set(
        "store_set",
        lua.create_function(move |_, (key, value): (String, mlua::Value)| {
            let mut map = load_store(&sp).unwrap_or_default();
            let v = lua_to_json(value).map_err(mlua::Error::external)?;
            map.insert(key, v);
            save_store(&sp, &map).map_err(mlua::Error::external)?;
            Ok(())
        })?,
    )?;
    let sp = store_path.clone();
    t.set(
        "store_delete",
        lua.create_function(move |_, key: String| {
            let mut map = load_store(&sp).unwrap_or_default();
            map.remove(&key);
            save_store(&sp, &map).map_err(mlua::Error::external)?;
            Ok(())
        })?,
    )?;

    // ---- file I/O (sandboxed to plugin.dir) ----
    let root = ctx.plugin.dir.clone();
    t.set(
        "read_file",
        lua.create_function(move |_, rel: String| {
            let p = sandbox(&root, &rel).map_err(mlua::Error::external)?;
            fs::read_to_string(&p).map_err(|e| mlua::Error::external(format!("read {}: {e}", p.display())))
        })?,
    )?;
    let root = ctx.plugin.dir.clone();
    t.set(
        "write_file",
        lua.create_function(move |_, (rel, contents): (String, String)| {
            let p = sandbox(&root, &rel).map_err(mlua::Error::external)?;
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).map_err(mlua::Error::external)?;
            }
            fs::write(&p, contents).map_err(|e| mlua::Error::external(format!("write {}: {e}", p.display())))?;
            Ok(())
        })?,
    )?;

    // ---- HTTP (blocking) ----
    let http_client = Arc::new(
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(format!("sociacli-plugin/{}", ctx.plugin.manifest.id))
            .build()
            .map_err(|e| anyhow!("build http client: {e}"))?,
    );
    let c = http_client.clone();
    t.set(
        "http_get",
        lua.create_function(move |lua, url: String| {
            let resp = c
                .get(&url)
                .send()
                .map_err(|e| mlua::Error::external(format!("GET {url}: {e}")))?;
            http_resp_to_lua(lua, resp)
        })?,
    )?;
    let c = http_client.clone();
    t.set(
        "http_post",
        lua.create_function(move |lua, (url, body): (String, Option<mlua::Value>)| {
            let req = c.post(&url);
            let req = match body {
                Some(mlua::Value::String(s)) => req
                    .header("content-type", "text/plain")
                    .body(s.to_str()?.to_string()),
                Some(v) => {
                    let j = lua_to_json(v).map_err(mlua::Error::external)?;
                    req.json(&j)
                }
                None => req,
            };
            let resp = req
                .send()
                .map_err(|e| mlua::Error::external(format!("POST {url}: {e}")))?;
            http_resp_to_lua(lua, resp)
        })?,
    )?;

    // ---- crypto / encoding ----
    t.set(
        "sha256",
        lua.create_function(|_, s: mlua::String| {
            let mut h = Sha256::new();
            h.update(s.as_bytes().as_ref());
            Ok(hex::encode(h.finalize()))
        })?,
    )?;
    t.set(
        "base64_encode",
        lua.create_function(|_, s: mlua::String| {
            Ok(base64::engine::general_purpose::STANDARD.encode(s.as_bytes().as_ref()))
        })?,
    )?;
    t.set(
        "base64_decode",
        lua.create_function(|_, s: String| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(s.as_bytes())
                .map_err(mlua::Error::external)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })?,
    )?;

    // ---- JSON helpers ----
    t.set(
        "json_encode",
        lua.create_function(|_, v: mlua::Value| {
            let j = lua_to_json(v).map_err(mlua::Error::external)?;
            serde_json::to_string(&j).map_err(mlua::Error::external)
        })?,
    )?;
    t.set(
        "json_decode",
        lua.create_function(|lua, s: String| {
            let v: serde_json::Value = serde_json::from_str(&s).map_err(mlua::Error::external)?;
            json_to_lua(lua, &v).map_err(mlua::Error::external)
        })?,
    )?;

    // ---- misc ----
    t.set(
        "uuid",
        lua.create_function(|_, ()| Ok(uuid::Uuid::new_v4().to_string()))?,
    )?;
    t.set(
        "now",
        lua.create_function(|_, ()| {
            Ok(SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0))
        })?,
    )?;
    t.set(
        "sleep",
        lua.create_function(|_, ms: u64| {
            std::thread::sleep(Duration::from_millis(ms));
            Ok(())
        })?,
    )?;

    Ok(t)
}

fn push_action(
    tx: &mpsc::UnboundedSender<String>,
    to: &str,
    kind: &str,
    title: &str,
    body: &str,
    data: serde_json::Value,
) -> mlua::Result<()> {
    let frame = serde_json::json!({
        "t": "send_action",
        "to": to,
        "action": kind,
        "title": title,
        "body": body,
        "data": data,
    });
    let s = serde_json::to_string(&frame).map_err(mlua::Error::external)?;
    tx.send(s).map_err(|e| mlua::Error::external(e.to_string()))
}

fn sandbox(root: &Path, rel: &str) -> Result<PathBuf> {
    let p = root.join(rel);
    let p = p
        .canonicalize()
        .or_else(|_| {
            // permit writes to not-yet-existing files: canonicalize the parent
            let parent = p.parent().ok_or_else(|| anyhow!("no parent for {}", p.display()))?;
            Ok::<PathBuf, anyhow::Error>(parent.canonicalize()?.join(
                p.file_name().ok_or_else(|| anyhow!("no file name"))?,
            ))
        })?;
    let root_c = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !p.starts_with(&root_c) {
        return Err(anyhow!(
            "path `{}` escapes plugin sandbox `{}`",
            p.display(),
            root_c.display()
        ));
    }
    Ok(p)
}

fn load_store(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>> {
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let raw = fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    Ok(v.as_object().cloned().unwrap_or_default())
}

fn save_store(path: &Path, map: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let raw = serde_json::to_string_pretty(&serde_json::Value::Object(map.clone()))?;
    fs::write(path, raw)?;
    Ok(())
}

fn http_resp_to_lua(lua: &Lua, resp: reqwest::blocking::Response) -> mlua::Result<mlua::Table> {
    let status = resp.status().as_u16();
    let headers = lua.create_table()?;
    for (k, v) in resp.headers().iter() {
        if let Ok(vs) = v.to_str() {
            headers.set(k.as_str(), vs)?;
        }
    }
    let body = resp.text().unwrap_or_default();
    let t = lua.create_table()?;
    t.set("status", status)?;
    t.set("headers", headers)?;
    t.set("body", body)?;
    Ok(t)
}

fn json_to_lua(lua: &Lua, v: &serde_json::Value) -> Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Number(f)
            } else {
                Value::Nil
            }
        }
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(a) => {
            let t = lua.create_table()?;
            for (i, item) in a.iter().enumerate() {
                t.raw_set(i + 1, json_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
        J::Object(map) => {
            let t = lua.create_table()?;
            for (k, val) in map {
                t.set(k.as_str(), json_to_lua(lua, val)?)?;
            }
            Value::Table(t)
        }
    })
}

fn lua_to_json(v: mlua::Value) -> Result<serde_json::Value> {
    use mlua::Value as V;
    Ok(match v {
        V::Nil => serde_json::Value::Null,
        V::Boolean(b) => serde_json::Value::Bool(b),
        V::Integer(i) => serde_json::json!(i),
        V::Number(n) => serde_json::json!(n),
        V::String(s) => serde_json::Value::String(s.to_str()?.to_string()),
        V::Table(t) => {
            let len = t.raw_len();
            let mut is_array = len > 0;
            if is_array {
                for pair in t.clone().pairs::<mlua::Value, mlua::Value>() {
                    let (k, _) = pair?;
                    match k {
                        mlua::Value::Integer(i) if i >= 1 && (i as usize) <= len => {}
                        _ => {
                            is_array = false;
                            break;
                        }
                    }
                }
            }
            if is_array {
                let mut arr = Vec::with_capacity(len as usize);
                for i in 1..=len {
                    let val = t.raw_get::<mlua::Value>(i)?;
                    arr.push(lua_to_json(val)?);
                }
                serde_json::Value::Array(arr)
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.pairs::<mlua::Value, mlua::Value>() {
                    let (k, v) = pair?;
                    let key = match k {
                        V::String(s) => s.to_str()?.to_string(),
                        V::Integer(i) => i.to_string(),
                        V::Number(n) => n.to_string(),
                        _ => continue,
                    };
                    map.insert(key, lua_to_json(v)?);
                }
                serde_json::Value::Object(map)
            }
        }
        _ => serde_json::Value::Null,
    })
}

/// JSON payload wrapping a plugin invocation, sent inside the `data` field
/// of an `ActionKind::Plugin` action so receivers know which plugin + which
/// metadata to display when prompting for authorization.
pub fn payload_envelope(plugin: &Plugin, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "plugin": {
            "id":          plugin.manifest.id,
            "name":        plugin.manifest.name,
            "version":     plugin.manifest.version,
            "author":      plugin.manifest.author,
            "description": plugin.manifest.description,
        },
        "args": args,
    })
}
