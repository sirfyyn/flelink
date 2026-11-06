use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind, KeyModifiers};
use flelink_core::{catalog, chat, engines, flow, server::Server, system, Event};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

// The brand in 256 colours (the macOS Terminal has no 24-bit colour): the pink signal colour of flelink.run (ff2d95 is close to 198),
// cyan as the quiet second colour, and the muted line colour for the frames.
const CYAN: Color = Color::Indexed(51);
const VIOLET: Color = Color::Indexed(198);
const FUCHSIA: Color = Color::Indexed(198);
const LINE: Color = Color::Indexed(238);

/// A rounded frame in the line colour with its title in normal text.
fn panel(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(LINE))
        .title(Span::styled(
            title.to_string(),
            Style::new().fg(Color::Reset),
        ))
}

/// "FLE Link" in the gradient of the logo.
fn brand() -> Vec<Span<'static>> {
    let colors = [
        FUCHSIA, FUCHSIA, FUCHSIA, FUCHSIA, FUCHSIA, FUCHSIA, FUCHSIA, FUCHSIA,
    ];
    let mut out = vec![Span::raw(" ")];
    for (ch, c) in "FLE Link".chars().zip(colors) {
        out.push(Span::styled(
            ch.to_string(),
            Style::new().fg(c).add_modifier(Modifier::BOLD),
        ));
    }
    out.push(Span::raw(" "));
    out
}

/// Three lights that come on in turn, like the route between the active experts of the logo.
fn lights() -> Vec<Span<'static>> {
    let n = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        / 220
        % 3) as usize;
    [CYAN, VIOLET, FUCHSIA]
        .iter()
        .enumerate()
        .map(|(i, c)| Span::styled(if i == n { "● " } else { "∙ " }, Style::new().fg(*c)))
        .collect()
}

enum Msg {
    Ev(Event),
    Srv(Server),
    Err(String),
    Tok(bool, String),
    Done(Result<chat::Stats, String>),
    Hits(Result<Vec<(String, String)>, String>),
    Picked(Result<Box<flelink_core::recipe::Recipe>, String>),
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Machine,
    Models,
    Search,
    Loading,
    Chat,
}

struct Ready {
    base: String,
    engine: String,
    model: String,
    command: String,
}

struct App {
    home: PathBuf,
    screen: Screen,
    sys: system::SysInfo,
    models: Vec<catalog::Model>,
    list: ListState,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    steps: Vec<String>,
    progress: Option<(String, u64, u64)>,
    logline: String,
    ready: Option<Ready>,
    server: Option<Server>,
    history: Vec<(String, String)>,
    reasoning: String,
    input: String,
    busy: bool,
    stats: chat::Stats,
    err: Option<String>,
    show_cmd: bool,
    quit: bool,
    // the search for a model on Hugging Face (the last row of the model list)
    query: String,
    hits: Vec<(String, String)>,
    hit_list: ListState,
    in_list: bool,
    dirty: bool,
    searching: bool,
}

pub fn run(home: &Path) -> Result<()> {
    let sys = system::detect(home);
    let (tx, rx) = channel();
    let models = catalog::builtin();
    let mut list = ListState::default();
    list.select(Some(0));
    let mut app = App {
        home: home.to_path_buf(),
        screen: Screen::Machine,
        sys,
        models,
        list,
        tx,
        rx,
        steps: vec![],
        progress: None,
        logline: String::new(),
        ready: None,
        server: None,
        history: vec![],
        reasoning: String::new(),
        input: String::new(),
        busy: false,
        stats: Default::default(),
        err: None,
        show_cmd: false,
        quit: false,
        query: String::new(),
        hits: vec![],
        hit_list: ListState::default(),
        in_list: false,
        dirty: false,
        searching: false,
    };
    let mut term = ratatui::init();
    let res = main_loop(&mut term, &mut app);
    ratatui::restore();
    drop(app.server.take()); // stops the engine
    res
}

fn main_loop(term: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        while let Ok(m) = app.rx.try_recv() {
            app.on_msg(m);
        }
        term.draw(|f| app.draw(f))?;
        if event::poll(Duration::from_millis(60))? {
            if let CEvent::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.on_key(k.code, k.modifiers);
                }
            }
        }
    }
    Ok(())
}

impl App {
    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Ev(Event::Step(s)) => {
                self.steps.push(s);
                self.progress = None;
            }
            Msg::Ev(Event::Progress { label, done, total }) => {
                self.progress = Some((label, done, total))
            }
            Msg::Ev(Event::Log(l)) => self.logline = l,
            Msg::Ev(Event::Ready {
                base,
                engine,
                model,
                command,
            }) => {
                self.ready = Some(Ready {
                    base,
                    engine,
                    model,
                    command,
                });
                self.screen = Screen::Chat;
                self.progress = None;
            }
            Msg::Srv(s) => self.server = Some(s),
            Msg::Err(e) => {
                self.err = Some(e);
                self.screen = Screen::Models;
            }
            Msg::Tok(r, t) => {
                if r {
                    self.reasoning.push_str(&t)
                } else if let Some(last) = self.history.last_mut() {
                    last.1.push_str(&t)
                }
            }
            Msg::Hits(r) => {
                self.searching = false;
                self.dirty = false;
                match r {
                    Ok(h) => {
                        self.err = if h.is_empty() {
                            Some("Nothing found. Try another word.".into())
                        } else {
                            None
                        };
                        self.hit_list
                            .select(if h.is_empty() { None } else { Some(0) });
                        self.hits = h;
                    }
                    Err(e) => self.err = Some(e),
                }
            }
            Msg::Picked(r) => {
                self.searching = false;
                match r {
                    Ok(rec) => self.begin_load(*rec),
                    Err(e) => self.err = Some(e),
                }
            }
            Msg::Done(r) => {
                self.busy = false;
                match r {
                    Ok(s) => self.stats = s,
                    Err(e) => self.err = Some(e),
                }
            }
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.screen {
            Screen::Machine => match code {
                KeyCode::Enter => self.screen = Screen::Models,
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                _ => {}
            },
            Screen::Models => match code {
                KeyCode::Up => {
                    let i = self.list.selected().unwrap_or(0);
                    self.list.select(Some(i.saturating_sub(1)));
                }
                KeyCode::Down => {
                    let i = self.list.selected().unwrap_or(0);
                    // one row more than the models: the search
                    self.list.select(Some((i + 1).min(self.models.len())));
                }
                KeyCode::Enter => {
                    if self.list.selected() == Some(self.models.len()) {
                        self.err = None;
                        self.screen = Screen::Search;
                    } else {
                        self.start_load()
                    }
                }
                KeyCode::Esc | KeyCode::Left => self.screen = Screen::Machine,
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
            Screen::Search => match code {
                KeyCode::Esc => {
                    self.err = None;
                    self.screen = Screen::Models;
                }
                KeyCode::Down if !self.hits.is_empty() => {
                    self.in_list = true;
                    let i = self.hit_list.selected().unwrap_or(0);
                    self.hit_list.select(Some((i + 1).min(self.hits.len() - 1)));
                }
                KeyCode::Up if self.in_list => {
                    let i = self.hit_list.selected().unwrap_or(0);
                    if i == 0 {
                        self.in_list = false;
                    } else {
                        self.hit_list.select(Some(i - 1));
                    }
                }
                KeyCode::Enter if !self.searching => {
                    if self.in_list && !self.dirty {
                        if let Some(i) = self.hit_list.selected() {
                            self.pick(self.hits[i].0.clone());
                        }
                    } else if !self.query.trim().is_empty() {
                        self.search();
                    }
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.in_list = false;
                    self.dirty = true;
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                    self.query.push(c);
                    self.in_list = false;
                    self.dirty = true;
                }
                _ => {}
            },
            Screen::Loading => {
                if code == KeyCode::Char('q') {
                    self.quit = true
                }
            }
            Screen::Chat => match code {
                KeyCode::Enter if !self.busy && !self.input.trim().is_empty() => self.send(),
                KeyCode::Char('o') if mods.contains(KeyModifiers::CONTROL) => {
                    self.show_cmd = !self.show_cmd
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Esc => self.input.clear(),
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => self.input.push(c),
                _ => {}
            },
        }
    }

    fn start_load(&mut self) {
        let m = self.models[self.list.selected().unwrap_or(0)].clone();
        if catalog::fit(&m, &self.sys) == catalog::Fit::TooBig {
            self.err = Some(format!(
                "{} does not fit this machine ({} memory).",
                m.title,
                system::gib(self.sys.ram_bytes)
            ));
            return;
        }
        match flelink_core::recipe::find(&self.home, m.id) {
            Ok(r) => self.begin_load(r),
            Err(e) => self.err = Some(format!("{e:#}")),
        }
    }

    /// Download what is missing, start the engine, open the chat: the same for a catalog model and a found one.
    fn begin_load(&mut self, r: flelink_core::recipe::Recipe) {
        self.err = None;
        self.steps.clear();
        self.progress = None;
        self.logline.clear();
        self.screen = Screen::Loading;
        let (tx, home) = (self.tx.clone(), self.home.clone());
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            match flow::ensure_and_start_recipe(&home, &r, &mut |e| {
                let _ = tx2.send(Msg::Ev(e));
            }) {
                Ok(srv) => {
                    let _ = tx.send(Msg::Srv(srv));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Err(format!("{e:#}")));
                }
            }
        });
    }

    fn search(&mut self) {
        self.searching = true;
        self.err = None;
        let (tx, q) = (self.tx.clone(), self.query.trim().to_string());
        std::thread::spawn(move || {
            let r = flelink_api::hf::search(&q, 12, "").map(|v| {
                v.iter()
                    .map(|h| {
                        let id = h["id"].as_str().unwrap_or("").to_string();
                        let dl = h["downloads"].as_u64().unwrap_or(0);
                        (id.clone(), format!("{:>6}  {id}", compact(dl)))
                    })
                    .filter(|(id, _)| !id.is_empty())
                    .collect()
            });
            let _ = tx.send(Msg::Hits(r));
        });
    }

    /// A found repo: look at its files, take the quantisation that fits this machine and turn it into a recipe.
    fn pick(&mut self, repo: String) {
        self.searching = true;
        self.err = None;
        let (tx, ram) = (self.tx.clone(), self.sys.ram_bytes);
        std::thread::spawn(move || {
            let r = (|| {
                let files = flelink_api::hf::tree(&repo, "main", "")?;
                let groups = flelink_api::hf::gguf_groups(&files, "Q4_K_M");
                let g = pick_group(&groups, ram).ok_or_else(|| {
                    format!(
                        "No quantisation of {repo} fits this machine ({} memory).",
                        system::gib(ram)
                    )
                })?;
                let paths: Vec<String> = g["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f["path"].as_str().map(|p| p.to_string()))
                    .collect();
                let toml = recipe_toml(&repo, &paths, g["size"].as_u64().unwrap_or(0));
                flelink_core::recipe::Recipe::parse(&toml, "search")
                    .map(Box::new)
                    .map_err(|e| format!("{e:#}"))
            })();
            let _ = tx.send(Msg::Picked(r));
        });
    }

    fn send(&mut self) {
        let Some(r) = &self.ready else { return };
        let text = std::mem::take(&mut self.input);
        self.history.push(("user".into(), text));
        let hist = self.history.clone();
        self.history.push(("assistant".into(), String::new()));
        self.reasoning.clear();
        self.busy = true;
        self.err = None;
        let (tx, base, model) = (self.tx.clone(), r.base.clone(), r.model.clone());
        std::thread::spawn(move || {
            let t2 = tx.clone();
            let res = chat::stream(&base, &model, &hist, &mut |r, t| {
                let _ = t2.send(Msg::Tok(r, t.to_string()));
            });
            let _ = tx.send(Msg::Done(res.map_err(|e| format!("{e:#}"))));
        });
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let [head, body, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(area);
        let mut title = brand();
        title.push(Span::styled(
            match self.screen {
                Screen::Machine => "· this machine",
                Screen::Models => "· choose a model",
                Screen::Search => "· search Hugging Face",
                Screen::Loading => "· loading ",
                Screen::Chat => "· chat",
            },
            Style::new().fg(Color::DarkGray),
        ));
        if matches!(self.screen, Screen::Loading) {
            title.extend(lights());
        }
        let title = Line::from(title);
        f.render_widget(Paragraph::new(title), head);
        let hint = match self.screen {
            Screen::Machine => "Enter: continue   q: quit",
            Screen::Models => "up/down: choose   Enter: load or search   Esc: back   q: quit",
            Screen::Search => {
                "type, Enter: search   Down: results   Enter on a result: load   Esc: back"
            }
            Screen::Loading => "q: quit (stops the engine)",
            Screen::Chat => {
                "Enter: send   Ctrl+O: API, agents and start command   Esc: clear   Ctrl+C: quit"
            }
        };
        f.render_widget(
            Paragraph::new(hint).style(Style::new().fg(Color::DarkGray)),
            foot,
        );
        match self.screen {
            Screen::Machine => self.machine(f, body),
            Screen::Models => self.models_view(f, body),
            Screen::Search => self.search_view(f, body),
            Screen::Loading => self.loading(f, body),
            Screen::Chat => self.chat_view(f, body),
        }
    }

    fn machine(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let s = &self.sys;
        let gpu = if s.gpus.is_empty() {
            "none".to_string()
        } else {
            s.gpus
                .iter()
                .map(|g| g.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let vs = engines::variants(s);
        let best = vs.first();
        let have = engines::installed(&self.home);
        let mut t = vec![
            Line::from(""),
            kv("Processor", &format!("{} ({} cores)", s.cpu, s.cores)),
            kv("Memory", &system::gib(s.ram_bytes)),
            kv("GPU", &gpu),
            kv("GPU may use", &system::gib(s.gpu_usable_bytes)),
            kv("Disk free", &system::gib(s.disk_free_bytes)),
            Line::from(""),
        ];
        if let Some(b) = best {
            t.push(Line::from(Span::styled(
                format!("  Best engine for this machine: {}", b.label),
                Style::new().add_modifier(Modifier::BOLD).fg(Color::Green),
            )));
            t.push(Line::from(format!("  {}", b.why)));
            t.push(Line::from(if have.is_empty() {
                "  Not installed yet: the first load installs it for you.".to_string()
            } else {
                format!(
                    "  Installed: {}",
                    have.iter()
                        .map(|(n, _)| n.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }));
        }
        f.render_widget(Paragraph::new(t).block(panel(" Your machine ")), area);
    }

    fn models_view(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let [top, bottom] =
            Layout::vertical([Constraint::Min(5), Constraint::Length(5)]).areas(area);
        let items: Vec<ListItem> = self
            .models
            .iter()
            .map(|m| {
                let fit = catalog::fit(m, &self.sys);
                let col = match fit {
                    catalog::Fit::Fits => Color::Green,
                    catalog::Fit::Tight => Color::Yellow,
                    catalog::Fit::TooBig => Color::Red,
                };
                let got = flow::model_path(&self.home, m).exists();
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:<8}", fit.label()),
                        Style::new().fg(col).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" {:>9}  {}", system::gib(m.size), m.title)),
                    Span::styled(
                        if got { "  [downloaded]" } else { "" },
                        Style::new().fg(Color::Cyan),
                    ),
                ]))
            })
            .chain(std::iter::once(ListItem::new(Line::from(vec![
                Span::styled(
                    " search  ",
                    Style::new().fg(FUCHSIA).add_modifier(Modifier::BOLD),
                ),
                Span::raw("            Search Hugging Face for another model ..."),
            ]))))
            .collect();
        let list = List::new(items)
            .block(panel(
                " Models (whether they fit is checked before anything is downloaded) ",
            ))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        f.render_stateful_widget(list, top, &mut self.list);
        let mut lines = vec![Line::from(match self.models.get(self.list.selected().unwrap_or(0)) {
            Some(sel) => sel.note.to_string(),
            None => "Type a name, for example qwen, llama or deepseek: FLE Link lists GGUF models from Hugging Face, takes the quantisation that fits this machine and loads it."
                .to_string(),
        })];
        if let Some(e) = &self.err {
            lines.push(Line::from(Span::styled(
                e.clone(),
                Style::new().fg(Color::Red),
            )));
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel(" About ")),
            bottom,
        );
    }

    fn search_view(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let [top, mid, bottom] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(4),
        ])
        .areas(area);
        let cursor = if self.in_list { "" } else { "_" };
        f.render_widget(
            Paragraph::new(format!("> {}{}", self.query, cursor))
                .block(panel(" Search Hugging Face for GGUF models ")),
            top,
        );
        let items: Vec<ListItem> = self
            .hits
            .iter()
            .map(|(_, label)| ListItem::new(format!(" {label}")))
            .collect();
        let title = if self.searching {
            " searching ... "
        } else if self.hits.is_empty() {
            " Results (most downloaded first) "
        } else {
            " Results, most downloaded first "
        };
        let list = List::new(items)
            .block(panel(title))
            .highlight_style(if self.in_list {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            })
            .highlight_symbol(if self.in_list { "> " } else { "  " });
        f.render_stateful_widget(list, mid, &mut self.hit_list);
        let mut lines = vec![Line::from(
            "The first load downloads the files; whether the model fits is checked before.",
        )];
        if let Some(e) = &self.err {
            lines.push(Line::from(Span::styled(
                e.clone(),
                Style::new().fg(Color::Red),
            )));
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel(" About ")),
            bottom,
        );
    }

    fn loading(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let [steps, gauge, log] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(area);
        let lines: Vec<Line> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let last = i + 1 == self.steps.len();
                Line::from(vec![
                    Span::styled(
                        if last { " ... " } else { " ok  " },
                        Style::new().fg(if last { FUCHSIA } else { Color::Green }),
                    ),
                    Span::raw(s.clone()),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel(" Getting ready ")),
            steps,
        );
        let (label, ratio) = match &self.progress {
            Some((l, d, t)) if *t > 0 => (
                format!("{l}  {} / {}", system::gib(*d), system::gib(*t)),
                *d as f64 / *t as f64,
            ),
            _ => (String::new(), 0.0),
        };
        f.render_widget(
            Gauge::default()
                .block(panel(" Download "))
                .gauge_style(Style::new().fg(VIOLET))
                .ratio(ratio.clamp(0.0, 1.0))
                .label(label),
            gauge,
        );
        f.render_widget(
            Paragraph::new(self.logline.clone())
                .style(Style::new().fg(Color::DarkGray))
                .block(panel(" Engine ")),
            log,
        );
    }

    fn chat_view(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let r = self.ready.as_ref();
        let extra = if self.show_cmd { 6 } else { 0 };
        let [status, convo, input] = Layout::vertical([
            Constraint::Length(3 + extra),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .areas(area);
        let tps = |v: Option<f64>, d: usize| v.map(|x| format!("{x:.d$}")).unwrap_or("-".into());
        let mut sl = vec![Line::from(format!(
            " {}  on  {}   |   decode {} tok/s   prompt {} tok/s   {} tokens",
            r.map(|r| r.model.as_str()).unwrap_or(""),
            r.map(|r| r.engine.as_str()).unwrap_or(""),
            tps(self.stats.decode_tps, 1),
            tps(self.stats.prefill_tps, 0),
            self.stats.tokens
        ))];
        if self.show_cmd {
            if let Some(r) = r {
                // one Line each: a "\n" inside a Span is not a line break in ratatui
                let dim = Style::new().fg(Color::DarkGray);
                sl.push(Line::from(vec![
                    Span::styled(format!(" API {}/v1", r.base), dim),
                    Span::styled("  (OpenAI and Anthropic; ", dim),
                    Span::styled("flelink connect", Style::new().fg(CYAN)),
                    Span::styled(" sets up Claude Code, Codex, OpenCode, pi)", dim),
                ]));
                sl.push(Line::from(Span::styled(format!(" {}", r.command), dim)));
            }
        }
        f.render_widget(
            Paragraph::new(sl)
                .wrap(Wrap { trim: true })
                .block(panel(" Live ")),
            status,
        );
        let mut t = Text::default();
        // the answer is labelled with the model that gave it
        let model = self
            .ready
            .as_ref()
            .map(|r| r.model.as_str())
            .unwrap_or("model");
        for (role, text) in &self.history {
            let (name, col) = if role == "user" {
                ("you", Color::Cyan)
            } else {
                (model, Color::Green)
            };
            t.lines.push(Line::from(Span::styled(
                name,
                Style::new().fg(col).add_modifier(Modifier::BOLD),
            )));
            if role == "assistant" && !self.reasoning.is_empty() && text.is_empty() {
                t.lines.push(Line::from(Span::styled(
                    format!(
                        "(thinking) {}",
                        self.reasoning
                            .chars()
                            .rev()
                            .take(300)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect::<String>()
                    ),
                    Style::new().fg(Color::DarkGray),
                )));
            }
            for l in text.lines() {
                t.lines.push(Line::from(l.to_string()));
            }
            if text.is_empty() && self.busy && role == "assistant" {
                t.lines.push(Line::from("..."));
            }
            t.lines.push(Line::from(""));
        }
        if let Some(e) = &self.err {
            t.lines.push(Line::from(Span::styled(
                e.clone(),
                Style::new().fg(Color::Red),
            )));
        }
        let h = convo.height.saturating_sub(2) as usize;
        let wrapped_est: usize = t
            .lines
            .iter()
            .map(|l| 1 + l.width() / (convo.width.max(10) as usize - 2))
            .sum();
        let scroll = wrapped_est.saturating_sub(h) as u16;
        f.render_widget(
            Paragraph::new(t)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(panel(" Conversation ")),
            convo,
        );
        f.render_widget(
            Paragraph::new(format!(
                "> {}{}",
                self.input,
                if self.busy { "" } else { "_" }
            ))
            .block(panel(if self.busy {
                " answering ... "
            } else {
                " your message "
            })),
            input,
        );
    }
}

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {k:<14}"), Style::new().fg(Color::DarkGray)),
        Span::raw(v.to_string()),
    ])
}

/// 12345 -> "12k", 3400000 -> "3.4M"
fn compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// The quantisation to load from a found repo: Q4_K_M when it fits (at most 95 % of the memory), otherwise the largest
/// model file group that does. Groups are what `hf::gguf_groups` returns; only plain model files count.
fn pick_group(groups: &[serde_json::Value], ram: u64) -> Option<&serde_json::Value> {
    let limit = ram / 100 * 95;
    let fits = |g: &&serde_json::Value| {
        g["kind"] == "model" && g["size"].as_u64().is_some_and(|s| s > 0 && s <= limit)
    };
    groups
        .iter()
        .filter(fits)
        .find(|g| {
            g["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case("Q4_K_M"))
        })
        .or_else(|| {
            groups
                .iter()
                .filter(fits)
                .max_by_key(|g| g["size"].as_u64().unwrap_or(0))
        })
}

/// A recipe for a found repo: the files of one quantisation, the default engine choice, nothing else.
fn recipe_toml(repo: &str, files: &[String], size: u64) -> String {
    let id: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let list = files
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "id = \"hf-{id}\"\ntitle = \"{repo}\"\n[model]\nrepo = \"{repo}\"\nfiles = [{list}]\nsize = {size}\n[engine]\nprefer = [\"fle\", \"upstream\"]\n"
    )
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use serde_json::json;

    fn g(name: &str, size: u64, kind: &str) -> serde_json::Value {
        json!({"name": name, "size": size, "kind": kind, "files": [{"path": format!("{name}.gguf")}]})
    }

    #[test]
    fn q4_k_m_is_taken_when_it_fits_else_the_largest_that_does() {
        let gb = 1u64 << 30;
        let groups = vec![
            g("Q8_0", 20 * gb, "model"),
            g("Q4_K_M", 12 * gb, "model"),
            g("Q2_K", 6 * gb, "model"),
            g("mmproj-f16", gb, "mmproj"),
        ];
        assert_eq!(pick_group(&groups, 32 * gb).unwrap()["name"], "Q4_K_M");
        // 16 GB of memory: Q4_K_M (12 GB) still fits within 95 %
        assert_eq!(pick_group(&groups, 16 * gb).unwrap()["name"], "Q4_K_M");
        // 10 GB: only Q2_K fits
        assert_eq!(pick_group(&groups, 10 * gb).unwrap()["name"], "Q2_K");
        // 4 GB: nothing fits (the mmproj file is not a model)
        assert!(pick_group(&groups, 4 * gb).is_none());
    }

    #[test]
    fn a_found_repo_becomes_a_valid_recipe() {
        let t = recipe_toml(
            "owner/My Model-GGUF",
            &[
                "Q4/a-00001-of-00002.gguf".into(),
                "Q4/a-00002-of-00002.gguf".into(),
            ],
            123,
        );
        let r = flelink_core::recipe::Recipe::parse(&t, "test");
        assert!(r.is_err(), "a repo name with a space is not a valid repo");
        let t = recipe_toml(
            "owner/My-Model-GGUF",
            &[
                "Q4/a-00001-of-00002.gguf".into(),
                "Q4/a-00002-of-00002.gguf".into(),
            ],
            123,
        );
        let r = flelink_core::recipe::Recipe::parse(&t, "test").unwrap();
        assert_eq!(r.id, "hf-owner-My-Model-GGUF");
        assert_eq!(r.model.files.len(), 2);
    }

    #[test]
    fn download_counts_are_short() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(12_400), "12k");
        assert_eq!(compact(3_400_000), "3.4M");
    }
}
