#[cfg(test)]
mod demo;
mod add;
mod mcp;
mod model;
mod net;
mod sync;
mod ui;
mod updates;

use model::*;
use std::io::{self, Write};
use std::process::{Command, Stdio};

const HELP: &str = "skillz - one place for agent skills, plugins & MCP servers (Claude Code, Codex, omp, OpenCode, Gemini CLI)

USAGE
  skillz                         open the TUI
  skillz list [query…]           print everything (or what matches) and which agents see it
  skillz search <query…>         same as `list <query…>`: name, description, plugin skills, MCP command/URL
  skillz sync [--apply]          show (or fix) what's out of sync between agents
  skillz update                  update all skills and plugins, then sync
  skillz enable  <name> [agent…] make it visible (default: every agent)
  skillz disable <name> [agent…] hide it (default: every agent)
  skillz rm <name> [-y]          remove it completely: hub, every agent, configs, caches
  skillz add <what>              install: owner/repo (skills or a plugin marketplace), owner/repo@skill,
                                 plugin@marketplace, an MCP URL, <name> <url>, or mcp <name> <command…>
  skillz find <query…>           search skills.sh and your plugin marketplaces
  skillz outdated                what has an update upstream
  skillz mcp add <name> <url | command [args…]>
                                 add an MCP server to every agent
  skillz mcp status              is each MCP server up, down or waiting for a login

  agents: claude codex omp opencode gemini
  <name> is a skill name, a plugin id (plugin@marketplace) or mcp:<server>;
  a bare plugin or server name works if unique.";

/// Run commands with the terminal attached; true if all succeeded.
pub fn run_cmds(cmds: &[Vec<String>]) -> bool {
    exec_cmds(cmds, true)
}

/// Run commands with stdio discarded (TUI overlay). True if all succeeded.
pub fn run_cmds_quiet(cmds: &[Vec<String>]) -> bool {
    exec_cmds(cmds, false)
}

fn exec_cmds(cmds: &[Vec<String>], visible: bool) -> bool {
    let mut ok = true;
    for c in cmds {
        if visible {
            println!("\x1b[1;38;2;217;119;87m$\x1b[0m \x1b[1m{}\x1b[0m", c.join(" "));
        }
        let mut p = Command::new(&c[0]);
        p.args(&c[1..]);
        if !visible {
            p.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        }
        match p.status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                if visible {
                    println!("  \x1b[31m✗ exit {}\x1b[0m", s.code().unwrap_or(-1));
                }
                ok = false;
            }
            Err(e) => {
                if visible {
                    println!("  \x1b[31m✗ {e}\x1b[0m");
                }
                ok = false;
            }
        }
    }
    ok
}

fn find<'a>(entries: &'a [Entry], name: &str) -> Result<&'a Entry, String> {
    if let Some(e) = entries.iter().find(|e| e.id == name) {
        return Ok(e);
    }
    let hits: Vec<&Entry> = entries
        .iter()
        .filter(|e| match e.kind {
            Kind::Plugin => e.plugin_name() == name,
            Kind::Mcp => mcp::name_of(&e.id) == name,
            Kind::Skill | Kind::Catalog => false,
        })
        .collect();
    match hits.len() {
        1 => Ok(hits[0]),
        0 => Err(format!("no skill, plugin or MCP server named '{name}' (see `skillz list`)")),
        _ => Err(format!("'{name}' is ambiguous: {}", hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

fn mark(v: Option<&Vis>) -> &'static str {
    match v {
        Some(Vis::On) => "●",
        Some(Vis::Off) => "○",
        Some(Vis::Always) => "◆",
        None => " ",
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        None => ui::main(),
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("skillz {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("list" | "ls" | "--list" | "search") => list(&args[1..].join(" ")),
        Some("sync") => sync_cmd(args.iter().any(|a| a == "--apply")),
        Some("update") => update(),
        Some(c @ ("enable" | "disable")) => toggle_cmd(c == "enable", &args[1..]),
        Some("rm" | "remove") => rm(&args[1..]),
        Some("mcp") if args.get(1).map(String::as_str) == Some("add") => mcp_add(&args[2..]),
        Some("mcp") if args.get(1).map(String::as_str) == Some("status") => mcp_status(),
        Some("add" | "install") => add_cmd(&args[1..].join(" ")),
        Some("find") => find_cmd(&args[1..].join(" ")),
        Some("outdated") => outdated(),
        Some(other) => Err(io::Error::other(format!("unknown command '{other}'\n\n{HELP}"))),
    };
    if let Err(e) = res {
        eprintln!("skillz: {e}");
        std::process::exit(1);
    }
}

fn list(query: &str) -> io::Result<()> {
    let cx = Ctx::load();
    let entries: Vec<Entry> = load_entries(&cx).into_iter().filter(|e| e.matches(query)).collect();
    if entries.is_empty() {
        return Err(io::Error::other(format!("nothing matches '{query}'")));
    }
    let w = entries.iter().map(|e| e.id.len()).max().unwrap_or(10).max(10);
    print!("{:<w$}  {:<6}", "NAME", "TYPE");
    for p in &cx.agents {
        print!(" {:^8}", p.name());
    }
    println!();
    for e in &entries {
        let kind = match e.kind {
            Kind::Skill => "skill",
            Kind::Plugin => "plugin",
            Kind::Mcp => "mcp",
            Kind::Catalog => "-",
        };
        print!("{:<w$}  {:<6}", e.id, kind);
        for p in &cx.agents {
            print!(" {:^8}", mark(e.vis.get(p)));
        }
        if !e.dups.is_empty() {
            print!("  ⚠ dup: {}", e.dups.join(", "));
        }
        println!();
    }
    println!("\n● visible  ○ hidden  ◆ always visible (agent has no switch)");
    Ok(())
}

fn sync_cmd(apply: bool) -> io::Result<()> {
    let mut cx = Ctx::load();
    let r = sync::sync(&mut cx, apply)?;
    for l in &r.lines {
        println!("  {l}");
    }
    if r.lines.is_empty() {
        println!("✓ everything in sync");
    } else if !apply {
        println!("\n(dry run - `skillz sync --apply` fixes it)");
    }
    if apply && !r.cmds.is_empty() {
        run_cmds(&r.cmds);
    }
    Ok(())
}

fn update() -> io::Result<()> {
    let cx = Ctx::load();
    let ok = run_cmds(&sync::update_all_cmds(&cx));
    sync_cmd(true)?;
    if !ok {
        return Err(io::Error::other("some update steps failed (see above)"));
    }
    Ok(())
}

fn toggle_cmd(on: bool, args: &[String]) -> io::Result<()> {
    let name = args.first().ok_or_else(|| io::Error::other("which skill or plugin?"))?;
    let mut cx = Ctx::load();
    let entries = load_entries(&cx);
    let e = find(&entries, name).map_err(io::Error::other)?;
    let agents: Vec<P> = if args.len() > 1 {
        args[1..].iter().map(|a| P::parse(a).ok_or_else(|| io::Error::other(format!("unknown agent '{a}'")))).collect::<io::Result<_>>()?
    } else {
        cx.agents.iter().copied().filter(|p| e.vis.get(p) != Some(&Vis::Always)).collect()
    };
    let mut cmds = Vec::new();
    for p in agents {
        if !cx.agents.contains(&p) {
            return Err(io::Error::other(format!("{} isn't installed", p.name())));
        }
        cmds.extend(set(&mut cx, e, p, on, true)?);
        println!("{} {} → {}", if on { "●" } else { "○" }, e.id, p.name());
    }
    cx.save()?;
    run_cmds(&cmds);
    println!("restart the agents to apply");
    Ok(())
}

fn rm(args: &[String]) -> io::Result<()> {
    let name = args.iter().find(|a| !a.starts_with('-')).ok_or_else(|| io::Error::other("which skill or plugin?"))?;
    let yes = args.iter().any(|a| a == "-y" || a == "--yes");
    let mut cx = Ctx::load();
    let entries = load_entries(&cx);
    let e = find(&entries, name).map_err(io::Error::other)?;
    if !yes {
        print!("Remove {} from the hub, every agent, all configs and caches? [y/N] ", e.id);
        io::stdout().flush()?;
        let mut a = String::new();
        io::stdin().read_line(&mut a)?;
        if !a.trim().eq_ignore_ascii_case("y") {
            println!("cancelled");
            return Ok(());
        }
    }
    let cmds = purge(&mut cx, e)?;
    cx.save()?;
    let ok = run_cmds(&cmds);
    println!("{} removed {}", if ok { "✓" } else { "⚠" }, e.id);
    Ok(())
}

fn mcp_add(args: &[String]) -> io::Result<()> {
    let name = args.first().ok_or_else(|| io::Error::other("usage: skillz mcp add <name> <url | command [args…]>"))?;
    let mut cx = Ctx::load();
    mcp::add(&mut cx, name, &args[1..])?;
    cx.save()?;
    println!("● mcp:{name} → {}", cx.agents.iter().map(|p| p.name()).collect::<Vec<_>>().join(", "));
    println!("restart the agents to apply; servers that use OAuth need a login in each agent");
    Ok(())
}

fn mcp_status() -> io::Result<()> {
    let cx = Ctx::load();
    let servers: Vec<Entry> = mcp::load(&cx);
    let w = servers.iter().map(|e| e.id.len()).max().unwrap_or(10);
    std::thread::scope(|s| {
        let jobs: Vec<_> = servers.iter().map(|e| (e, s.spawn(|| e.mcp.get(&P::Claude).or_else(|| e.mcp.values().next()).map(mcp::probe)))).collect();
        for (e, j) in jobs {
            let h = j.join().ok().flatten().unwrap_or(mcp::Health::Down("no definition".into()));
            let s = match h {
                mcp::Health::Live => "\x1b[32m● live\x1b[0m".to_string(),
                mcp::Health::Login => "\x1b[33m● up, OAuth\x1b[0m  each agent logs in once".to_string(),
                mcp::Health::Down(why) => format!("\x1b[31m● down\x1b[0m  {why}"),
                mcp::Health::Checking => String::new(),
            };
            println!("{:<w$}  {s}", e.id);
        }
    });
    Ok(())
}

/// Install anything, then spread it to every agent.
fn add_cmd(input: &str) -> io::Result<()> {
    let plan = add::resolve(add::parse(input).map_err(io::Error::other)?);
    let mut cx = Ctx::load();
    if let add::Plan::Mcp { name, target } = &plan {
        mcp::add(&mut cx, name, target)?;
        cx.save()?;
        println!("● mcp:{name} added to every agent");
        return Ok(());
    }
    if let add::Plan::Marketplace { repo, install: None } = &plan {
        println!("{repo} is a plugin marketplace with several plugins: adding it; pick plugins with `skillz find` or Browse in the TUI");
    }
    let ok = run_cmds(&add::cmds(&plan));
    sync_cmd(true)?;
    if !ok {
        return Err(io::Error::other("install failed (see above)"));
    }
    Ok(())
}

fn find_cmd(query: &str) -> io::Result<()> {
    if query.trim().is_empty() {
        return Err(io::Error::other("usage: skillz find <query…>"));
    }
    let cx = Ctx::load();
    let mut hits: Vec<Entry> = add::catalog(&cx).into_iter().filter(|e| e.matches(query)).collect();
    hits.extend(add::search_skills(&cx.hub(), query));
    if hits.is_empty() {
        return Err(io::Error::other(format!("nothing found for '{query}'")));
    }
    for e in &hits {
        println!("{}  \x1b[2m{}\x1b[0m\n    {}", e.id, e.source, e.desc.chars().take(110).collect::<String>());
    }
    println!("\ninstall with: skillz add <name>");
    Ok(())
}

fn outdated() -> io::Result<()> {
    let cx = Ctx::load();
    let ids = updates::check(&cx.home);
    if ids.is_empty() {
        println!("✓ everything is up to date");
    }
    for id in &ids {
        println!("⬆ {id}");
    }
    if !ids.is_empty() {
        println!("\nskillz update   (or u / U in the TUI)");
    }
    Ok(())
}

#[cfg(test)]
mod exec_cmds_tests {
    use super::*;

    #[test]
    fn quiet_true_and_false() {
        assert!(run_cmds_quiet(&[vec!["true".into()]]));
        assert!(!run_cmds_quiet(&[vec!["false".into()]]));
    }
}
