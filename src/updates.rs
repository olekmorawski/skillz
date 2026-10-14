//! Which installed skills and plugins have a newer version upstream. Network only, no installs.
use crate::model::{read_json, repo_of};
use crate::net;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    thread,
};

/// Ids (skill names, plugin ids) with an update available. Unknown counts as up to date.
pub fn check(home: &Path) -> BTreeSet<String> {
    let (skills, plugins) = thread::scope(|s| {
        let a = s.spawn(|| skills(home));
        let b = s.spawn(|| plugins(home));
        (a.join().unwrap_or_default(), b.join().unwrap_or_default())
    });
    skills.into_iter().chain(plugins).collect()
}

/// Hub skills from GitHub: the lock keeps the git tree hash of each skill folder; one tree listing per repo.
fn skills(home: &Path) -> BTreeSet<String> {
    let lock = read_json(&home.join(".agents/.skill-lock.json"));
    let mut by_repo: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    for (name, v) in lock["skills"].as_object().into_iter().flatten() {
        let (Some(src), Some(hash)) = (v["source"].as_str(), v["skillFolderHash"].as_str()) else { continue };
        if v["sourceType"] != "github" {
            continue;
        }
        let folder = Path::new(v["skillPath"].as_str().unwrap_or("SKILL.md")).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        by_repo.entry(repo_of(src)).or_default().push((name.clone(), folder, hash.to_string()));
    }
    thread::scope(|s| {
        let jobs: Vec<_> = by_repo
            .iter()
            .map(|(repo, items)| {
                s.spawn(move || {
                    let Some(tree) = net::github(&format!("repos/{repo}/git/trees/HEAD?recursive=1")) else { return vec![] };
                    items.iter().filter(|(_, folder, hash)| latest_tree(&tree, folder).is_some_and(|l| l != *hash)).map(|(n, _, _)| n.clone()).collect()
                })
            })
            .collect();
        jobs.into_iter().flat_map(|j| j.join().unwrap_or_default()).collect()
    })
}

/// Tree hash of `folder` ("" = repo root) in a `git/trees?recursive=1` response.
pub fn latest_tree(tree: &Value, folder: &str) -> Option<String> {
    if folder.is_empty() {
        return tree["sha"].as_str().map(String::from);
    }
    tree["tree"].as_array()?.iter().find(|t| t["path"] == folder && t["type"] == "tree").and_then(|t| t["sha"].as_str()).map(String::from)
}

/// Claude plugins: the marketplace's (or plugin's own) declared version vs the installed one.
/// Plugins versioned only by commit (no version anywhere) can't be compared and are skipped.
fn plugins(home: &Path) -> BTreeSet<String> {
    let installed = read_json(&home.join(".claude/plugins/installed_plugins.json"));
    let markets = read_json(&home.join(".claude/plugins/known_marketplaces.json"));
    let list: Vec<(String, String, String, String)> = installed["plugins"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(pid, ins)| {
            let (name, mkt) = pid.rsplit_once('@')?;
            let inst = ins.as_array()?.iter().max_by_key(|i| i["scope"] == "user")?;
            let repo = markets[mkt]["source"]["repo"].as_str()?;
            Some((pid.clone(), name.to_string(), repo.to_string(), inst["version"].as_str()?.to_string()))
        })
        .collect();
    let repos: BTreeSet<&String> = list.iter().map(|x| &x.2).collect();
    let catalogs: BTreeMap<&String, Value> = thread::scope(|s| {
        let jobs: Vec<_> = repos.iter().map(|r| (*r, s.spawn(move || net::raw_json(r, ".claude-plugin/marketplace.json")))).collect();
        jobs.into_iter().filter_map(|(r, j)| Some((r, j.join().ok()??))).collect()
    });
    thread::scope(|s| {
        let jobs: Vec<_> = list
            .iter()
            .filter_map(|(pid, name, repo, ver)| {
                let ent = catalogs.get(repo)?["plugins"].as_array()?.iter().find(|p| p["name"] == name.as_str())?.clone();
                Some(s.spawn(move || {
                    let latest = ent["version"].as_str().map(String::from).or_else(|| match &ent["source"] {
                        Value::String(path) => net::raw_json(repo, &format!("{path}/.claude-plugin/plugin.json"))?["version"].as_str().map(String::from),
                        src => net::raw_json(&repo_of(src["repo"].as_str()?), ".claude-plugin/plugin.json")?["version"].as_str().map(String::from),
                    });
                    latest.filter(|l| l != ver).map(|_| pid.clone())
                }))
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tree_lookup() {
        let t = json!({"sha": "root", "tree": [{"path": "skills/a", "type": "tree", "sha": "x"}, {"path": "skills/a/SKILL.md", "type": "blob", "sha": "y"}]});
        assert_eq!(latest_tree(&t, "").as_deref(), Some("root"));
        assert_eq!(latest_tree(&t, "skills/a").as_deref(), Some("x"));
        assert_eq!(latest_tree(&t, "skills/b"), None);
    }
}
