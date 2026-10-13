//! `skillz sync`: make every agent see what it should, without undoing switches made by hand.
use crate::model::*;
use std::{collections::BTreeMap, fs, io, process::Command};

pub struct Report {
    pub lines: Vec<String>,
    pub cmds: Cmds,
}

/// Plan (and with `apply`, perform) the sync. Commands that need a terminal are returned, not run.
pub fn sync(cx: &mut Ctx, apply: bool) -> io::Result<Report> {
    let mut lines = Vec::new();
    let mut cmds = Vec::new();
    let entries = load_entries(cx);
    let h = cx.home.clone();

    // omp ignores Claude's plugins/skills unless the provider is enabled
    if cx.agents.contains(&P::Omp) && has_cmd("omp") {
        let out = Command::new("omp").args(["config", "get", "enabledProviders", "--json"]).output()?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        let list: Vec<String> = v["value"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect();
        if !list.iter().any(|s| s == "claude") {
            lines.push("omp doesn't read Claude plugins yet → enabledProviders += claude".into());
            if apply {
                let mut l = list.clone();
                l.push("claude".into());
                Command::new("omp").args(["config", "set", "enabledProviders", &serde_json::to_string(&l).unwrap()]).output()?;
            }
        }
    }

    for e in &entries {
        if !e.dups.is_empty() && e.kind == Kind::Plugin {
            lines.push(format!("DUPLICATE {} also provided by {} - keep one (skillz rm <id>)", e.id, e.dups.join(", ")));
        }
        match e.kind {
            // hub skills: Claude needs a link; the rest read ~/.agents/skills natively
            Kind::Skill => {
                if e.vis.get(&P::Claude) == Some(&Vis::Off) && !cx.is_off(P::Claude, &e.id) && e.claude_cmd.is_none() && !e.claude_copy {
                    lines.push(format!("{}: link into Claude", e.id));
                    if apply {
                        set(cx, e, P::Claude, true, false)?;
                    }
                }
            }
            // plugins enabled in Claude follow to every agent the user didn't switch off
            Kind::Plugin if e.vis.get(&P::Claude) == Some(&Vis::On) => {
                for &p in &cx.agents.clone() {
                    if p == P::Claude || p == P::Omp || cx.is_off(p, &e.id) {
                        continue;
                    }
                    let links = e.modes.get(&p) == Some(&Mode::Links);
                    let stale = links && e.vis.get(&p) == Some(&Vis::On) && !links_current(&p.link_dir(&h).unwrap(), e.plugin_name(), &e.path, &e.skills);
                    if e.vis.get(&p) == Some(&Vis::Off) || stale {
                        lines.push(format!("{}: {} in {}", e.id, if stale { "refresh links" } else { "enable" }, p.name()));
                        if apply {
                            cmds.extend(set(cx, e, p, true, false)?);
                        }
                    }
                }
            }
            Kind::Plugin => {}
        }
    }

    // links left behind by uninstalled plugins or deleted hub skills
    let mut dirs: BTreeMap<&str, std::path::PathBuf> = BTreeMap::new();
    dirs.insert("Claude", h.join(".claude/skills"));
    for p in [P::Codex, P::OpenCode, P::Gemini] {
        dirs.insert(p.name(), p.link_dir(&h).unwrap());
    }
    for (agent, d) in dirs {
        for ent in fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = ent.path();
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) && !path.exists() {
                lines.push(format!("{agent}: remove broken link {}", ent.file_name().to_string_lossy()));
                if apply {
                    fs::remove_file(&path)?;
                }
            }
        }
    }
    if apply {
        cx.save()?;
    }
    Ok(Report { lines, cmds })
}

/// Everything `skillz update` runs, in order; a sync follows.
pub fn update_all_cmds(cx: &Ctx) -> Cmds {
    let mut v = Vec::new();
    if cx.home.join(".agents/.skill-lock.json").exists() {
        v.push(sv(&["npx", "-y", "skills", "update", "-g", "-y"]));
    }
    for e in fs::read_dir(cx.hub()).into_iter().flatten().flatten() {
        let p = e.path().canonicalize().unwrap_or(e.path());
        if p.join(".git").exists() {
            v.push(sv(&["git", "-C", &p.to_string_lossy(), "pull", "--ff-only", "-q"]));
        }
    }
    if cx.agents.contains(&P::Claude) && has_cmd("claude") {
        v.push(sv(&["claude", "plugin", "marketplace", "update"]));
        if let Some(m) = cx.claude["enabledPlugins"].as_object() {
            for (id, on) in m {
                if on == true {
                    v.push(sv(&["claude", "plugin", "update", id]));
                }
            }
        }
    }
    if cx.agents.contains(&P::Codex) && has_cmd("codex") {
        v.push(sv(&["codex", "plugin", "marketplace", "upgrade"]));
    }
    if cx.agents.contains(&P::Omp) && has_cmd("omp") {
        v.push(sv(&["omp", "plugin", "upgrade"]));
    }
    v
}
