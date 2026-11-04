//! Pi's global hub skill switches. Never move or link shared skill files.
use crate::model::Vis;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

pub struct Pi {
    pub dir: PathBuf,
    home: PathBuf,
    settings: Option<Value>,
    pending: BTreeMap<String, Option<bool>>,
}

impl Pi {
    pub fn load(home: &Path, custom: Option<&str>) -> Self {
        let dir = custom.filter(|s| !s.is_empty()).map(|s| PathBuf::from(expand(s, home))).unwrap_or_else(|| home.join(".pi/agent"));
        Self { settings: read_settings(&dir).ok(), dir, home: home.into(), pending: BTreeMap::new() }
    }

    pub fn visibility(&self, name: &str) -> Vis {
        let Some(settings) = &self.settings else { return Vis::Unknown };
        match enabled(settings, name, &self.home) {
            Some(true) => Vis::On,
            Some(false) => Vis::Off,
            None => Vis::Unknown,
        }
    }

    pub fn set(&mut self, name: &str, on: bool) -> io::Result<()> {
        let settings = self.settings.as_mut().ok_or_else(|| io::Error::other("Pi settings.json is unreadable or invalid; left unchanged"))?;
        check_exact_filters(settings)?;
        toggle(settings, name, on, &self.home);
        self.pending.insert(name.into(), Some(on));
        Ok(())
    }

    pub fn forget(&mut self, name: &str) -> io::Result<()> {
        let settings = self.settings.as_mut().ok_or_else(|| io::Error::other("Pi settings.json is unreadable or invalid; left unchanged"))?;
        remove_exact(settings, name, &self.home);
        self.pending.insert(name.into(), None);
        Ok(())
    }

    pub fn save(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        // Re-read and replay only our exact overrides: preserve concurrent settings edits.
        let mut current = read_settings(&self.dir)?;
        let before = current.clone();
        for (name, on) in &self.pending {
            if let Some(on) = on {
                check_exact_filters(&current)?;
                toggle(&mut current, name, *on, &self.home);
            } else {
                remove_exact(&mut current, name, &self.home);
            }
        }
        if current != before {
            let path = self.dir.join("settings.json");
            // Do not replace a symlink (or write through one to an unexpected file).
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(io::Error::other("Pi settings.json is a symlink; left unchanged"));
            }
            let tmp = self.dir.join(format!(".settings.json.skillz-{}", std::process::id()));
            use std::io::Write;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            let result = (|| {
                if let Ok(meta) = fs::metadata(&path) {
                    file.set_permissions(meta.permissions())?;
                }
                file.write_all((serde_json::to_string_pretty(&current)? + "\n").as_bytes())?;
                file.sync_all()?;
                fs::rename(&tmp, &path)
            })();
            if result.is_err() {
                let _ = fs::remove_file(&tmp);
            }
            result?;
        }
        self.settings = Some(current);
        self.pending.clear();
        Ok(())
    }
}

fn read_settings(dir: &Path) -> io::Result<Value> {
    let value: Value = match fs::read_to_string(dir.join("settings.json")) {
        Ok(text) => serde_json::from_str(&text)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e),
    };
    if !value.is_object() || value.get("skills").is_some_and(|v| !v.as_array().is_some_and(|a| a.iter().all(Value::is_string))) {
        return Err(io::Error::other("Pi settings.json must be an object with a string-array skills setting"));
    }
    Ok(value)
}

fn expand(s: &str, home: &Path) -> String {
    if s == "~" {
        home.to_string_lossy().into_owned()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest).to_string_lossy().into_owned()
    } else {
        s.into()
    }
}

// Hub autoload filters use ~/.agents as their base, NOT the Pi agent directory.
fn candidates(name: &str, home: &Path) -> [String; 6] {
    let dir = format!("skills/{name}");
    let abs = home.join(".agents").join(&dir).to_string_lossy().into_owned();
    [format!("{dir}/SKILL.md"), "SKILL.md".into(), format!("{abs}/SKILL.md"), dir, name.into(), abs]
}

fn exact(pattern: &str, paths: &[String; 6], home: &Path) -> bool {
    let p = expand(pattern.strip_prefix("./").unwrap_or(pattern), home);
    [0, 2, 3, 5].iter().any(|&i| paths[i] == p)
}

fn enabled(settings: &Value, name: &str, home: &Path) -> Option<bool> {
    let paths = candidates(name, home);
    let entries: Vec<&str> = settings["skills"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    // Explicit resource paths may discover the same file under a different base.
    // Do not guess its effective precedence against hub autoloading.
    if check_exact_filters(settings).is_err() {
        return None;
    }
    let mut result = Some(true);
    for entry in &entries {
        if let Some(pattern) = entry.strip_prefix('!') {
            let pattern = expand(pattern, home);
            let matches: Vec<_> = paths.iter().map(|p| glob(&pattern, p)).collect();
            if matches.contains(&Some(true)) {
                result = Some(false);
            } else if matches.contains(&None) && result != Some(false) {
                result = None;
            }
        }
    }
    // Pi groups overrides by type; array order does not change precedence.
    if entries.iter().any(|p| p.strip_prefix('+').is_some_and(|p| exact(p, &paths, home))) {
        result = Some(true);
    }
    if entries.iter().any(|p| p.strip_prefix('-').is_some_and(|p| exact(p, &paths, home))) {
        result = Some(false);
    }
    result
}

fn check_exact_filters(settings: &Value) -> io::Result<()> {
    if settings["skills"].as_array().into_iter().flatten().filter_map(Value::as_str).any(|s| {
        !s.starts_with(['!', '+', '-']) || s.strip_prefix('+').or_else(|| s.strip_prefix('-')).is_some_and(|p| p.starts_with("file:") || p.contains('\\') || unusual_path(p))
    }) {
        return Err(io::Error::other("Pi has explicit skill paths or unsupported exact filters; use pi config to switch this skill"));
    }
    Ok(())
}

fn remove_exact(settings: &mut Value, name: &str, home: &Path) {
    let paths = candidates(name, home);
    if let Some(entries) = settings["skills"].as_array_mut() {
        entries.retain(|v| !v.as_str().and_then(|s| s.strip_prefix('+').or_else(|| s.strip_prefix('-'))).is_some_and(|p| exact(p, &paths, home)));
    }
}

fn toggle(settings: &mut Value, name: &str, on: bool, home: &Path) {
    if enabled(settings, name, home) == Some(on) {
        return;
    }
    let paths = candidates(name, home);
    let mut entries = settings["skills"].as_array().cloned().unwrap_or_default();
    entries.retain(|v| !v.as_str().and_then(|s| s.strip_prefix('+').or_else(|| s.strip_prefix('-'))).is_some_and(|p| exact(p, &paths, home)));
    // Literal paths avoid interpreting skill names as globs; leave all !patterns intact.
    entries.push(json!(format!("{}{}/SKILL.md", if on { "+" } else { "-" }, paths[5])));
    settings["skills"] = json!(entries);
}

fn unusual_path(path: &str) -> bool {
    path.contains("//") || path.split('/').any(|part| part == "..") || path.contains("/./")
}

/// Conservative subset of minimatch: literals, *, ?, and whole-component **.
/// Complex minimatch syntax stays unknown, never silently treated as enabled.
fn glob(pattern: &str, path: &str) -> Option<bool> {
    if pattern.len() > 256 || path.len() > 4096 {
        return None;
    }
    if pattern.contains(['[', ']', '{', '}', '(', ')', '\\'])
        || pattern.starts_with('!')
        || pattern.starts_with("file:")
        || unusual_path(pattern)
        || (pattern.contains('?') && !path.is_ascii())
    {
        return None;
    }
    if pattern.starts_with('#') {
        return Some(false);
    }
    fn segment(p: &[char], s: &[char]) -> bool {
        let mut row = vec![false; s.len() + 1];
        row[0] = true;
        for c in p {
            let mut next = vec![false; s.len() + 1];
            next[0] = *c == '*' && row[0];
            for j in 1..=s.len() {
                next[j] = if *c == '*' { row[j] || next[j - 1] } else { row[j - 1] && (*c == '?' || *c == s[j - 1]) };
            }
            row = next;
        }
        row[s.len()]
    }
    fn parts(p: &[&str], s: &[&str]) -> bool {
        let mut row = vec![false; s.len() + 1];
        row[0] = true;
        for part in p {
            let mut next = vec![false; s.len() + 1];
            next[0] = *part == "**" && row[0];
            for j in 1..=s.len() {
                next[j] = if *part == "**" {
                    row[j] || (!s[j - 1].starts_with('.') && next[j - 1])
                } else {
                    row[j - 1] && (!s[j - 1].starts_with('.') || part.starts_with('.')) && segment(&part.chars().collect::<Vec<_>>(), &s[j - 1].chars().collect::<Vec<_>>())
                };
            }
            row = next;
        }
        row[s.len()]
    }
    Some(parts(&pattern.split('/').collect::<Vec<_>>(), &path.split('/').collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_filters_use_agents_base_and_type_precedence() {
        let home = Path::new("/home/tester");
        for pattern in ["!foo", "!skills/foo", "!skills/foo/SKILL.md", "!SKILL.md", "!~/.agents/skills/foo", "!f*", "!skills/**/SKILL.md", "!f??"] {
            assert_eq!(enabled(&json!({"skills": [pattern]}), "foo", home), Some(false), "{pattern}");
        }
        assert_eq!(enabled(&json!({"skills": ["-foo"]}), "foo", home), Some(true)); // exact is not a basename match
        assert_eq!(enabled(&json!({"skills": ["!foo", "+skills/foo", "-skills/foo/SKILL.md"]}), "foo", home), Some(false));
        assert_eq!(enabled(&json!({"skills": ["-skills/foo", "+skills/foo/SKILL.md", "!foo"]}), "foo", home), Some(false));
        assert_eq!(enabled(&json!({"skills": ["+./skills/foo", "!foo"]}), "foo", home), Some(true));
        assert_eq!(enabled(&json!({"skills": ["!{foo,bar}"]}), "foo", home), None);
        assert_eq!(enabled(&json!({"skills": ["!{foo,bar}", "+skills/foo"]}), "foo", home), Some(true));
        assert_eq!(enabled(&json!({"skills": ["/extra/skills"]}), "foo", home), None);
    }

    #[test]
    fn exact_switches_preserve_globs_other_skills_and_literal_names() {
        let home = Path::new("/home/tester");
        let mut settings = json!({"theme": "dark", "skills": ["!f*", "-skills/bar", "-./skills/foo"]});
        toggle(&mut settings, "foo", true, home);
        assert_eq!(enabled(&settings, "foo", home), Some(true));
        assert_eq!(enabled(&settings, "bar", home), Some(false));
        assert_eq!(settings["skills"][0], "!f*");
        assert_eq!(settings["theme"], "dark");
        let once = settings.clone();
        toggle(&mut settings, "foo", true, home);
        assert_eq!(settings, once);
        for name in ["foo", "literal[1]*", "with space", "café"] {
            toggle(&mut settings, name, false, home);
            assert_eq!(enabled(&settings, name, home), Some(false));
            toggle(&mut settings, name, true, home);
            assert_eq!(enabled(&settings, name, home), Some(true));
        }
        remove_exact(&mut settings, "foo", home);
        assert_eq!(enabled(&settings, "foo", home), Some(false)); // original glob remains
    }

    #[test]
    fn save_replays_on_fresh_settings_and_refuses_broken_or_symlinked_files() {
        let home = std::env::temp_dir().join(format!("skillz-pi-save-{}", std::process::id()));
        let dir = home.join(".pi/agent");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{}").unwrap();
        let mut pi = Pi::load(&home, None);
        pi.set("foo", false).unwrap();
        fs::write(&path, r#"{"newKey":42,"skills":["!bar"]}"#).unwrap();
        pi.save().unwrap();
        let saved = read_settings(&dir).unwrap();
        assert_eq!(saved["newKey"], 42);
        assert_eq!(saved["skills"][0], "!bar");
        assert_eq!(pi.visibility("foo"), Vis::Off);
        pi.set("foo", true).unwrap();
        fs::write(&path, "{broken").unwrap();
        assert!(pi.save().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
        let target = home.join("linked-settings.json");
        fs::write(&target, serde_json::to_string(&saved).unwrap()).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(pi.save().is_err());
        assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(&target).unwrap()).unwrap(), saved);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn common_globs_and_unknown_syntax() {
        for (p, s, want) in [
            ("foo*", "foobar", true),
            ("foo?", "foo", false),
            ("*", ".hidden", false),
            (".*", ".hidden", true),
            ("skills/**/SKILL.md", "skills/foo/SKILL.md", true),
            ("skills/*", "skills/a/b", false),
            ("#comment", "#comment", false),
        ] {
            assert_eq!(glob(p, s), Some(want), "{p}: {s}");
        }
        for p in ["[ab]", "@(foo|bar)", "{a,b}", r"\*"] {
            assert_eq!(glob(p, "foo"), None);
        }
    }
}
