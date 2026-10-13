//! Demo fixture + SVG screenshot of the real TUI render (used for the README).
//! `SKILLZ_SVG=docs/screenshot.svg cargo test screenshot -- --ignored`
use crate::ui::{draw, App};
use ratatui::{backend::TestBackend, style::Color, Terminal};
use std::{fmt::Write as _, fs, os::unix::fs::symlink, path::Path};

fn w(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn skill(dir: &Path, name: &str, desc: &str) {
    w(&dir.join(name).join("SKILL.md"), &format!("---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n{desc}\n\n## When to use\n\n- {desc}\n- Keep it short and concrete.\n"));
}

/// A fake $HOME with five agents, hub skills, two plugins and two MCP servers.
pub fn fixture(h: &Path) {
    let _ = fs::remove_dir_all(h);
    let hub = h.join(".agents/skills");
    let skills = [
        ("brainstorming", "Turn a vague idea into a clear design before any code is written."),
        ("changelog-writer", "Write release notes and changelog entries from git history."),
        ("frontend-taste", "Design distinctive, non-generic interfaces with real typography and spacing."),
        ("pdf-tools", "Read, fill, merge and split PDF files."),
        ("postgres-tuning", "Explain slow queries and propose indexes for Postgres."),
        ("systematic-debugging", "Find the root cause before proposing any fix."),
        ("test-first", "Write the failing test first, then the smallest code that passes."),
    ];
    let mut lock = serde_json::json!({"version": 3, "skills": {}});
    for (n, d) in skills {
        skill(&hub, n, d);
        lock["skills"][n] = serde_json::json!({"source": "acme/agent-skills"});
        if n != "postgres-tuning" {
            fs::create_dir_all(h.join(".claude/skills")).unwrap();
            symlink(Path::new("../../.agents/skills").join(n), h.join(".claude/skills").join(n)).unwrap();
        }
    }
    w(&h.join(".agents/.skill-lock.json"), &lock.to_string());

    // two Claude plugins, one shipped as skills to every agent
    let cache = h.join(".claude/plugins/cache");
    let ponytail = cache.join("ponytail/ponytail/5.2.0");
    w(&ponytail.join(".claude-plugin/plugin.json"), r#"{"name":"ponytail","description":"Lazy senior dev mode: the simplest change that fully works."}"#);
    skill(&ponytail.join("skills"), "ponytail", "Lazy senior dev mode.");
    skill(&ponytail.join("skills"), "ponytail-review", "Review a diff for over-engineering.");
    let lsp = cache.join("claude-plugins-official/rust-analyzer-lsp/1.0.0");
    w(&lsp.join(".claude-plugin/plugin.json"), r#"{"name":"rust-analyzer-lsp","description":"Rust language server for Claude Code."}"#);
    w(&h.join(".claude/plugins/installed_plugins.json"), &serde_json::json!({"version": 2, "plugins": {
        "ponytail@ponytail": [{"scope": "user", "installPath": ponytail}],
        "rust-analyzer-lsp@claude-plugins-official": [{"scope": "user", "installPath": lsp}]
    }}).to_string());
    w(&h.join(".claude/plugins/known_marketplaces.json"), r#"{"ponytail":{"source":{"source":"github","repo":"DietrichGebert/ponytail"}},"claude-plugins-official":{"source":{"source":"github","repo":"anthropics/claude-plugins-official"}}}"#);
    w(&h.join(".claude/settings.json"), r#"{"enabledPlugins":{"ponytail@ponytail":true,"rust-analyzer-lsp@claude-plugins-official":true}}"#);
    w(&h.join(".codex/config.toml"), "[marketplaces.ponytail]\nsource_type = \"git\"\nsource = \"https://github.com/DietrichGebert/ponytail.git\"\n\n[plugins.\"ponytail@ponytail\"]\nenabled = true\n\n[[skills.config]]\nname = \"frontend-taste\"\nenabled = false\n\n[mcp_servers.playwright]\ncommand = \"npx\"\nargs = [\"@playwright/mcp@latest\"]\n");
    w(&h.join(".claude.json"), r#"{"mcpServers":{"playwright":{"type":"stdio","command":"npx","args":["@playwright/mcp@latest"]},"linear":{"type":"http","url":"https://mcp.linear.app/mcp"}}}"#);
    fs::create_dir_all(h.join(".omp/agent")).unwrap();
    w(&h.join(".config/opencode/opencode.json"), r#"{"plugin":[]}"#);
    w(&h.join(".gemini/settings.json"), r#"{"skills":{"disabled":["pdf-tools"]}}"#);
    for s in ["ponytail", "ponytail-review"] {
        let d = h.join(".gemini/skills");
        fs::create_dir_all(&d).unwrap();
        symlink(ponytail.join("skills").join(s), d.join(format!("ponytail--{s}"))).unwrap();
    }
}

fn hex(c: Color, fallback: &str) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::White => "#f2f2f5".into(),
        Color::Black => "#101014".into(),
        Color::Reset => fallback.into(),
        _ => fallback.into(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Render the terminal buffer as an SVG "window".
pub fn svg(buf: &ratatui::buffer::Buffer) -> String {
    let (cw, lh, pad, top) = (8.6_f32, 19.0_f32, 18.0_f32, 40.0_f32);
    let (cols, rows) = (buf.area.width, buf.area.height);
    let (wd, ht) = (cols as f32 * cw + pad * 2.0, rows as f32 * lh + top + pad);
    let bg = "#141419";
    let mut s = String::new();
    let _ = write!(
        s,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{wd}" height="{ht}" viewBox="0 0 {wd} {ht}" font-family="JetBrains Mono, SF Mono, Menlo, Consolas, monospace" font-size="14.2">
<rect width="{wd}" height="{ht}" rx="12" fill="{bg}"/><circle cx="22" cy="20" r="6" fill="#ff5f57"/><circle cx="42" cy="20" r="6" fill="#febc2e"/><circle cx="62" cy="20" r="6" fill="#28c840"/>
<text x="{}" y="25" fill="#77778a" text-anchor="middle" font-size="12.5">skillz</text>
"##,
        wd / 2.0
    );
    for y in 0..rows {
        let ty = top + y as f32 * lh;
        let mut x = 0;
        while x < cols {
            let c = &buf[(x, y)];
            let (fg, bgc, bold) = (c.fg, c.bg, c.modifier.contains(ratatui::style::Modifier::BOLD));
            let ul = c.modifier.contains(ratatui::style::Modifier::UNDERLINED);
            let mut text = String::new();
            let x0 = x;
            while x < cols {
                let d = &buf[(x, y)];
                if d.fg != fg || d.bg != bgc || d.modifier != c.modifier {
                    break;
                }
                text.push_str(d.symbol());
                x += 1;
            }
            let px = pad + x0 as f32 * cw;
            if bgc != Color::Reset {
                let _ = write!(s, r#"<rect x="{px}" y="{}" width="{}" height="{lh}" fill="{}"/>"#, ty, (x - x0) as f32 * cw, hex(bgc, bg));
            }
            if !text.trim().is_empty() {
                let _ = write!(
                    s,
                    r#"<text x="{px}" y="{}" fill="{}"{}{} xml:space="preserve" textLength="{}" lengthAdjust="spacingAndGlyphs">{}</text>"#,
                    ty + lh * 0.72,
                    hex(fg, "#d6d6de"),
                    if bold { r#" font-weight="700""# } else { "" },
                    if ul { r#" text-decoration="underline""# } else { "" },
                    (x - x0) as f32 * cw,
                    esc(&text)
                );
            }
            s.push('\n');
        }
    }
    s.push_str("</svg>\n");
    s
}

#[test]
#[ignore]
fn screenshot() {
    let h = std::env::temp_dir().join("skillz-demo-home");
    fixture(&h);
    // SAFETY: single-threaded ignored test; nothing else reads HOME concurrently
    unsafe { std::env::set_var("HOME", &h) };
    let mut app = App::new();
    app.table.select(Some(2));
    let mut t = Terminal::new(TestBackend::new(132, 25)).unwrap();
    t.draw(|f| draw(f, &mut app)).unwrap();
    let out = std::env::var("SKILLZ_SVG").unwrap_or_else(|_| "docs/screenshot.svg".into());
    fs::create_dir_all(Path::new(&out).parent().unwrap()).unwrap();
    fs::write(&out, svg(t.backend().buffer())).unwrap();
    // the demo must load like a real machine: 7 skills, 2 plugins, 2 MCP servers, all five agents
    assert_eq!(app.entries.len(), 11);
    assert_eq!(app.cx.agents.len(), 5);
}
