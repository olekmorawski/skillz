//! Run the real CLI in isolated homes, never mutate process-global HOME in tests.
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    home: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("skillz-pi-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&home).unwrap();
        let f = Self { home };
        f.write(".agents/skills/foo/SKILL.md", "---\nname: foo\ndescription: Test skill\n---\n");
        f.write(".agents/skills/bar/SKILL.md", "---\nname: bar\ndescription: Other skill\n---\n");
        f
    }
    fn write(&self, path: &str, text: &str) {
        let p = self.home.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    fn run(&self, args: &[&str], custom: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_skillz"));
        cmd.args(args).current_dir(&self.home).env("HOME", &self.home).env("PATH", self.home.join("empty-bin")).env_remove("PI_CODING_AGENT_DIR");
        if let Some(custom) = custom {
            cmd.env("PI_CODING_AGENT_DIR", custom);
        }
        cmd.output().unwrap()
    }
    fn ok(&self, args: &[&str], custom: Option<&str>) -> String {
        let o = self.run(args, custom);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    }
    fn settings(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.home.join(".pi/agent/settings.json")).unwrap()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.home).unwrap();
    }
}

#[test]
fn detection_absent_pi_only_and_custom_root() {
    let f = Fixture::new();
    assert!(!f.ok(&["list"], None).contains("Pi"));
    assert!(!f.run(&["disable", "foo", "pi"], None).status.success());
    assert!(!f.home.join(".pi").exists());
    f.write(".pi/agent/settings.json", "{}");
    assert!(!f.run(&["mcp", "add", "example", "nonexistent"], None).status.success());
    let list = f.ok(&["list"], None);
    assert!(list.contains("Pi"));
    assert!(!list.contains("omp"));
    assert!(!list.contains("Claude"));
    // An explicit missing root overrides the standard home, rather than falling back.
    assert!(!f.ok(&["list"], Some("~/missing-root")).contains("Pi"));
    f.write("custom/settings.json", "{\"theme\":\"light\"}");
    f.ok(&["disable", "foo", "pi"], Some("~/custom"));
    assert_eq!(f.settings(), json!({}));
    let custom: Value = serde_json::from_str(&fs::read_to_string(f.home.join("custom/settings.json")).unwrap()).unwrap();
    assert_eq!(custom["theme"], "light");
    assert!(custom["skills"][0].as_str().unwrap().starts_with('-'));
}

#[test]
fn toggles_preserve_sources_other_agents_settings_and_are_idempotent() {
    let f = Fixture::new();
    f.write(".pi/agent/settings.json", r#"{"theme":"dark","custom":{"keep":true},"skills":["!f*","-skills/bar"]}"#);
    f.write(".codex/config.toml", "model = 'test'\n");
    f.write(".gemini/settings.json", "{\"other\":true}");
    let untouched = [".claude/settings.json", ".omp/agent/settings.json", ".config/opencode/opencode.json"];
    for path in untouched {
        f.write(path, "{\"keep\":true}");
    }
    let before_source = fs::read(f.home.join(".agents/skills/foo/SKILL.md")).unwrap();
    f.ok(&["enable", "foo", "pi"], None);
    let on = fs::read(f.home.join(".pi/agent/settings.json")).unwrap();
    assert_eq!(f.settings()["custom"], json!({"keep":true}));
    assert_eq!(f.settings()["skills"][0], "!f*");
    assert_eq!(f.settings()["skills"][1], "-skills/bar");
    assert!(f.settings()["skills"][2].as_str().unwrap().starts_with('+'));
    f.ok(&["enable", "foo", "pi"], None);
    assert_eq!(fs::read(f.home.join(".pi/agent/settings.json")).unwrap(), on);
    f.ok(&["disable", "foo", "pi"], None);
    let off = fs::read(f.home.join(".pi/agent/settings.json")).unwrap();
    f.ok(&["disable", "foo", "pi"], None);
    assert_eq!(fs::read(f.home.join(".pi/agent/settings.json")).unwrap(), off);
    assert!(f.settings()["skills"][2].as_str().unwrap().starts_with('-'));
    f.ok(&["enable", "foo", "pi"], None);
    assert_eq!(fs::read(f.home.join(".pi/agent/settings.json")).unwrap(), on);
    assert_eq!(fs::read(f.home.join(".agents/skills/foo/SKILL.md")).unwrap(), before_source);
    assert_eq!(fs::read_to_string(f.home.join(".codex/config.toml")).unwrap(), "model = 'test'\n");
    assert_eq!(fs::read_to_string(f.home.join(".gemini/settings.json")).unwrap(), "{\"other\":true}");
    assert!(!f.home.join(".pi/agent/skills").exists());
    for path in untouched {
        assert_eq!(fs::read_to_string(f.home.join(path)).unwrap(), "{\"keep\":true}");
    }
    let before_sync = f.settings();
    f.ok(&["sync", "--apply"], None);
    assert_eq!(f.settings(), before_sync);
}

#[test]
fn invalid_settings_and_unsupported_paths_are_not_overwritten() {
    let f = Fixture::new();
    for text in ["{broken", "[]", r#"{"skills":{}}"#, r#"{"skills":[42]}"#, r#"{"skills":["/extra/skills"]}"#] {
        f.write(".pi/agent/settings.json", text);
        assert!(f.ok(&["list"], None).contains('?'));
        assert!(!f.run(&["disable", "foo", "pi"], None).status.success());
        assert_eq!(fs::read_to_string(f.home.join(".pi/agent/settings.json")).unwrap(), text);
    }
}

#[test]
fn global_removal_cleans_exact_pi_rules_without_creating_other_agents() {
    let f = Fixture::new();
    f.write(".pi/agent/settings.json", r#"{"skills":["!f*","+skills/foo","-skills/bar"],"keep":true}"#);
    f.ok(&["rm", "foo", "-y"], None);
    assert_eq!(f.settings(), json!({"skills":["!f*","-skills/bar"],"keep":true}));
    assert!(!f.home.join(".agents/skills/foo").exists());
    assert!(f.home.join(".agents/skills/bar/SKILL.md").exists());
    assert!(!f.home.join(".codex").exists());
}

#[test]
fn plugins_and_mcp_are_not_pi_features() {
    let f = Fixture::new();
    f.write(".pi/agent/settings.json", "{}");
    f.write(".claude.json", r#"{"mcpServers":{"test":{"command":"nonexistent"}}}"#);
    f.write(".claude/plugins/installed_plugins.json", r#"{"plugins":{"example@test":[{"installPath":"/nonexistent","scope":"user"}]}}"#);
    f.write(".claude/settings.json", r#"{"enabledPlugins":{"example@test":true}}"#);
    for name in ["mcp:test", "example@test"] {
        assert!(!f.run(&["enable", name, "pi"], None).status.success());
    }
    f.ok(&["sync", "--apply"], None);
    assert_eq!(f.settings(), json!({}));
    assert!(!f.home.join(".pi/agent/mcp.json").exists());
    assert!(!f.home.join(".pi/agent/skills").exists());
}
