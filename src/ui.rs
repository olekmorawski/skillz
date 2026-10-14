use crate::model::*;
use crate::{run_cmds, sync};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Style, Stylize},
    symbols,
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, LineGauge, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState, Tabs, Wrap},
    DefaultTerminal, Frame,
};
use std::{collections::BTreeSet, fs, io};

const ACCENT: Color = Color::Rgb(217, 119, 87);
const DIM: Color = Color::Rgb(115, 115, 128);
const FAINT: Color = Color::Rgb(70, 70, 82);
const SEL_BG: Color = Color::Rgb(38, 38, 46);
const CHIP_BG: Color = Color::Rgb(55, 55, 66);
const OK: Color = Color::Rgb(120, 200, 140);
const BAD: Color = Color::Rgb(235, 100, 100);
const WARN: Color = Color::Rgb(240, 190, 90);
const SKILL_C: Color = Color::Rgb(110, 180, 230);
const PLUGIN_C: Color = Color::Rgb(220, 140, 220);
const MCP_C: Color = Color::Rgb(130, 210, 190);
const TABS: [&str; 4] = ["All", "Skills", "Plugins", "MCP"];

fn pcolor(p: P) -> Color {
    match p {
        P::Claude => ACCENT,
        P::Codex => Color::Rgb(16, 163, 127),
        P::Omp => Color::Rgb(167, 139, 250),
        P::OpenCode => Color::Rgb(230, 230, 235),
        P::Gemini => Color::Rgb(66, 133, 244),
    }
}

pub struct App {
    pub cx: Ctx,
    pub entries: Vec<Entry>,
    pub filter: String,
    pub typing: bool,
    pub tab: usize,
    pub table: TableState,
    pub toast: Option<(String, bool)>,
    pub confirm_delete: bool,
    pub marked: BTreeSet<String>, // ids picked with space; actions apply to these instead of the cursor row
}

impl App {
    pub fn new() -> App {
        let cx = Ctx::load();
        let entries = load_entries(&cx);
        App { cx, entries, filter: String::new(), typing: false, tab: 0, table: TableState::default().with_selected(0), toast: None, confirm_delete: false, marked: BTreeSet::new() }
    }
    fn visible(&self) -> Vec<usize> {
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                let tab_ok = match self.tab {
                    1 => e.kind == Kind::Skill,
                    2 => e.kind == Kind::Plugin,
                    3 => e.kind == Kind::Mcp,
                    _ => true,
                };
                tab_ok && e.matches(&self.filter)
            })
            .collect()
    }
    fn current(&self) -> Option<usize> {
        self.visible().get(self.table.selected()?).copied()
    }
    fn clamp(&mut self) {
        let n = self.visible().len();
        self.table.select(if n == 0 { None } else { Some(self.table.selected().unwrap_or(0).min(n - 1)) });
    }
    /// Rows an action applies to: the marked ones, else the cursor row.
    fn targets(&self) -> Vec<usize> {
        if self.marked.is_empty() {
            return self.current().into_iter().collect();
        }
        (0..self.entries.len()).filter(|&i| self.marked.contains(&self.entries[i].id)).collect()
    }
    fn reload(&mut self) {
        self.cx = Ctx::load();
        self.entries = load_entries(&self.cx);
        self.marked.retain(|id| self.entries.iter().any(|e| &e.id == id));
        self.clamp();
    }
    fn ok(&mut self, m: impl Into<String>) {
        self.toast = Some((m.into(), true));
    }
    fn err(&mut self, m: impl Into<String>) {
        self.toast = Some((m.into(), false));
    }
}

fn rounded(title: &str, color: Color) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(format!(" {title} "), Style::new().fg(color).bold()))
}

fn dot(v: Option<&Vis>, p: P) -> Span<'static> {
    match v {
        Some(Vis::On) => Span::styled("●", Style::new().fg(pcolor(p))),
        Some(Vis::Always) => Span::styled("◆", Style::new().fg(pcolor(p))),
        Some(Vis::Off) => Span::styled("○", Style::new().fg(FAINT)),
        None => Span::styled("·", Style::new().fg(FAINT)),
    }
}

fn kind_glyph(k: Kind) -> (&'static str, Color) {
    match k {
        Kind::Skill => ("◇", SKILL_C),
        Kind::Plugin => ("▣", PLUGIN_C),
        Kind::Mcp => ("⌁", MCP_C),
    }
}

fn chip(key: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!(" {key} "), Style::new().bg(CHIP_BG).fg(Color::White).bold()),
        Span::styled(format!(" {label}  "), Style::new().fg(DIM)),
    ]
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let agents = app.cx.agents.clone();
    let [head, stats, bar, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Length(3), Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)])
        .areas(f.area().inner(Margin::new(1, 0)));

    // header
    let skills = app.entries.iter().filter(|e| e.kind == Kind::Skill).count();
    let plugins = app.entries.iter().filter(|e| e.kind == Kind::Plugin).count();
    let mcps = app.entries.len() - skills - plugins;
    let mut title = vec![Span::styled("✻ skillz", Style::new().fg(ACCENT).bold()), Span::styled("  skills, plugins & MCP for ", Style::new().fg(DIM))];
    for (i, p) in agents.iter().enumerate() {
        if i > 0 {
            title.push(Span::styled(" · ", Style::new().fg(FAINT)));
        }
        title.push(Span::styled(p.name(), Style::new().fg(pcolor(*p))));
    }
    f.render_widget(Paragraph::new(Line::from(title)), head);
    let dups = app.entries.iter().filter(|e| !e.dups.is_empty()).count();
    let mut right = vec![Span::styled(format!("{skills} skills · {plugins} plugins · {mcps} mcp"), Style::new().fg(DIM))];
    if dups > 0 {
        right.push(Span::styled(format!("  ⚠ {dups} duplicates"), Style::new().fg(WARN).bold()));
    }
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), head);

    // agent cards
    if !agents.is_empty() {
        let cards = Layout::horizontal(vec![Constraint::Ratio(1, agents.len() as u32); agents.len()]).spacing(1).split(stats);
        for (i, &p) in agents.iter().enumerate() {
            let on = app.entries.iter().filter(|e| matches!(e.vis.get(&p), Some(Vis::On | Vis::Always))).count();
            let total = app.entries.iter().filter(|e| e.vis.contains_key(&p)).count();
            let ratio = if total == 0 { 0.0 } else { on as f64 / total as f64 };
            let title = format!("{} {}", i + 1, p.name());
            let gauge = LineGauge::default()
                .block(rounded(&title, pcolor(p)))
                .filled_style(Style::new().fg(pcolor(p)))
                .unfilled_style(Style::new().fg(FAINT))
                .line_set(symbols::line::THICK)
                .label(Span::styled(format!("{on:>3}/{total:<3}"), Style::new().fg(Color::White).bold()))
                .ratio(ratio);
            f.render_widget(gauge, cards[i]);
        }
    }

    // tabs + search
    // while searching, tabs count matches
    let hits = |k: Option<Kind>| app.entries.iter().filter(|e| k.is_none_or(|k| e.kind == k) && e.matches(&app.filter)).count();
    let counts = [hits(None), hits(Some(Kind::Skill)), hits(Some(Kind::Plugin)), hits(Some(Kind::Mcp))];
    let tabs = Tabs::new((0..4).map(|i| format!("{} {}", TABS[i], counts[i])))
        .select(app.tab)
        .style(Style::new().fg(DIM))
        .highlight_style(Style::new().fg(ACCENT).bold().underlined())
        .divider(Span::styled("│", Style::new().fg(FAINT)))
        .padding(" ", " ");
    f.render_widget(tabs, bar);
    let search = if app.typing {
        Line::from(vec![Span::styled("⌕ ", Style::new().fg(ACCENT)), Span::raw(app.filter.clone()), Span::styled("▏", Style::new().fg(ACCENT))])
    } else if !app.filter.is_empty() {
        Line::from(vec![Span::styled("⌕ ", Style::new().fg(DIM)), Span::styled(app.filter.clone(), Style::new().fg(Color::White))])
    } else {
        Line::from(Span::styled("⌕ / to search", Style::new().fg(FAINT)))
    };
    f.render_widget(Paragraph::new(search).alignment(Alignment::Right), bar);

    // list + details
    let [list_area, detail_area] = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).spacing(1).areas(body);
    let vis = app.visible();
    let col_w: Vec<u16> = agents.iter().map(|p| p.name().len().max(3) as u16 + 1).collect();
    let rows = vis.iter().map(|&i| {
        let e = &app.entries[i];
        let (glyph, gc) = kind_glyph(e.kind);
        let tick = if app.marked.contains(&e.id) { Span::styled("✓ ", Style::new().fg(ACCENT).bold()) } else { Span::raw("  ") };
        let mut name = vec![tick, Span::styled(format!("{glyph} "), Style::new().fg(gc))];
        if !e.dups.is_empty() {
            name.push(Span::styled("⚠ ", Style::new().fg(WARN)));
        }
        match e.id.strip_prefix(crate::mcp::PREFIX).map(|n| (n, "")).or(e.id.split_once('@')) {
            Some((n, "")) => name.push(Span::raw(n.to_string())),
            Some((n, m)) => name.extend([Span::raw(n.to_string()), Span::styled(format!("@{m}"), Style::new().fg(FAINT))]),
            None => name.push(Span::raw(e.id.clone())),
        }
        let mut cells = vec![Cell::from(Line::from(name))];
        cells.extend(agents.iter().map(|&p| Cell::from(Line::from(dot(e.vis.get(&p), p)).alignment(Alignment::Center))));
        Row::new(cells)
    });
    let mut header = vec![Cell::from(Span::styled("NAME", Style::new().fg(DIM).bold()))];
    header.extend(agents.iter().map(|&p| Cell::from(Line::from(Span::styled(p.name(), Style::new().fg(pcolor(p)).bold())).alignment(Alignment::Center))));
    let mut widths = vec![Constraint::Min(18)];
    widths.extend(col_w.iter().map(|&w| Constraint::Length(w)));
    let table = Table::new(rows, widths)
        .header(Row::new(header))
        .block(rounded("items", FAINT).padding(Padding::horizontal(1)))
        .row_highlight_style(Style::new().bg(SEL_BG).bold())
        .highlight_symbol(Span::styled("▌", Style::new().fg(ACCENT)))
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    f.render_stateful_widget(table, list_area, &mut app.table);
    if vis.is_empty() {
        let msg = Paragraph::new(Span::styled(format!("nothing matches “{}”", app.filter), Style::new().fg(DIM))).alignment(Alignment::Center);
        f.render_widget(msg, list_area.inner(Margin::new(1, list_area.height / 2)));
    }
    let rows_fit = list_area.height.saturating_sub(3) as usize;
    if vis.len() > rows_fit {
        let mut sb = ScrollbarState::new(vis.len().saturating_sub(rows_fit)).position(app.table.offset());
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None).thumb_style(Style::new().fg(ACCENT)).track_style(Style::new().fg(FAINT)),
            list_area.inner(Margin::new(0, 1)),
            &mut sb,
        );
    }
    draw_details(f, app, detail_area);

    // footer: toast replaces the key chips until the next key press
    let foot_line = if let Some((m, ok)) = &app.toast {
        let (icon, c) = if *ok { ("✓ ", OK) } else { ("✗ ", BAD) };
        Line::from(vec![Span::styled(icon, Style::new().fg(c).bold()), Span::styled(m.clone(), Style::new().fg(c))])
    } else if app.typing {
        Line::from([chip("enter", "keep"), chip("esc", "clear")].concat())
    } else {
        let mut keys = Vec::new();
        if !app.marked.is_empty() {
            keys.push(Span::styled(format!(" {} selected ", app.marked.len()), Style::new().bg(ACCENT).fg(Color::Black).bold()));
            keys.push(Span::raw("  "));
            keys.extend(chip("esc", "unselect"));
        }
        let n = format!("1-{}", agents.len());
        for (k, l) in [(n.as_str(), "toggle"), ("a", "all"), ("space", "select"), ("*", "select shown"), ("u", "update"), ("U", "update all"), ("s", "sync"), ("d", "delete"), ("tab", "view"), ("/", "search"), ("q", "quit")] {
            keys.extend(chip(k, l));
        }
        Line::from(keys)
    };
    f.render_widget(Paragraph::new(foot_line), foot);

    if app.confirm_delete {
        let ids: Vec<&str> = app.targets().iter().map(|&i| app.entries[i].id.as_str()).collect();
        if !ids.is_empty() {
            draw_confirm(f, &ids);
        }
    }
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let block = rounded("details", FAINT).padding(Padding::new(2, 2, 1, 0));
    let Some(i) = app.current() else {
        f.render_widget(block, area);
        return;
    };
    let e = &app.entries[i];
    let home = app.cx.home.to_string_lossy().into_owned();
    let (badge, bc) = match e.kind {
        Kind::Skill => (" SKILL ", SKILL_C),
        Kind::Plugin => (" PLUGIN ", PLUGIN_C),
        Kind::Mcp => (" MCP ", MCP_C),
    };
    let mut lines = vec![
        Line::from(Span::styled(e.id.clone(), Style::new().fg(Color::White).bold())),
        Line::from(vec![Span::styled(badge, Style::new().bg(bc).fg(Color::Black).bold()), Span::raw("  "), Span::styled(e.source.clone(), Style::new().fg(DIM))]),
        Line::from(Span::styled(e.path.to_string_lossy().replace(&home, "~"), Style::new().fg(FAINT))),
        Line::default(),
    ];
    if !e.dups.is_empty() {
        lines.push(Line::from(Span::styled(format!("⚠ duplicate of {} · keep one (d removes this)", e.dups.join(", ")), Style::new().fg(WARN).bold())));
        lines.push(Line::default());
    }
    let desc = if e.desc.is_empty() { "no description".to_string() } else { e.desc.clone() };
    lines.push(Line::from(Span::styled(desc, Style::new().fg(Color::Rgb(200, 200, 210)))));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("VISIBLE IN", Style::new().fg(DIM).bold())));
    for (n, &p) in app.cx.agents.iter().enumerate() {
        let v = e.vis.get(&p);
        let (state, sc) = match v {
            Some(Vis::On) => ("on", pcolor(p)),
            Some(Vis::Always) => ("always (no switch in this agent)", pcolor(p)),
            Some(Vis::Off) => ("off", FAINT),
            None => ("n/a - nothing this agent can load (no skills)", FAINT),
        };
        let how = match e.modes.get(&p) {
            Some(Mode::Links) => "  via links",
            Some(Mode::Native(_)) => "  native",
            None => "",
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", n + 1), Style::new().fg(FAINT)),
            dot(v, p),
            Span::styled(format!("  {:<9}", p.name()), Style::new().fg(Color::White)),
            Span::styled(state, Style::new().fg(sc)),
            Span::styled(how, Style::new().fg(FAINT)),
        ]));
    }
    if e.kind == Kind::Mcp {
        if e.vis.get(&P::Claude) == Some(&Vis::Off) && e.mcp.contains_key(&P::Claude) {
            lines.push(Line::from(Span::styled("  Claude has no MCP switch: its entry is parked in ~/.agents/skillz.json", Style::new().fg(DIM).italic())));
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("CONFIG", Style::new().fg(DIM).bold())));
        for (p, d) in &e.mcp {
            lines.push(Line::from(vec![Span::styled(format!("  {:<9}", p.name()), Style::new().fg(pcolor(*p))), Span::styled(crate::mcp::summary(d), Style::new().fg(FAINT))]));
        }
        lines.push(Line::from(Span::styled("  OAuth servers need a login in each agent (e.g. omp: /mcp reauth <name>)", Style::new().fg(DIM).italic())));
    }
    if e.claude_copy {
        lines.push(Line::from(Span::styled("  Claude uses its own copy (parked in ~/.agents/disabled when off)", Style::new().fg(DIM).italic())));
    }
    if let Some(c) = &e.claude_cmd {
        lines.push(Line::from(Span::styled(format!("  in Claude this is the /{c} command"), Style::new().fg(DIM).italic())));
    }
    if e.kind == Kind::Plugin && !e.skills.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(format!("SKILLS · {}", e.skills.len()), Style::new().fg(DIM).bold())));
        for s in e.skills.iter().take(8) {
            lines.push(Line::from(vec![Span::styled("  ◇ ", Style::new().fg(SKILL_C)), Span::raw(s.clone())]));
        }
        if e.skills.len() > 8 {
            lines.push(Line::from(Span::styled(format!("    +{} more", e.skills.len() - 8), Style::new().fg(DIM))));
        }
    }
    if e.kind == Kind::Skill {
        let md = fs::read_to_string(e.path.join("SKILL.md")).unwrap_or_default();
        let body: Vec<&str> = md.splitn(3, "---").nth(2).unwrap_or(&md).lines().filter(|l| !l.trim().is_empty()).take(14).collect();
        if !body.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("SKILL.md", Style::new().fg(DIM).bold())));
            for l in body {
                let st = if l.starts_with('#') { Style::new().fg(Color::Rgb(170, 170, 185)).bold() } else { Style::new().fg(FAINT) };
                lines.push(Line::from(Span::styled(format!("  {}", l.trim_end()), st)));
            }
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(block), area);
}

fn draw_confirm(f: &mut Frame, ids: &[&str]) {
    let name = if ids.len() == 1 { ids[0].to_string() } else { format!("{} items", ids.len()) };
    let more = if ids.len() > 1 { ids.iter().take(4).copied().collect::<Vec<_>>().join(", ") + if ids.len() > 4 { ", …" } else { "" } } else { String::new() };
    let [r] = Layout::horizontal([Constraint::Length(60)]).flex(Flex::Center).areas(f.area());
    let [r] = Layout::vertical([Constraint::Length(if more.is_empty() { 8 } else { 10 })]).flex(Flex::Center).areas(r);
    f.render_widget(Clear, r);
    let mut text = vec![Line::from(vec![Span::raw("Remove "), Span::styled(name, Style::new().bold()), Span::raw(" completely?")])];
    if !more.is_empty() {
        text.push(Line::from(Span::styled(more, Style::new().fg(Color::White))));
    }
    text.extend([
        Line::from(Span::styled("hub, every agent, all configs and caches", Style::new().fg(DIM))),
        Line::default(),
        Line::from([chip("y", "remove"), chip("n", "cancel")].concat()),
    ]);
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: true }).alignment(Alignment::Center).block(rounded("remove", BAD).padding(Padding::top(1))), r);
}

/// Hand the terminal to external commands, then come back.
fn run_visible(term: &mut DefaultTerminal, cmds: &[Vec<String>]) -> bool {
    ratatui::restore();
    let ok = run_cmds(cmds);
    print!("\n\x1b[2mEnter to return…\x1b[0m");
    let _ = io::Write::flush(&mut io::stdout());
    let _ = io::stdin().read_line(&mut String::new());
    *term = ratatui::init();
    ok
}

fn finish(term: &mut DefaultTerminal, app: &mut App, res: io::Result<Cmds>, ok_msg: String) {
    let res = res.and_then(|c| app.cx.save().map(|_| c));
    match res {
        Ok(cmds) => {
            let ok = cmds.is_empty() || run_visible(term, &cmds);
            if ok { app.ok(ok_msg) } else { app.err(format!("{ok_msg} - but a command failed")) }
        }
        Err(e) => app.err(e.to_string()),
    }
    app.reload();
}

pub fn main() -> io::Result<()> {
    let mut app = App::new();
    let mut term = ratatui::init();
    let res = run(&mut term, &mut app);
    ratatui::restore();
    res
}

fn run(term: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        term.draw(|f| draw(f, app))?;
        let Event::Key(k) = event::read()? else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        if app.typing {
            match k.code {
                KeyCode::Enter => app.typing = false,
                KeyCode::Esc => {
                    app.typing = false;
                    app.filter.clear();
                }
                KeyCode::Backspace => {
                    app.filter.pop();
                }
                KeyCode::Char(c) => app.filter.push(c),
                _ => {}
            }
            app.table.select(Some(0));
            app.clamp();
            continue;
        }
        if app.confirm_delete {
            app.confirm_delete = false;
            if k.code == KeyCode::Char('y') {
                let idx = app.targets();
                let (mut cmds, mut err) = (Vec::new(), None);
                for &i in &idx {
                    match purge(&mut app.cx, &app.entries[i]) {
                        Ok(c) => cmds.extend(c),
                        Err(e) => err = err.or(Some(e)),
                    }
                }
                let msg = match idx.as_slice() {
                    [i] => format!("removed {} everywhere", app.entries[*i].id),
                    _ => format!("removed {} items everywhere", idx.len()),
                };
                app.marked.clear();
                finish(term, app, err.map_or(Ok(cmds), Err), msg);
            } else {
                app.ok("cancelled");
            }
            continue;
        }
        app.toast = None;
        let n = app.visible().len();
        let sel = app.table.selected().unwrap_or(0);
        match k.code {
            KeyCode::Char('q') => return Ok(()),
            KeyCode::Esc if !app.marked.is_empty() => app.marked.clear(),
            KeyCode::Char(' ') => {
                if let Some(i) = app.current() {
                    let id = app.entries[i].id.clone();
                    if !app.marked.remove(&id) {
                        app.marked.insert(id);
                    }
                    app.table.select(Some((sel + 1).min(n.saturating_sub(1))));
                }
            }
            // select every row shown (e.g. after a search); again to unselect them
            KeyCode::Char('*') => {
                let ids: Vec<String> = app.visible().iter().map(|&i| app.entries[i].id.clone()).collect();
                if ids.iter().all(|id| app.marked.contains(id)) {
                    ids.iter().for_each(|id| { app.marked.remove(id); });
                } else {
                    app.marked.extend(ids);
                }
            }
            KeyCode::Esc if !app.filter.is_empty() => {
                app.filter.clear();
                app.clamp();
            }
            KeyCode::Esc => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') if n > 0 => app.table.select(Some((sel + 1).min(n - 1))),
            KeyCode::Up | KeyCode::Char('k') => app.table.select(Some(sel.saturating_sub(1))),
            KeyCode::PageDown => app.table.select(Some((sel + 15).min(n.saturating_sub(1)))),
            KeyCode::PageUp => app.table.select(Some(sel.saturating_sub(15))),
            KeyCode::Home | KeyCode::Char('g') => app.table.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => app.table.select(Some(n.saturating_sub(1))),
            KeyCode::Tab | KeyCode::Right => {
                app.tab = (app.tab + 1) % TABS.len();
                app.table.select(Some(0));
                app.clamp();
            }
            KeyCode::BackTab | KeyCode::Left => {
                app.tab = (app.tab + TABS.len() - 1) % TABS.len();
                app.table.select(Some(0));
                app.clamp();
            }
            KeyCode::Char('/') => {
                app.typing = true;
                app.filter.clear();
            }
            KeyCode::Char('r') => {
                app.reload();
                app.ok("reloaded");
            }
            KeyCode::Char('d') if !app.targets().is_empty() => app.confirm_delete = true,
            KeyCode::Char('s') => {
                let r = sync::sync(&mut app.cx, true).map(|r| r.cmds);
                finish(term, app, r, "synced".into());
            }
            KeyCode::Char('U') => {
                let cmds = sync::update_all_cmds(&app.cx);
                let ok = run_visible(term, &cmds);
                let r = sync::sync(&mut app.cx, true).map(|r| r.cmds);
                finish(term, app, r, if ok { "everything updated".into() } else { "updated - some steps failed".into() });
            }
            KeyCode::Char('u') => {
                let idx = app.targets();
                let cmds: Cmds = idx.iter().flat_map(|&i| update_cmds(&app.cx, &app.entries[i])).collect();
                let label = match idx.as_slice() {
                    [i] => app.entries[*i].id.clone(),
                    _ => format!("{} items", idx.len()),
                };
                if cmds.is_empty() {
                    app.err(format!("{label}: no update source"));
                } else {
                    app.marked.clear();
                    finish(term, app, Ok(cmds), format!("{label} updated"));
                }
            }
            KeyCode::Char(c @ ('1'..='9' | 'a')) => {
                let agents = app.cx.agents.clone();
                let one = match c {
                    'a' => None,
                    _ => match agents.get(c as usize - '1' as usize) {
                        Some(&p) => Some(p),
                        None => continue,
                    },
                };
                let idx = app.targets();
                // (row, agent) pairs this key switches; 'a' skips agents without a switch
                let pairs: Vec<(usize, P)> = idx
                    .iter()
                    .flat_map(|&i| {
                        let e = &app.entries[i];
                        agents.iter().copied().filter(move |p| match one {
                            Some(q) => *p == q && e.vis.contains_key(p),
                            None => matches!(e.vis.get(p), Some(Vis::On | Vis::Off)),
                        }).map(move |p| (i, p))
                    })
                    .collect();
                if pairs.is_empty() {
                    continue;
                }
                // one direction for all: on unless everything already is
                let on = !pairs.iter().all(|&(i, p)| app.entries[i].vis.get(&p) == Some(&Vis::On));
                let (mut cmds, mut err, mut done) = (Vec::new(), None, BTreeSet::new());
                for &(i, p) in &pairs {
                    match set(&mut app.cx, &app.entries[i], p, on, true) {
                        Ok(x) => {
                            cmds.extend(x);
                            done.insert(p);
                        }
                        Err(x) => err = Some(x),
                    }
                }
                let who = match idx.as_slice() {
                    [i] => app.entries[*i].id.clone(),
                    _ => format!("{} items", idx.len()),
                };
                let names: Vec<&str> = done.iter().map(|p| p.name()).collect();
                let msg = format!("{who} → {} {} · restart the agent to apply", names.join(", "), if on { "on" } else { "off" });
                let r = match err {
                    Some(x) if done.is_empty() => Err(x),
                    _ => Ok(cmds),
                };
                finish(term, app, r, msg);
            }
            _ => {}
        }
    }
}
