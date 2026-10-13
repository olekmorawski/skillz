//! MCP servers: one row per server name, wherever it's defined. "Off" uses the agent's own
//! switch (Codex/OpenCode `enabled`, omp `disabledServers`, Gemini `mcp.excluded`); Claude has
//! no global switch, so its definition is parked in ~/.agents/skillz.json and restored on "on".
use crate::model::*;
use serde_json::{json, Map, Value};
use std::{collections::{BTreeMap, BTreeSet}, io, path::{Path, PathBuf}};
use toml_edit::{InlineTable, Item as TItem, Table as TTable, Value as TValue};

pub const PREFIX: &str = "mcp:";

pub fn name_of(id: &str) -> &str {
    id.strip_prefix(PREFIX).unwrap_or(id)
}

// ---------- definitions: canonical form is Claude's {type, command, args, env, cwd, url, headers} ----------

fn put(m: &mut Map<String, Value>, k: &str, v: &Value) {
    if !(v.is_null() || v == &json!({}) || v == &json!([])) {
        m.insert(k.into(), v.clone());
    }
}

/// An agent's own entry -> canonical definition.
pub fn from_agent(p: P, raw: &Value) -> Value {
    let mut m = Map::new();
    if p == P::OpenCode {
        if raw["type"] == "remote" || raw["url"].is_string() {
            m.insert("type".into(), json!("http"));
            put(&mut m, "url", &raw["url"]);
            put(&mut m, "headers", &raw["headers"]);
        } else {
            let cmd = raw["command"].as_array().cloned().unwrap_or_default();
            m.insert("type".into(), json!("stdio"));
            put(&mut m, "command", cmd.first().unwrap_or(&Value::Null));
            put(&mut m, "args", &json!(cmd.get(1..).unwrap_or(&[])));
            put(&mut m, "env", &raw["environment"]);
        }
        return Value::Object(m);
    }
    let url = raw["httpUrl"].as_str().or(raw["url"].as_str());
    if let Some(u) = url {
        let t = match p {
            P::Gemini if raw["httpUrl"].is_null() => "sse",
            P::Claude | P::Omp => raw["type"].as_str().filter(|t| *t != "stdio").unwrap_or("http"),
            _ => "http",
        };
        m.insert("type".into(), json!(t));
        m.insert("url".into(), json!(u));
        put(&mut m, "headers", if raw["headers"].is_null() { &raw["http_headers"] } else { &raw["headers"] });
    } else {
        m.insert("type".into(), json!("stdio"));
        for k in ["command", "args", "env", "cwd"] {
            put(&mut m, k, &raw[k]);
        }
    }
    Value::Object(m)
}

/// Canonical definition -> what the agent expects in its config.
pub fn to_agent(p: P, d: &Value) -> Value {
    let remote = d["url"].is_string();
    let mut m = Map::new();
    match p {
        P::Claude | P::Omp => {
            for k in ["type", "command", "args", "env", "url", "headers"] {
                put(&mut m, k, &d[k]);
            }
            if p == P::Omp {
                put(&mut m, "cwd", &d["cwd"]);
            }
        }
        P::Codex if remote => {
            put(&mut m, "url", &d["url"]);
            put(&mut m, "http_headers", &d["headers"]);
        }
        P::Gemini if remote => {
            put(&mut m, if d["type"] == "sse" { "url" } else { "httpUrl" }, &d["url"]);
            put(&mut m, "headers", &d["headers"]);
        }
        P::Codex | P::Gemini => {
            for k in ["command", "args", "env", "cwd"] {
                put(&mut m, k, &d[k]);
            }
        }
        P::OpenCode if remote => {
            m.insert("type".into(), json!("remote"));
            put(&mut m, "url", &d["url"]);
            put(&mut m, "headers", &d["headers"]);
        }
        P::OpenCode => {
            let mut cmd = vec![d["command"].clone()];
            cmd.extend(d["args"].as_array().cloned().unwrap_or_default());
            m.insert("type".into(), json!("local"));
            m.insert("command".into(), json!(cmd));
            put(&mut m, "environment", &d["env"]);
        }
    }
    Value::Object(m)
}

/// One-line summary: `stdio · cmd args` / `http · url`.
pub fn summary(d: &Value) -> String {
    match d["url"].as_str() {
        Some(u) => format!("{} · {u}", d["type"].as_str().unwrap_or("http")),
        None => {
            let args: Vec<&str> = d["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).collect();
            format!("stdio · {} {}", d["command"].as_str().unwrap_or("?"), args.join(" ")).trim_end().to_string()
        }
    }
}

/// Safe to copy to another agent: a URL, an absolute command that exists, or one on PATH.
pub fn portable(d: &Value) -> bool {
    if d["url"].is_string() {
        return true;
    }
    let Some(c) = d["command"].as_str() else { return false };
    let cwd_ok = d["cwd"].as_str().is_none_or(|w| Path::new(w).is_absolute());
    cwd_ok && if c.contains('/') { Path::new(c).is_absolute() && Path::new(c).exists() } else { has_cmd(c) }
}

// ---------- Codex TOML <-> JSON ----------

fn tv_json(v: &TValue) -> Value {
    match v {
        TValue::String(s) => json!(s.value()),
        TValue::Integer(i) => json!(i.value()),
        TValue::Float(f) => json!(f.value()),
        TValue::Boolean(b) => json!(b.value()),
        TValue::Datetime(d) => json!(d.value().to_string()),
        TValue::Array(a) => Value::Array(a.iter().map(tv_json).collect()),
        TValue::InlineTable(t) => Value::Object(t.iter().map(|(k, v)| (k.to_string(), tv_json(v))).collect()),
    }
}

pub fn toml_json(i: &TItem) -> Value {
    match i {
        TItem::Value(v) => tv_json(v),
        TItem::Table(t) => Value::Object(t.iter().map(|(k, v)| (k.to_string(), toml_json(v))).collect()),
        _ => Value::Null,
    }
}

fn json_tv(v: &Value) -> TValue {
    match v {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => n.as_i64().map(TValue::from).unwrap_or_else(|| n.as_f64().unwrap_or(0.0).into()),
        Value::Array(a) => TValue::Array(a.iter().map(json_tv).collect()),
        Value::Object(o) => {
            let mut t = InlineTable::new();
            for (k, v) in o {
                t.insert(k, json_tv(v));
            }
            TValue::InlineTable(t)
        }
        Value::String(s) => s.as_str().into(),
        Value::Null => "".into(),
    }
}

fn codex_servers(cx: &mut Ctx) -> &mut TTable {
    if !cx.codex.contains_key("mcp_servers") {
        let mut t = TTable::new();
        t.set_implicit(true);
        cx.codex.insert("mcp_servers", TItem::Table(t));
    }
    cx.codex["mcp_servers"].as_table_mut().expect("[mcp_servers] must be a table")
}

// ---------- reading ----------

fn strs(v: &Value) -> BTreeSet<String> {
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect()
}

fn omp_disabled(cx: &Ctx) -> BTreeSet<String> {
    strs(&cx.omp_mcp["disabledServers"])
}

fn gemini_excluded(cx: &Ctx) -> BTreeSet<String> {
    strs(&cx.gemini["mcp"]["excluded"])
}

/// Every server an agent defines itself: name -> (raw entry, switched on). Claude's parked ones count, as off.
fn defined(cx: &Ctx, p: P) -> BTreeMap<String, (Value, bool)> {
    let pairs = |v: &Value, on: &dyn Fn(&str, &Value) -> bool| -> BTreeMap<String, (Value, bool)> {
        v.as_object().into_iter().flatten().map(|(k, v)| (k.clone(), (v.clone(), on(k, v)))).collect()
    };
    match p {
        P::Claude => {
            let mut m = pairs(&cx.state["mcp_parked"], &|_, _| false);
            m.extend(pairs(&cx.claude_json["mcpServers"], &|_, _| true));
            m
        }
        P::Codex => cx
            .codex
            .get("mcp_servers")
            .and_then(|t| t.as_table())
            .map(|t| t.iter().map(|(k, v)| (k.to_string(), (toml_json(v), v.get("enabled").and_then(|e| e.as_bool()) != Some(false)))).collect())
            .unwrap_or_default(),
        P::Omp => {
            let off = omp_disabled(cx);
            pairs(&cx.omp_mcp["mcpServers"], &|k, _| !off.contains(k))
        }
        P::OpenCode => cx
            .opencode
            .as_ref()
            .map(|o| {
                let legacy: Map<String, Value> = o["mcp"].as_object().into_iter().flatten().filter(|(k, _)| *k != "servers").map(|(k, v)| (k.clone(), v.clone())).collect();
                let mut m = pairs(&Value::Object(legacy), &|_, v| v["enabled"] != json!(false));
                m.extend(pairs(&o["mcp"]["servers"], &|_, v| v["disabled"] != json!(true)));
                m
            })
            .unwrap_or_default(),
        P::Gemini => {
            let off = gemini_excluded(cx);
            pairs(&cx.gemini["mcpServers"], &|k, _| !off.contains(k))
        }
    }
}

/// omp also loads Claude's servers (via its claude provider).
fn omp_reaches(cx: &Ctx, name: &str) -> bool {
    cx.omp_mcp["mcpServers"].get(name).is_some() || cx.claude_json["mcpServers"].get(name).is_some()
}

pub fn load(cx: &Ctx) -> Vec<Entry> {
    let per: BTreeMap<P, BTreeMap<String, (Value, bool)>> = cx.agents.iter().map(|&p| (p, defined(cx, p))).collect();
    let names: BTreeSet<&String> = per.values().flat_map(|m| m.keys()).collect();
    let omp_off = omp_disabled(cx);
    let mut out = Vec::new();
    for name in names {
        let mut vis = BTreeMap::new();
        let mut defs = BTreeMap::new();
        for &p in &cx.agents {
            // OpenCode with a JSONC config can't be read or written
            if p == P::OpenCode && cx.opencode.is_none() {
                continue;
            }
            let own = per[&p].get(name);
            if let Some((raw, _)) = own {
                defs.insert(p, from_agent(p, raw));
            }
            let on = match p {
                P::Omp => omp_reaches(cx, name) && !omp_off.contains(name),
                _ => own.is_some_and(|(_, on)| *on),
            };
            vis.insert(p, if on { Vis::On } else { Vis::Off });
        }
        let best = defs.get(&P::Claude).or_else(|| defs.values().next()).cloned().unwrap_or_default();
        out.push(Entry {
            kind: Kind::Mcp,
            id: format!("{PREFIX}{name}"),
            source: best["type"].as_str().unwrap_or("stdio").to_string(),
            in_lock: false,
            skills: vec![],
            vis,
            path: PathBuf::from(best["url"].as_str().or(best["command"].as_str()).unwrap_or_default()),
            desc: summary(&best),
            dups: vec![],
            claude_copy: false,
            claude_cmd: None,
            modes: BTreeMap::new(),
            omp_native: false,
            mcp: defs,
        });
    }
    out
}

/// No definition at all in this agent (and not switched off there): `sync` may copy one in.
pub fn absent(cx: &Ctx, e: &Entry, p: P) -> bool {
    let n = name_of(&e.id);
    !e.mcp.contains_key(&p) && match p {
        P::Omp => !omp_reaches(cx, n) && !omp_disabled(cx).contains(n),
        P::Gemini => !gemini_excluded(cx).contains(n),
        _ => true,
    }
}

// ---------- switching ----------

fn set_member(v: &mut Value, name: &str, add: bool) -> bool {
    let mut s = strs(v);
    let changed = if add { s.insert(name.into()) } else { s.remove(name) };
    if changed {
        *v = json!(s);
    }
    changed
}

fn best(e: &Entry) -> io::Result<Value> {
    e.mcp.get(&P::Claude).or_else(|| e.mcp.values().next()).cloned().ok_or_else(|| io::Error::other(format!("{} has no definition to copy", e.id)))
}

pub fn set(cx: &mut Ctx, e: &Entry, p: P, on: bool) -> io::Result<()> {
    let n = name_of(&e.id).to_string();
    match p {
        P::Claude => {
            if !cx.claude_json["mcpServers"].is_object() {
                cx.claude_json["mcpServers"] = json!({});
            }
            if on {
                let raw = match cx.state["mcp_parked"].as_object_mut().and_then(|m| m.remove(&n)) {
                    Some(r) => r,
                    None => to_agent(P::Claude, &best(e)?),
                };
                cx.claude_json["mcpServers"][&n] = raw;
            } else if let Some(raw) = cx.claude_json["mcpServers"].as_object_mut().unwrap().remove(&n) {
                // omp reads Claude's servers; keep it a Claude-only change
                if cx.agents.contains(&P::Omp) && cx.omp_mcp["mcpServers"].get(&n).is_none() && !omp_disabled(cx).contains(&n) {
                    set_omp_def(cx, &n, to_agent(P::Omp, &from_agent(P::Claude, &raw)));
                }
                if !cx.state["mcp_parked"].is_object() {
                    cx.state["mcp_parked"] = json!({});
                }
                cx.state["mcp_parked"][&n] = raw;
            }
            cx.touch("claude_json");
            cx.touch("state");
        }
        P::Codex => {
            let def = if on { Some(to_agent(P::Codex, &best(e)?)) } else { None };
            let servers = codex_servers(cx);
            match servers.get_mut(&n).and_then(|t| t.as_table_like_mut()) {
                Some(t) if on => {
                    t.remove("enabled");
                }
                Some(t) => {
                    t.insert("enabled", toml_edit::value(false));
                }
                None if on => {
                    let mut t = TTable::new();
                    for (k, v) in def.unwrap().as_object().unwrap() {
                        t.insert(k, TItem::Value(json_tv(v)));
                    }
                    servers.insert(&n, TItem::Table(t));
                }
                None => {}
            }
            cx.touch("codex");
        }
        P::Omp => {
            if !cx.omp_mcp["disabledServers"].is_array() {
                cx.omp_mcp["disabledServers"] = json!([]);
            }
            set_member(&mut cx.omp_mcp["disabledServers"], &n, !on);
            if cx.omp_mcp["disabledServers"] == json!([]) {
                cx.omp_mcp.as_object_mut().unwrap().remove("disabledServers");
            }
            if on && !omp_reaches(cx, &n) {
                set_omp_def(cx, &n, to_agent(P::Omp, &best(e)?));
            }
            cx.touch("omp_mcp");
        }
        P::OpenCode => {
            let def = to_agent(P::OpenCode, &best(e)?);
            let Some(oc) = cx.opencode.as_mut() else {
                return Err(io::Error::other("opencode.json has comments (JSONC); edit its `mcp` block by hand"));
            };
            let servers = oc_servers(oc);
            match servers.get_mut(&n) {
                Some(s) if on => {
                    s.as_object_mut().map(|o| o.remove("disabled"));
                }
                Some(s) => s["disabled"] = json!(true),
                None if on => {
                    servers.insert(n.clone(), def);
                }
                None => {}
            }
            cx.touch("opencode");
        }
        P::Gemini => {
            if !cx.gemini["mcp"].is_object() {
                cx.gemini["mcp"] = json!({});
            }
            set_member(&mut cx.gemini["mcp"]["excluded"], &n, !on);
            if on && cx.gemini["mcpServers"].get(&n).is_none() {
                if !cx.gemini["mcpServers"].is_object() {
                    cx.gemini["mcpServers"] = json!({});
                }
                cx.gemini["mcpServers"][&n] = to_agent(P::Gemini, &best(e)?);
            }
            cx.touch("gemini");
        }
    }
    Ok(())
}

/// OpenCode v2 only loads `mcp.servers.<name>` (with `disabled`); move v1 `mcp.<name>` (with `enabled`) there.
fn oc_servers(oc: &mut Value) -> &mut Map<String, Value> {
    if !oc["mcp"].is_object() {
        oc["mcp"] = json!({});
    }
    let mcp = oc["mcp"].as_object_mut().unwrap();
    let legacy: Vec<String> = mcp.keys().filter(|k| *k != "servers").cloned().collect();
    let mut servers = mcp.remove("servers").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    for k in legacy {
        let mut v = mcp.remove(&k).unwrap();
        if let Some(o) = v.as_object_mut() {
            if o.remove("enabled") == Some(json!(false)) {
                o.insert("disabled".into(), json!(true));
            }
        }
        servers.entry(k).or_insert(v);
    }
    mcp.insert("servers".into(), Value::Object(servers));
    mcp["servers"].as_object_mut().unwrap()
}

fn set_omp_def(cx: &mut Ctx, name: &str, def: Value) {
    if !cx.omp_mcp["mcpServers"].is_object() {
        cx.omp_mcp["mcpServers"] = json!({});
    }
    cx.omp_mcp["mcpServers"][name] = def;
    cx.touch("omp_mcp");
}

/// Remove the server from every agent's config, Claude's project scopes and parked copies.
pub fn purge(cx: &mut Ctx, e: &Entry) {
    let n = name_of(&e.id).to_string();
    let drop = |v: &mut Value| v.as_object_mut().is_some_and(|m| m.remove(&n).is_some());
    drop(&mut cx.claude_json["mcpServers"]);
    drop(&mut cx.state["mcp_parked"]);
    cx.claude_mcp_rm.insert(n.clone());
    cx.touch("claude_json");
    cx.touch("state");
    if let Some(t) = cx.codex.get_mut("mcp_servers").and_then(|t| t.as_table_mut()) {
        if t.remove(&n).is_some() {
            cx.touch("codex");
        }
    }
    if drop(&mut cx.omp_mcp["mcpServers"]) | set_member(&mut cx.omp_mcp["disabledServers"], &n, false) {
        cx.touch("omp_mcp");
    }
    if let Some(oc) = cx.opencode.as_mut() {
        if oc["mcp"].as_object_mut().is_some_and(|m| m.remove(&n).is_some()) | oc["mcp"]["servers"].as_object_mut().is_some_and(|m| m.remove(&n).is_some()) {
            cx.touch("opencode");
        }
    }
    if drop(&mut cx.gemini["mcpServers"]) | set_member(&mut cx.gemini["mcp"]["excluded"], &n, false) {
        cx.touch("gemini");
    }
}

/// `skillz mcp add <name> <url | command [args…]>`: a row that only has a definition to copy.
pub fn new_entry(name: &str, target: &[String]) -> io::Result<Entry> {
    let first = target.first().ok_or_else(|| io::Error::other("usage: skillz mcp add <name> <url | command [args…]>"))?;
    let def = if first.starts_with("http://") || first.starts_with("https://") {
        json!({"type": "http", "url": first})
    } else {
        json!({"type": "stdio", "command": first, "args": &target[1..]})
    };
    Ok(Entry {
        kind: Kind::Mcp,
        id: format!("{PREFIX}{name}"),
        source: def["type"].as_str().unwrap().into(),
        in_lock: false,
        skills: vec![],
        vis: BTreeMap::new(),
        path: PathBuf::new(),
        desc: summary(&def),
        dups: vec![],
        claude_copy: false,
        claude_cmd: None,
        modes: BTreeMap::new(),
        omp_native: false,
        mcp: BTreeMap::from([(P::Claude, def)]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_every_agent() {
        let defs = [
            json!({"type": "stdio", "command": "/bin/echo", "args": ["mcp"], "env": {"A": "1"}}),
            json!({"type": "http", "url": "https://x.dev/mcp", "headers": {"H": "v"}}),
            json!({"type": "sse", "url": "https://x.dev/sse"}),
        ];
        for d in &defs {
            for p in ALL {
                let back = from_agent(p, &to_agent(p, d));
                // Codex and OpenCode only speak streamable http for remotes
                let want = if d["type"] == "sse" && matches!(p, P::Codex | P::OpenCode) { json!({"type": "http", "url": d["url"]}) } else { d.clone() };
                assert_eq!(back, want, "{p:?}");
            }
        }
    }

    #[test]
    fn reads_native_shapes() {
        assert_eq!(from_agent(P::Gemini, &json!({"httpUrl": "https://a"}))["type"], "http");
        assert_eq!(from_agent(P::Codex, &json!({"url": "https://a", "enabled": false}))["url"], "https://a");
        assert_eq!(from_agent(P::OpenCode, &json!({"type": "local", "command": ["x", "y"], "enabled": true})), json!({"type": "stdio", "command": "x", "args": ["y"]}));
        assert_eq!(from_agent(P::Claude, &json!({"command": "graft", "args": ["mcp"]}))["type"], "stdio");
    }

    #[test]
    fn toml_json_roundtrip() {
        let d = to_agent(P::Codex, &json!({"type": "stdio", "command": "c", "args": ["a"], "env": {"K": "v"}}));
        let mut t = TTable::new();
        for (k, v) in d.as_object().unwrap() {
            t.insert(k, TItem::Value(json_tv(v)));
        }
        assert_eq!(toml_json(&TItem::Table(t)), d);
    }

    #[test]
    fn opencode_v1_entries_move_to_servers() {
        let mut oc = json!({"mcp": {"a": {"type": "local", "command": ["x"], "enabled": false}, "servers": {"b": {"type": "remote", "url": "u"}}}});
        oc_servers(&mut oc);
        assert_eq!(oc, json!({"mcp": {"servers": {"b": {"type": "remote", "url": "u"}, "a": {"type": "local", "command": ["x"], "disabled": true}}}}));
    }

    #[test]
    fn portability() {
        assert!(portable(&json!({"url": "https://a"})));
        assert!(portable(&json!({"command": "/bin/sh"})));
        assert!(!portable(&json!({"command": "./App.app/x"})));
        assert!(!portable(&json!({"command": "/bin/sh", "cwd": "."})));
    }
}
