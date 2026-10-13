//! Skills hub + per-agent visibility. Every switch lives in the agent's own config;
//! nothing is copied - plugins exist once (Claude's plugin cache) and are linked elsewhere.
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};
use toml_edit::{ArrayOfTables, DocumentMut, Item as TItem, Table as TTable};

// ---------- agents ----------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum P {
    Claude,
    Codex,
    Omp,
    OpenCode,
    Gemini,
}

pub const ALL: [P; 5] = [P::Claude, P::Codex, P::Omp, P::OpenCode, P::Gemini];

impl P {
    pub fn name(self) -> &'static str {
        match self {
            P::Claude => "Claude",
            P::Codex => "Codex",
            P::Omp => "omp",
            P::OpenCode => "OpenCode",
            P::Gemini => "Gemini",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            P::Claude => "claude",
            P::Codex => "codex",
            P::Omp => "omp",
            P::OpenCode => "opencode",
            P::Gemini => "gemini",
        }
    }
    pub fn parse(s: &str) -> Option<P> {
        ALL.into_iter().find(|p| p.key() == s.to_lowercase())
    }
    /// The agent counts as installed when its home exists.
    fn marker(self) -> &'static str {
        match self {
            P::Claude => ".claude",
            P::Codex => ".codex",
            P::Omp => ".omp",
            P::OpenCode => ".config/opencode",
            P::Gemini => ".gemini",
        }
    }
    /// Where plugin skills get linked for agents that can't install the plugin natively.
    pub fn link_dir(self, h: &Path) -> Option<PathBuf> {
        match self {
            P::Codex => Some(h.join(".codex/skills")),
            P::OpenCode => Some(h.join(".config/opencode/skills")),
            P::Gemini => Some(h.join(".gemini/skills")),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Vis {
    On,
    Off,
    /// Visible and the agent offers no switch for it (e.g. OpenCode v2 always loads ~/.agents/skills).
    Always,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Skill,
    Plugin,
    Mcp,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    /// Agent installs the plugin itself (Codex `name@market`, OpenCode plugin spec).
    Native(String),
    /// Plugin's skills are symlinked into the agent's skills dir as `<plugin>--<skill>`.
    Links,
}

#[derive(Debug)]
pub struct Entry {
    pub kind: Kind,
    pub id: String, // skill name or plugin id (name@marketplace)
    pub source: String,
    pub in_lock: bool,
    pub skills: Vec<String>,
    pub vis: BTreeMap<P, Vis>,
    pub path: PathBuf,
    pub desc: String,
    pub dups: Vec<String>,
    pub claude_copy: bool,           // Claude has its own copy (not a hub link)
    pub claude_cmd: Option<String>,  // `source-command-*` skills map to ~/.claude/commands/<cmd>
    pub modes: BTreeMap<P, Mode>,    // plugins only
    pub omp_native: bool,
    pub mcp: BTreeMap<P, Value>,     // MCP only: canonical definition per agent that has one
}

impl Entry {
    pub fn plugin_name(&self) -> &str {
        self.id.split('@').next().unwrap_or(&self.id)
    }
}

// ---------- context (all agent configs, loaded once, saved together) ----------

pub struct Ctx {
    pub home: PathBuf,
    pub agents: Vec<P>,
    pub codex: DocumentMut,
    pub omp_ignored: BTreeSet<String>,
    pub omp_reg: Value,
    pub gemini: Value,
    pub opencode: Option<Value>, // None when opencode.json can't be parsed as plain JSON
    pub claude: Value,
    pub claude_json: Value, // ~/.claude.json, for `mcpServers`
    pub claude_mcp_rm: BTreeSet<String>, // purged servers to drop from Claude's project scopes too
    pub omp_mcp: Value,
    pub state: Value,
    dirty: BTreeSet<&'static str>,
    omp_ignored_orig: BTreeSet<String>,
}

pub fn read_json(p: &Path) -> Value {
    fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
}

fn write_json(p: &Path, v: &Value) -> io::Result<()> {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    fs::write(p, serde_json::to_string_pretty(v).unwrap() + "\n")
}

/// 'https://github.com/a/b.git' | 'a/b' -> 'a/b'
pub fn repo_of(s: &str) -> String {
    s.trim_start_matches("https://github.com/").trim_end_matches(".git").trim_matches('/').to_lowercase()
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is not set"))
}

pub fn has_cmd(c: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {c}")]).output().is_ok_and(|o| o.status.success())
}

impl Ctx {
    pub fn load() -> Ctx {
        let home = home();
        let agents: Vec<P> = ALL.into_iter().filter(|p| home.join(p.marker()).is_dir()).collect();
        let codex = fs::read_to_string(home.join(".codex/config.toml")).unwrap_or_default().parse().unwrap_or_default();
        let omp_ignored: BTreeSet<String> = if agents.contains(&P::Omp) && has_cmd("omp") {
            Command::new("omp")
                .args(["config", "get", "skills.ignoredSkills", "--json"])
                .output()
                .ok()
                .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
                .and_then(|v| v["value"].as_array().cloned())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                .unwrap_or_default()
        } else {
            BTreeSet::new()
        };
        let oc_path = home.join(".config/opencode/opencode.json");
        let opencode = match fs::read_to_string(&oc_path) {
            Ok(s) => serde_json::from_str(&s).ok(),
            Err(_) => Some(json!({})),
        };
        let mut state = read_json(&home.join(".agents/skillz.json"));
        migrate_state(&mut state);
        Ctx {
            omp_reg: read_json(&home.join(".omp/plugins/installed_plugins.json")),
            gemini: read_json(&home.join(".gemini/settings.json")),
            claude: read_json(&home.join(".claude/settings.json")),
            claude_json: read_json(&home.join(".claude.json")),
            claude_mcp_rm: BTreeSet::new(),
            omp_mcp: read_json(&home.join(".omp/agent/mcp.json")),
            opencode,
            state,
            dirty: BTreeSet::new(),
            omp_ignored_orig: omp_ignored.clone(),
            omp_ignored,
            codex,
            agents,
            home,
        }
    }

    pub fn hub(&self) -> PathBuf {
        self.home.join(".agents/skills")
    }

    pub fn save(&mut self) -> io::Result<()> {
        let h = self.home.clone();
        if self.dirty.contains("codex") {
            let text = self.codex.to_string();
            text.parse::<DocumentMut>().map_err(|e| io::Error::other(format!("refusing to write broken config.toml: {e}")))?;
            fs::write(h.join(".codex/config.toml"), text)?;
        }
        if self.dirty.contains("claude") {
            // re-read so concurrent edits by Claude Code survive; only our key changes
            let mut cur = read_json(&h.join(".claude/settings.json"));
            cur["enabledPlugins"] = self.claude["enabledPlugins"].clone();
            write_json(&h.join(".claude/settings.json"), &cur)?;
        }
        if self.dirty.contains("gemini") {
            let mut cur = read_json(&h.join(".gemini/settings.json"));
            if !cur["skills"].is_object() {
                cur["skills"] = json!({});
            }
            cur["skills"]["disabled"] = self.gemini["skills"]["disabled"].clone();
            for k in ["mcpServers", "mcp"] {
                if !self.gemini[k].is_null() {
                    cur[k] = self.gemini[k].clone();
                }
            }
            write_json(&h.join(".gemini/settings.json"), &cur)?;
        }
        if self.dirty.contains("opencode") {
            if let Some(v) = &self.opencode {
                write_json(&h.join(".config/opencode/opencode.json"), v)?;
            }
        }
        if self.dirty.contains("claude_json") {
            // Claude Code rewrites this file all the time: re-read, change only MCP keys, swap in atomically
            let p = h.join(".claude.json");
            // never replace the whole file with a partial view: an unreadable file means abort
            let mut cur: Value = match fs::read_to_string(&p) {
                Ok(t) => serde_json::from_str(&t).map_err(|e| io::Error::other(format!("~/.claude.json is mid-write or broken ({e}); try again")))?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
                Err(e) => return Err(e),
            };
            if !self.claude_json["mcpServers"].is_null() {
                cur["mcpServers"] = self.claude_json["mcpServers"].clone();
            }
            for proj in cur["projects"].as_object_mut().into_iter().flat_map(|m| m.values_mut()) {
                if let Some(m) = proj["mcpServers"].as_object_mut() {
                    m.retain(|k, _| !self.claude_mcp_rm.contains(k));
                }
            }
            let tmp = h.join(".claude.json.skillz-tmp");
            write_json(&tmp, &cur)?;
            fs::rename(&tmp, &p)?;
            self.claude_mcp_rm.clear();
        }
        if self.dirty.contains("omp_mcp") {
            write_json(&h.join(".omp/agent/mcp.json"), &self.omp_mcp)?;
        }
        if self.dirty.contains("omp_reg") {
            write_json(&h.join(".omp/plugins/installed_plugins.json"), &self.omp_reg)?;
        }
        if self.omp_ignored != self.omp_ignored_orig {
            let v = serde_json::to_string(&self.omp_ignored).unwrap();
            Command::new("omp").args(["config", "set", "skills.ignoredSkills", &v]).output()?;
            self.omp_ignored_orig = self.omp_ignored.clone();
        }
        if self.dirty.contains("state") {
            write_json(&h.join(".agents/skillz.json"), &self.state)?;
        }
        self.dirty.clear();
        Ok(())
    }

    pub(crate) fn touch(&mut self, what: &'static str) {
        self.dirty.insert(what);
    }

    /// Agents the user switched off for an entry; `sync` must not switch them back on.
    pub fn is_off(&self, p: P, id: &str) -> bool {
        self.state["off"][p.key()].as_array().is_some_and(|a| a.iter().any(|v| v == id))
    }

    fn remember(&mut self, p: P, id: &str, off: bool) {
        let mut set: BTreeSet<String> = self.state["off"][p.key()].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
        let changed = if off { set.insert(id.into()) } else { set.remove(id) };
        if changed {
            if !self.state["off"].is_object() {
                self.state["off"] = json!({});
            }
            self.state["off"][p.key()] = json!(set);
            self.touch("state");
        }
    }

    fn forget(&mut self, id: &str) {
        for p in ALL {
            self.remember(p, id, false);
        }
    }

    fn gemini_disabled(&self) -> BTreeSet<String> {
        self.gemini["skills"]["disabled"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect()
    }

    fn set_gemini_disabled(&mut self, name: &str, off: bool) {
        let mut set = self.gemini_disabled();
        if off { set.insert(name.into()); } else { set.remove(name); }
        if !self.gemini["skills"].is_object() {
            self.gemini["skills"] = json!({});
        }
        self.gemini["skills"]["disabled"] = json!(set);
        self.touch("gemini");
    }
}

/// skillz <= 0.1 kept separate lists; fold them into `off.<agent>`.
fn migrate_state(state: &mut Value) {
    for (old, agent) in [("claude_skills_off", "claude"), ("codex_plugins_off", "codex"), ("omp_plugins_off", "omp")] {
        if let Some(a) = state.get(old).and_then(|v| v.as_array()).cloned() {
            if !state["off"].is_object() {
                state["off"] = json!({});
            }
            let mut set: BTreeSet<String> = state["off"][agent].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
            set.extend(a.iter().filter_map(|v| v.as_str().map(String::from)));
            state["off"][agent] = json!(set);
        }
        if let Some(o) = state.as_object_mut() {
            o.remove(old);
        }
    }
}

// ---------- Codex config.toml (pure) ----------

pub fn codex_disabled_skills(doc: &DocumentMut) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(arr) = doc.get("skills").and_then(|s| s.get("config")).and_then(|c| c.as_array_of_tables()) {
        for t in arr.iter() {
            if t.get("enabled").and_then(|e| e.as_bool()) == Some(false) {
                if let Some(n) = t.get("name").and_then(|n| n.as_str()) {
                    out.insert(n.to_string());
                }
            }
        }
    }
    out
}

pub fn codex_set_skill(doc: &mut DocumentMut, name: &str, on: bool) {
    let has_rule = doc.get("skills").and_then(|s| s.get("config")).is_some();
    if on && !has_rule {
        return;
    }
    if !doc.contains_key("skills") {
        let mut t = TTable::new();
        t.set_implicit(true);
        doc.insert("skills", TItem::Table(t));
    }
    let skills = doc["skills"].as_table_mut().expect("[skills] must be a table");
    if !skills.contains_key("config") {
        skills.insert("config", TItem::ArrayOfTables(ArrayOfTables::new()));
    }
    let arr = skills["config"].as_array_of_tables_mut().expect("skills.config must be [[array]]");
    arr.retain(|t| t.get("name").and_then(|n| n.as_str()) != Some(name));
    if !on {
        let mut t = TTable::new();
        t["name"] = toml_edit::value(name);
        t["enabled"] = toml_edit::value(false);
        arr.push(t);
    }
}

pub fn codex_plugin_enabled(doc: &DocumentMut, id: &str) -> Option<bool> {
    doc.get("plugins")?.get(id)?.get("enabled")?.as_bool()
}

pub fn codex_set_plugin(doc: &mut DocumentMut, id: &str, on: bool) -> bool {
    match doc.get_mut("plugins").and_then(|p| p.get_mut(id)).and_then(|t| t.as_table_mut()) {
        Some(t) => {
            t["enabled"] = toml_edit::value(on);
            true
        }
        None => false,
    }
}

// ---------- omp plugin registry (pure) ----------

/// omp follows Claude's `enabledPlugins` unless its own registry has an entry for the id.
pub fn omp_plugin_on(reg: &Value, id: &str, claude_on: bool) -> bool {
    match reg["plugins"][id].as_array() {
        Some(es) if !es.is_empty() => es.iter().all(|e| e["enabled"].as_bool() != Some(false)),
        _ => claude_on,
    }
}

/// Our override entries carry `skillz: true` and disappear once omp would match Claude anyway.
pub fn omp_set_plugin(reg: &mut Value, id: &str, on: bool, claude_on: bool, path: &str, version: &str) {
    if !reg["plugins"].is_object() {
        reg["version"] = json!(2);
        reg["plugins"] = json!({});
    }
    let plugins = reg["plugins"].as_object_mut().unwrap();
    let ours = plugins.get(id).and_then(|v| v.as_array()).is_none_or(|es| es.iter().all(|e| e["skillz"] == json!(true)));
    if !ours {
        for e in plugins[id].as_array_mut().unwrap() {
            if on {
                e.as_object_mut().unwrap().remove("enabled");
            } else {
                e["enabled"] = json!(false);
            }
        }
    } else if on == claude_on {
        plugins.remove(id);
    } else {
        plugins.insert(id.into(), json!([{ "scope": "user", "installPath": path, "version": version, "enabled": on, "skillz": true }]));
    }
}

// ---------- OpenCode plugin specs (pure) ----------

/// An opencode.json `plugin` entry belongs to this plugin if it names its repo or npm package.
pub fn opencode_spec_matches(spec: &str, repo: &str, npm: Option<&str>) -> bool {
    let s = spec.to_lowercase();
    (!repo.is_empty() && s.contains(repo)) || npm.is_some_and(|n| s == n.to_lowercase() || s.starts_with(&format!("{}@", n.to_lowercase())))
}

// ---------- SKILL.md frontmatter (pure) ----------

/// `description:` from SKILL.md frontmatter (plain, quoted, or `|` / `>` block).
pub fn frontmatter_desc(md: &str) -> String {
    let mut lines = md.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    let mut lines = lines.take_while(|l| l.trim() != "---").peekable();
    while let Some(l) = lines.next() {
        let Some(rest) = l.strip_prefix("description:") else { continue };
        let rest = rest.trim();
        if matches!(rest, "" | "|" | ">" | "|-" | ">-") {
            let mut parts = Vec::new();
            while let Some(n) = lines.next_if(|n| n.starts_with(' ') || n.starts_with('\t')) {
                parts.push(n.trim());
            }
            return parts.join(" ");
        }
        return rest.trim_matches(|c| c == '"' || c == '\'').to_string();
    }
    String::new()
}

// ---------- filesystem helpers ----------

pub fn skill_dirs(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn exists_or_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to.parent().unwrap())?;
    fs::rename(from, to)
}

/// Remove a link, file or whole directory.
pub fn remove_any(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(_) => Ok(()),
    }
}

const DISABLED: &str = ".agents/disabled";

/// `source-command-<cmd>-...` skills are ports of ~/.claude/commands/<cmd>.
fn claude_command_for(h: &Path, skill: &str) -> Option<String> {
    let rest = skill.strip_prefix("source-command-")?;
    [h.join(".claude/commands"), h.join(DISABLED).join("claude-commands")]
        .iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|d| rest == d || rest.starts_with(&format!("{d}-")))
        .max_by_key(|d| d.len())
}

pub fn plugin_links(dir: &Path, plugin: &str) -> Vec<PathBuf> {
    let prefix = format!("{plugin}--");
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// Link (or unlink) every skill of a plugin into an agent's skills dir.
pub fn set_links(dir: &Path, plugin: &str, install: &Path, skills: &[String], on: bool) -> io::Result<()> {
    for l in plugin_links(dir, plugin) {
        remove_any(&l)?;
    }
    if on {
        fs::create_dir_all(dir)?;
        for s in skills {
            std::os::unix::fs::symlink(install.join("skills").join(s), dir.join(format!("{plugin}--{s}")))?;
        }
    }
    Ok(())
}

/// Links exist and all point at the plugin's current install.
pub fn links_current(dir: &Path, plugin: &str, install: &Path, skills: &[String]) -> bool {
    let links = plugin_links(dir, plugin);
    links.len() == skills.len()
        && skills.iter().all(|s| fs::read_link(dir.join(format!("{plugin}--{s}"))).is_ok_and(|t| t == install.join("skills").join(s)))
}

// ---------- loading ----------

pub fn load_entries(cx: &Ctx) -> Vec<Entry> {
    let h = &cx.home;
    let hub = cx.hub();
    let lock = read_json(&h.join(".agents/.skill-lock.json"));
    let codex_off = codex_disabled_skills(&cx.codex);
    let gemini_off = cx.gemini_disabled();
    let mut out = Vec::new();

    for name in skill_dirs(&hub) {
        let link = h.join(".claude/skills").join(&name);
        let claude_cmd = claude_command_for(h, &name);
        let claude_copy = fs::symlink_metadata(&link).is_ok_and(|m| m.is_dir()) || h.join(DISABLED).join("claude-skills").join(&name).exists();
        let mut vis = BTreeMap::new();
        for &p in &cx.agents {
            let on = match p {
                P::Claude => match &claude_cmd {
                    Some(c) => h.join(".claude/commands").join(c).exists(),
                    None => exists_or_link(&link),
                },
                P::Codex => !codex_off.contains(&name),
                P::Omp => !cx.omp_ignored.contains(&name),
                P::OpenCode => {
                    vis.insert(p, Vis::Always);
                    continue;
                }
                P::Gemini => !gemini_off.contains(&name),
            };
            vis.insert(p, if on { Vis::On } else { Vis::Off });
        }
        let src = &lock["skills"][&name]["source"];
        let path = hub.join(&name);
        out.push(Entry {
            kind: Kind::Skill,
            desc: frontmatter_desc(&fs::read_to_string(path.join("SKILL.md")).unwrap_or_default()),
            source: src.as_str().unwrap_or("local").to_string(),
            in_lock: !src.is_null(),
            skills: vec![name.clone()],
            id: name,
            vis,
            path,
            dups: vec![],
            claude_copy,
            claude_cmd,
            modes: BTreeMap::new(),
            omp_native: false,
            mcp: BTreeMap::new(),
        });
    }

    let installed = read_json(&h.join(".claude/plugins/installed_plugins.json"));
    let markets = read_json(&h.join(".claude/plugins/known_marketplaces.json"));
    let codex_by_repo: HashMap<String, String> = cx
        .codex
        .get("marketplaces")
        .and_then(|m| m.as_table())
        .map(|t| t.iter().filter_map(|(k, v)| Some((repo_of(v.get("source")?.as_str()?), k.to_string()))).collect())
        .unwrap_or_default();
    let oc_plugins: Vec<String> = cx.opencode.as_ref().and_then(|v| v["plugin"].as_array().cloned()).unwrap_or_default().iter().filter_map(|s| s.as_str().map(String::from)).collect();

    if let Some(plugins) = installed["plugins"].as_object() {
        for (pid, installs) in plugins {
            let (name, mkt) = pid.rsplit_once('@').unwrap_or((pid, ""));
            // the copy Claude loads: user-scope install first
            let inst = installs.as_array().into_iter().flatten().max_by_key(|i| i["scope"] == "user");
            let path = PathBuf::from(inst.and_then(|i| i["installPath"].as_str()).unwrap_or_default());
            let skills = skill_dirs(&path.join("skills"));
            let repo = repo_of(markets[mkt]["source"]["repo"].as_str().or(markets[mkt]["source"]["url"].as_str()).unwrap_or(""));
            let claude_on = cx.claude["enabledPlugins"][pid].as_bool() == Some(true);
            let npm = read_json(&path.join("package.json"))["name"].as_str().map(String::from);

            let mut modes = BTreeMap::new();
            // Codex installs Claude-format plugins, unless the repo's own Codex manifest disagrees on the name
            let codex_manifest = read_json(&path.join(".codex-plugin/plugin.json"));
            let codex_ok = !repo.is_empty() && codex_manifest["name"].as_str().is_none_or(|n| n == name);
            modes.insert(
                P::Codex,
                if codex_ok { Mode::Native(format!("{name}@{}", codex_by_repo.get(&repo).map_or(mkt, |s| s.as_str()))) } else { Mode::Links },
            );
            let oc_native = path.join(".opencode").is_dir() && !repo.is_empty();
            modes.insert(P::OpenCode, if oc_native { Mode::Native(format!("{name}@git+https://github.com/{repo}.git")) } else { Mode::Links });
            modes.insert(P::Gemini, Mode::Links);

            let mut vis = BTreeMap::new();
            for &p in &cx.agents {
                // links carry skills only; LSP/agent/hook-only plugins have nothing to share there
                if modes.get(&p) == Some(&Mode::Links) && skills.is_empty() {
                    continue;
                }
                let on = match (p, modes.get(&p)) {
                    (P::Claude, _) => claude_on,
                    (P::Omp, _) => omp_plugin_on(&cx.omp_reg, pid, claude_on),
                    (P::Codex, Some(Mode::Native(id))) => codex_plugin_enabled(&cx.codex, id) == Some(true),
                    (P::OpenCode, Some(Mode::Native(_))) => oc_plugins.iter().any(|s| opencode_spec_matches(s, &repo, npm.as_deref())),
                    (_, _) => p.link_dir(h).is_some_and(|d| !plugin_links(&d, name).is_empty()),
                };
                vis.insert(p, if on { Vis::On } else { Vis::Off });
            }
            out.push(Entry {
                kind: Kind::Plugin,
                id: pid.clone(),
                source: repo,
                in_lock: false,
                desc: read_json(&path.join(".claude-plugin/plugin.json"))["description"].as_str().unwrap_or("").to_string(),
                skills,
                vis,
                path,
                dups: vec![],
                claude_copy: false,
                claude_cmd: None,
                modes,
                omp_native: cx.omp_reg["plugins"][pid].as_array().is_some_and(|es| es.iter().any(|e| e["skillz"] != json!(true))),
                mcp: BTreeMap::new(),
            });
        }
    }
    out.extend(crate::mcp::load(cx));
    mark_dups(&mut out);
    out
}

/// Same plugin from two marketplaces, or a hub skill that a plugin also ships.
pub fn mark_dups(out: &mut [Entry]) {
    let snapshot: Vec<(Kind, String, String, Vec<String>)> = out.iter().map(|e| (e.kind, e.id.clone(), e.plugin_name().to_string(), e.skills.clone())).collect();
    for e in out.iter_mut() {
        let me = e.plugin_name().to_string();
        e.dups = snapshot
            .iter()
            .filter(|(k, id, b, skills)| {
                *id != e.id
                    && match (e.kind, k) {
                        (Kind::Plugin, Kind::Plugin) => *b == me,
                        (Kind::Skill, Kind::Plugin) => skills.contains(&e.id),
                        (Kind::Plugin, Kind::Skill) => e.skills.contains(id),
                        _ => false,
                    }
            })
            .map(|(_, id, _, _)| id.clone())
            .collect();
    }
}

// ---------- switching ----------

pub type Cmds = Vec<Vec<String>>;

pub fn sv(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn version_of(e: &Entry) -> String {
    e.path.file_name().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Switch one agent for one entry. `remember` records user intent so `sync` won't undo it.
/// Returns CLI commands that must run interactively (installs); config edits are done in place.
pub fn set(cx: &mut Ctx, e: &Entry, p: P, on: bool, remember: bool) -> io::Result<Cmds> {
    let h = cx.home.clone();
    let mut cmds = Vec::new();
    match (p, e.kind) {
        (_, _) if e.vis.get(&p) == Some(&Vis::Always) => {
            return Err(io::Error::other(format!("{} always loads ~/.agents/skills and has no per-skill switch", p.name())));
        }
        (_, Kind::Mcp) => crate::mcp::set(cx, e, p, on)?,
        (P::Claude, Kind::Skill) => {
            if let Some(cmd) = &e.claude_cmd {
                let (live, parked) = (h.join(".claude/commands").join(cmd), h.join(DISABLED).join("claude-commands").join(cmd));
                if on && parked.exists() && !live.exists() {
                    move_dir(&parked, &live)?;
                }
                if !on && live.exists() {
                    move_dir(&live, &parked)?;
                }
            } else {
                let link = h.join(".claude/skills").join(&e.id);
                let parked = h.join(DISABLED).join("claude-skills").join(&e.id);
                if on && !exists_or_link(&link) {
                    if parked.exists() {
                        move_dir(&parked, &link)?;
                    } else {
                        fs::create_dir_all(link.parent().unwrap())?;
                        std::os::unix::fs::symlink(Path::new("../../.agents/skills").join(&e.id), &link)?;
                    }
                } else if !on && exists_or_link(&link) {
                    if is_link(&link) { fs::remove_file(&link)? } else { move_dir(&link, &parked)? }
                }
            }
        }
        (P::Claude, Kind::Plugin) => {
            let omp_now = e.vis.get(&P::Omp).map(|v| *v == Vis::On);
            if !cx.claude["enabledPlugins"].is_object() {
                cx.claude["enabledPlugins"] = json!({});
            }
            cx.claude["enabledPlugins"][&e.id] = json!(on);
            cx.touch("claude");
            // omp follows Claude by default; pin omp so this stays a Claude-only change
            if let Some(omp_on) = omp_now {
                omp_set_plugin(&mut cx.omp_reg, &e.id, omp_on, on, &e.path.to_string_lossy(), &version_of(e));
                cx.touch("omp_reg");
            }
        }
        (P::Codex, Kind::Skill) => {
            codex_set_skill(&mut cx.codex, &e.id, on);
            cx.touch("codex");
        }
        (P::Omp, Kind::Skill) => {
            if on { cx.omp_ignored.remove(&e.id); } else { cx.omp_ignored.insert(e.id.clone()); }
        }
        (P::Gemini, Kind::Skill) => cx.set_gemini_disabled(&e.id, !on),
        (P::Omp, Kind::Plugin) => {
            let claude_on = e.vis.get(&P::Claude) == Some(&Vis::On);
            omp_set_plugin(&mut cx.omp_reg, &e.id, on, claude_on, &e.path.to_string_lossy(), &version_of(e));
            cx.touch("omp_reg");
        }
        (P::Codex, Kind::Plugin) => match e.modes.get(&p) {
            Some(Mode::Native(id)) => {
                if codex_set_plugin(&mut cx.codex, id, on) {
                    cx.touch("codex");
                } else if on {
                    let xm = id.rsplit_once('@').map_or("", |x| x.1);
                    if cx.codex.get("marketplaces").and_then(|m| m.get(xm)).is_none() {
                        cmds.push(sv(&["codex", "plugin", "marketplace", "add", &e.source]));
                    }
                    cmds.push(sv(&["codex", "plugin", "add", id]));
                }
            }
            _ => set_links(&p.link_dir(&h).unwrap(), e.plugin_name(), &e.path, &e.skills, on)?,
        },
        (P::OpenCode, Kind::Plugin) => match e.modes.get(&p) {
            Some(Mode::Native(spec)) => {
                let Some(oc) = cx.opencode.as_mut() else {
                    return Err(io::Error::other("opencode.json has comments (JSONC); edit its `plugin` list by hand"));
                };
                let npm = read_json(&e.path.join("package.json"))["name"].as_str().map(String::from);
                let mut list: Vec<Value> = oc["plugin"].as_array().cloned().unwrap_or_default();
                list.retain(|s| !s.as_str().is_some_and(|s| opencode_spec_matches(s, &e.source, npm.as_deref())));
                if on {
                    list.push(json!(spec));
                }
                oc["plugin"] = json!(list);
                cx.touch("opencode");
            }
            _ => set_links(&p.link_dir(&h).unwrap(), e.plugin_name(), &e.path, &e.skills, on)?,
        },
        (P::Gemini, Kind::Plugin) => set_links(&p.link_dir(&h).unwrap(), e.plugin_name(), &e.path, &e.skills, on)?,
        (P::OpenCode, Kind::Skill) => unreachable!("handled by Vis::Always"),
    }
    if remember {
        cx.remember(p, &e.id, !on);
    }
    Ok(cmds)
}

// ---------- total removal ----------

/// Remove an entry from every agent, every config rule, every cache and the hub itself.
/// Returns the CLI commands (uninstallers) to run; file/config cleanup happens here.
pub fn purge(cx: &mut Ctx, e: &Entry) -> io::Result<Cmds> {
    let h = cx.home.clone();
    let mut cmds = Vec::new();
    match e.kind {
        Kind::Skill => {
            let n = &e.id;
            let (hub_dir, claude_link) = (cx.hub().join(n), h.join(".claude/skills").join(n));
            if e.in_lock {
                // npx must see the files to drop its lock entry; leftovers go after it
                cmds.push(sv(&["npx", "-y", "skills", "remove", n, "-g", "-y"]));
                cmds.push(sv(&["rm", "-rf", &hub_dir.to_string_lossy(), &claude_link.to_string_lossy()]));
            } else {
                remove_any(&hub_dir)?;
                remove_any(&claude_link)?;
            }
            for d in [
                h.join(DISABLED).join("claude-skills").join(n),
                h.join(".codex/skills").join(n),
                h.join(".gemini/skills").join(n),
                h.join(".config/opencode/skills").join(n),
                h.join(".omp/agent/skills").join(n),
                h.join(".pi/agent/skills").join(n),
            ] {
                remove_any(&d)?;
            }
            codex_set_skill(&mut cx.codex, n, true);
            cx.touch("codex");
            cx.omp_ignored.remove(n);
            if cx.gemini_disabled().contains(n) {
                cx.set_gemini_disabled(n, false);
            }
        }
        Kind::Plugin => {
            let name = e.plugin_name().to_string();
            cmds.push(sv(&["claude", "plugin", "uninstall", &e.id]));
            if let Some(Mode::Native(id)) = e.modes.get(&P::Codex) {
                if codex_plugin_enabled(&cx.codex, id).is_some() {
                    cmds.push(sv(&["codex", "plugin", "remove", id]));
                }
            }
            if e.omp_native {
                cmds.push(sv(&["omp", "plugin", "uninstall", &e.id]));
            } else if let Some(m) = cx.omp_reg["plugins"].as_object_mut() {
                if m.remove(&e.id).is_some() {
                    cx.touch("omp_reg");
                }
            }
            for p in [P::Codex, P::OpenCode, P::Gemini] {
                if let Some(d) = p.link_dir(&h) {
                    for l in plugin_links(&d, &name) {
                        remove_any(&l)?;
                    }
                }
            }
            if let Some(oc) = cx.opencode.as_mut() {
                let npm = read_json(&e.path.join("package.json"))["name"].as_str().map(String::from);
                if let Some(list) = oc["plugin"].as_array_mut() {
                    let before = list.len();
                    list.retain(|s| !s.as_str().is_some_and(|s| opencode_spec_matches(s, &e.source, npm.as_deref())));
                    if list.len() != before {
                        cx.touch("opencode");
                    }
                }
            }
            if let Some(m) = cx.claude["enabledPlugins"].as_object_mut() {
                if m.remove(&e.id).is_some() {
                    cx.touch("claude");
                }
            }
            // every cached version of it
            let (pname, mkt) = e.id.rsplit_once('@').unwrap_or((&e.id, ""));
            cmds.push(sv(&["rm", "-rf", &h.join(".claude/plugins/cache").join(mkt).join(pname).to_string_lossy()]));
        }
        Kind::Mcp => crate::mcp::purge(cx, e),
    }
    cx.forget(&e.id);
    Ok(cmds)
}

pub fn update_cmds(cx: &Ctx, e: &Entry) -> Cmds {
    match e.kind {
        Kind::Skill if e.in_lock => vec![sv(&["npx", "-y", "skills", "update", &e.id, "-g", "-y"])],
        Kind::Skill if e.path.join(".git").exists() => vec![sv(&["git", "-C", &e.path.to_string_lossy(), "pull", "--ff-only"])],
        Kind::Skill | Kind::Mcp => vec![],
        Kind::Plugin => {
            let mut v = vec![sv(&["claude", "plugin", "update", &e.id])];
            if e.omp_native && cx.agents.contains(&P::Omp) {
                v.push(sv(&["omp", "plugin", "upgrade"]));
            }
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_skill_toggle_roundtrip() {
        let mut doc: DocumentMut = "model = \"x\"\n[plugins.\"a@b\"]\nenabled = true\n".parse().unwrap();
        codex_set_skill(&mut doc, "foo", true); // no-op without rules
        assert!(!doc.to_string().contains("skills"));
        codex_set_skill(&mut doc, "foo", false);
        codex_set_skill(&mut doc, "bar", false);
        assert_eq!(codex_disabled_skills(&doc), BTreeSet::from(["foo".into(), "bar".into()]));
        codex_set_skill(&mut doc, "foo", true);
        assert_eq!(codex_disabled_skills(&doc), BTreeSet::from(["bar".into()]));
        assert!(doc.to_string().parse::<DocumentMut>().is_ok());
        assert!(doc.to_string().contains("model = \"x\""));
    }

    #[test]
    fn codex_plugin_toggle_only_when_installed() {
        let mut doc: DocumentMut = "[plugins.\"a@b\"]\nenabled = true\n".parse().unwrap();
        assert!(codex_set_plugin(&mut doc, "a@b", false));
        assert_eq!(codex_plugin_enabled(&doc, "a@b"), Some(false));
        assert!(!codex_set_plugin(&mut doc, "missing@b", true));
    }

    #[test]
    fn omp_plugin_registry_overrides() {
        let mut reg = json!({});
        assert!(omp_plugin_on(&reg, "a@m", true));
        omp_set_plugin(&mut reg, "a@m", false, true, "/p", "1");
        assert!(!omp_plugin_on(&reg, "a@m", true));
        omp_set_plugin(&mut reg, "a@m", true, true, "/p", "1");
        assert!(reg["plugins"].get("a@m").is_none());
        omp_set_plugin(&mut reg, "a@m", true, false, "/p", "1");
        assert!(omp_plugin_on(&reg, "a@m", false));
        let mut reg = json!({"plugins": {"n@m": [{"installPath": "/x"}]}});
        omp_set_plugin(&mut reg, "n@m", false, true, "/p", "1");
        assert_eq!(reg["plugins"]["n@m"][0]["installPath"], "/x");
        assert!(!omp_plugin_on(&reg, "n@m", true));
        omp_set_plugin(&mut reg, "n@m", true, true, "/p", "1");
        assert!(omp_plugin_on(&reg, "n@m", true));
    }

    #[test]
    fn opencode_specs() {
        assert!(opencode_spec_matches("superpowers-plus@git+https://github.com/xhyqaq/superpowers-plus.git", "xhyqaq/superpowers-plus", None));
        assert!(opencode_spec_matches("@dietrichgebert/ponytail", "dietrichgebert/ponytail", Some("@dietrichgebert/ponytail")));
        assert!(opencode_spec_matches("@dietrichgebert/ponytail@5.2.0", "", Some("@dietrichgebert/ponytail")));
        assert!(!opencode_spec_matches("other-plugin", "a/b", Some("x")));
    }

    #[test]
    fn links_roundtrip() {
        let t = std::env::temp_dir().join(format!("skillz-links-{}", std::process::id()));
        let install = t.join("plugin/1.0");
        fs::create_dir_all(install.join("skills/a")).unwrap();
        fs::create_dir_all(install.join("skills/b")).unwrap();
        let dir = t.join("agent/skills");
        let skills = vec!["a".to_string(), "b".to_string()];
        set_links(&dir, "p", &install, &skills, true).unwrap();
        assert!(links_current(&dir, "p", &install, &skills));
        assert!(!links_current(&dir, "p", &t.join("plugin/2.0"), &skills));
        set_links(&dir, "p", &install, &skills, false).unwrap();
        assert!(plugin_links(&dir, "p").is_empty());
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn state_migrates() {
        let mut s = json!({"claude_skills_off": ["x"], "codex_plugins_off": []});
        migrate_state(&mut s);
        assert_eq!(s, json!({"off": {"claude": ["x"], "codex": []}}));
    }

    #[test]
    fn frontmatter_desc_variants() {
        assert_eq!(frontmatter_desc("---\nname: a\ndescription: \"Hi there\"\n---\nbody"), "Hi there");
        assert_eq!(frontmatter_desc("---\ndescription: |\n  line one\n  line two\nname: x\n---"), "line one line two");
        assert_eq!(frontmatter_desc("no frontmatter"), "");
    }

    #[test]
    fn repo_normalizes() {
        assert_eq!(repo_of("https://github.com/A/b.git"), "a/b");
        assert_eq!(repo_of("A/b"), "a/b");
    }
}
