//! Installing new things: one input line (`+` in the TUI, `skillz add`) and the Browse catalog.
use crate::model::*;
use crate::net;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Debug, PartialEq)]
pub enum Plan {
    /// `npx skills add` source: `owner/repo` or `owner/repo@skill`.
    Skills(String),
    /// A repo that is a Claude plugin marketplace; installs its plugin when it has exactly one.
    Marketplace { repo: String, install: Option<String> },
    Plugin(String),
    Mcp { name: String, target: Vec<String> },
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// `https://www.ascii-magic.com/mcp` -> `ascii-magic`, `https://mcp.linear.app/sse` -> `linear`.
pub fn name_from_url(url: &str) -> String {
    let auth = url.split("://").nth(1).unwrap_or(url).split(['/', '?']).next().unwrap_or_default();
    let (host, port) = auth.split_once(':').unwrap_or((auth, ""));
    if host == "localhost" || host.parse::<std::net::IpAddr>().is_ok() {
        return if port.is_empty() { "local".into() } else { format!("local-{port}") };
    }
    let labels: Vec<&str> = host.split('.').filter(|l| *l != "www").collect();
    labels.get(labels.len().saturating_sub(2)).unwrap_or(&"mcp").to_string()
}

/// What the user typed -> what to install. No network here (see `resolve`).
pub fn parse(input: &str) -> Result<Plan, String> {
    let t: Vec<&str> = input.split_whitespace().collect();
    let gh = |s: &str| s.strip_prefix("https://github.com/").map(|r| r.split('/').take(2).collect::<Vec<_>>().join("/"));
    match t.as_slice() {
        [] => Err("type a repo, plugin@marketplace or MCP URL".into()),
        ["mcp", name, cmd @ ..] if !cmd.is_empty() => Ok(Plan::Mcp { name: name.to_string(), target: cmd.iter().map(|s| s.to_string()).collect() }),
        [name, url] if is_url(url) && gh(url).is_none() => Ok(Plan::Mcp { name: name.to_string(), target: vec![url.to_string()] }),
        [s] => Ok(match gh(s) {
            Some(repo) => Plan::Skills(repo),
            None if is_url(s) => Plan::Mcp { name: name_from_url(s), target: vec![s.to_string()] },
            None if s.contains('/') => Plan::Skills(s.to_string()),
            None if s.contains('@') => Plan::Plugin(s.to_string()),
            None => return Err(format!("'{s}': expected owner/repo, plugin@marketplace or an MCP URL")),
        }),
        _ => Err("expected: owner/repo · plugin@marketplace · <url> · <name> <url> · mcp <name> <command…>".into()),
    }
}

/// A bare `owner/repo` may be a plugin marketplace rather than a skills repo: ask GitHub.
pub fn resolve(plan: Plan) -> Plan {
    match plan {
        Plan::Skills(repo) if !repo.contains('@') => match net::raw_json(&repo, ".claude-plugin/marketplace.json") {
            Some(m) => {
                let plugins = m["plugins"].as_array().cloned().unwrap_or_default();
                let mkt = m["name"].as_str().unwrap_or_default();
                let install = (plugins.len() == 1 && !mkt.is_empty()).then(|| format!("{}@{mkt}", plugins[0]["name"].as_str().unwrap_or_default()));
                Plan::Marketplace { repo, install }
            }
            None => Plan::Skills(repo),
        },
        p => p,
    }
}

/// Commands that install it (MCP servers are config edits: `mcp::add`, no commands).
pub fn cmds(plan: &Plan) -> Cmds {
    match plan {
        Plan::Skills(s) if s.contains('@') => vec![sv(&["npx", "-y", "skills", "add", s, "-g", "-y"])],
        Plan::Skills(s) => vec![sv(&["npx", "-y", "skills", "add", s, "-g", "-y", "--skill", "*"])],
        Plan::Marketplace { repo, install } => {
            let mut v = vec![sv(&["claude", "plugin", "marketplace", "add", repo])];
            v.extend(install.iter().map(|id| sv(&["claude", "plugin", "install", id])));
            v
        }
        Plan::Plugin(id) => vec![sv(&["claude", "plugin", "install", id])],
        Plan::Mcp { .. } => vec![],
    }
}

/// Install command for a Browse row (plugin `name@mkt` or skills.sh `owner/repo@skill`).
pub fn catalog_cmd(id: &str) -> Vec<String> {
    if id.contains('/') { sv(&["npx", "-y", "skills", "add", id, "-g", "-y"]) } else { sv(&["claude", "plugin", "install", id]) }
}

fn catalog_entry(id: String, source: String, desc: String) -> Entry {
    Entry {
        kind: Kind::Catalog,
        id,
        source,
        in_lock: false,
        skills: vec![],
        vis: BTreeMap::new(),
        path: PathBuf::new(),
        desc,
        dups: vec![],
        claude_copy: false,
        claude_cmd: None,
        modes: BTreeMap::new(),
        omp_native: false,
        mcp: BTreeMap::new(),
    }
}

/// Plugins offered by the marketplaces Claude already knows, minus installed ones (local, instant).
pub fn catalog(cx: &Ctx) -> Vec<Entry> {
    let h = &cx.home;
    let installed = read_json(&h.join(".claude/plugins/installed_plugins.json"));
    let markets = read_json(&h.join(".claude/plugins/known_marketplaces.json"));
    let mut out = Vec::new();
    for (mkt, m) in markets.as_object().into_iter().flatten() {
        let dir = m["installLocation"].as_str().map(PathBuf::from).unwrap_or_else(|| h.join(".claude/plugins/marketplaces").join(mkt));
        for p in read_json(&dir.join(".claude-plugin/marketplace.json"))["plugins"].as_array().into_iter().flatten() {
            let Some(name) = p["name"].as_str() else { continue };
            let id = format!("{name}@{mkt}");
            if installed["plugins"].get(&id).is_none() {
                out.push(catalog_entry(id, mkt.clone(), p["description"].as_str().unwrap_or_default().to_string()));
            }
        }
    }
    out
}

/// skills.sh search (the registry behind `npx skills find`), minus skills already in the hub.
pub fn search_skills(cx_hub: &std::path::Path, query: &str) -> Vec<Entry> {
    let q: String = query.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '+' }).collect();
    let Some(v) = net::json(&format!("https://skills.sh/api/search?q={q}&limit=30")) else { return vec![] };
    v["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let (src, skill) = (s["source"].as_str()?, s["skillId"].as_str().or(s["name"].as_str())?);
            if cx_hub.join(skill).exists() {
                return None;
            }
            let installs = s["installs"].as_u64().unwrap_or(0);
            let n = if installs >= 1000 { format!("{:.1}K", installs as f64 / 1000.0) } else { installs.to_string() };
            Some(catalog_entry(format!("{src}@{skill}"), format!("skills.sh · {src}"), format!("{n} installs on skills.sh")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_form() {
        assert_eq!(parse("vercel-labs/agent-skills"), Ok(Plan::Skills("vercel-labs/agent-skills".into())));
        assert_eq!(parse("https://github.com/a/b/tree/main/x"), Ok(Plan::Skills("a/b".into())));
        assert_eq!(parse("juliusbrussee/caveman@caveman"), Ok(Plan::Skills("juliusbrussee/caveman@caveman".into())));
        assert_eq!(parse("ponytail@ponytail"), Ok(Plan::Plugin("ponytail@ponytail".into())));
        assert_eq!(parse("https://www.ascii-magic.com/mcp"), Ok(Plan::Mcp { name: "ascii-magic".into(), target: vec!["https://www.ascii-magic.com/mcp".into()] }));
        assert_eq!(parse("lin https://mcp.linear.app/mcp"), Ok(Plan::Mcp { name: "lin".into(), target: vec!["https://mcp.linear.app/mcp".into()] }));
        assert_eq!(parse("mcp pw npx @playwright/mcp"), Ok(Plan::Mcp { name: "pw".into(), target: vec!["npx".into(), "@playwright/mcp".into()] }));
        assert!(parse("").is_err() && parse("justaword").is_err());
    }

    #[test]
    fn url_names() {
        assert_eq!(name_from_url("https://mcp.linear.app/sse"), "linear");
        assert_eq!(name_from_url("http://127.0.0.1:3274/mcp"), "local-3274");
        assert_eq!(name_from_url("http://localhost/mcp"), "local");
    }
}
