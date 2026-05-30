//! The Elm-style application state and pure render/update logic.
//!
//! Everything here is synchronous and terminal-free: [`App::render`] draws to
//! any [`ratatui::Frame`] (incl. a `TestBackend`), and [`App::on_key`] mutates
//! state and returns an optional [`DataRequest`] for the event loop to fetch.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::data::{DataRequest, EntityView, ScreenData};

/// A view in the navigation stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Skills,
    Instances { skill: String },
    Entity { page_id: String },
    Inbox,
    Events { instance: String },
    Search { query: String },
}

impl Screen {
    /// The [`DataRequest`] that loads this screen's data.
    pub fn request(&self) -> DataRequest {
        match self {
            Screen::Skills => DataRequest::Skills,
            Screen::Instances { skill } => DataRequest::Instances(skill.clone()),
            Screen::Entity { page_id } => DataRequest::Entity(page_id.clone()),
            Screen::Inbox => DataRequest::Inbox,
            Screen::Events { instance } => DataRequest::Events(instance.clone()),
            Screen::Search { query } => DataRequest::Search(query.clone()),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Screen::Skills => "Skills",
            Screen::Instances { .. } => "Instances",
            Screen::Entity { .. } => "Entity",
            Screen::Inbox => "Inbox",
            Screen::Events { .. } => "Events",
            Screen::Search { .. } => "Search",
        }
    }
}

/// The application state.
pub struct App {
    /// Navigation stack; the last element is the focused screen.
    stack: Vec<Screen>,
    /// Data loaded for the focused screen.
    data: ScreenData,
    /// Selected row index within the focused screen's list.
    selected: usize,
    /// Filter string (active while `filtering`).
    filter: String,
    /// Whether the filter input bar is active.
    filtering: bool,
    /// Whether the help overlay is shown.
    help: bool,
    /// Status / breadcrumb status message.
    status: String,
    /// Set once the user requests quit.
    should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// A fresh app focused on the Skills screen with no data yet loaded.
    pub fn new() -> Self {
        Self {
            stack: vec![Screen::Skills],
            data: ScreenData::Empty,
            selected: 0,
            filter: String::new(),
            filtering: false,
            help: false,
            status: String::new(),
            should_quit: false,
        }
    }

    /// Build an app whose initial focused screen is `screen` (e.g. to launch
    /// straight into the inbox or an instance's events). The caller fetches
    /// [`App::current_request`] to populate it.
    pub fn with_screen(screen: Screen) -> Self {
        let mut app = Self::new();
        app.stack = vec![screen];
        app
    }

    /// The focused screen.
    pub fn current(&self) -> &Screen {
        self.stack.last().expect("stack is never empty")
    }

    /// The [`DataRequest`] needed to populate the focused screen.
    pub fn current_request(&self) -> DataRequest {
        self.current().request()
    }

    /// Whether the user asked to quit.
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Store freshly fetched data, resetting the selection to the top.
    pub fn set_data(&mut self, data: ScreenData) {
        self.selected = 0;
        self.data = data;
    }

    /// Set the status / breadcrumb message.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Push a new screen; the caller should then fetch its data.
    fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
        self.selected = 0;
        self.filter.clear();
        self.data = ScreenData::Empty;
    }

    /// Pop the focused screen; returns true if a screen was popped.
    fn pop(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            self.selected = 0;
            self.filter.clear();
            self.data = ScreenData::Empty;
            true
        } else {
            false
        }
    }

    /// Indices of rows passing the current filter, in display order.
    fn filtered_indices(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        let matches = |hay: &str| needle.is_empty() || hay.to_lowercase().contains(&needle);
        match &self.data {
            ScreenData::Skills(v) => v
                .iter()
                .enumerate()
                .filter(|(_, r)| matches(&r.id) || matches(&r.description))
                .map(|(i, _)| i)
                .collect(),
            ScreenData::Instances(v) => v
                .iter()
                .enumerate()
                .filter(|(_, r)| matches(&r.page_id) || matches(&r.skill))
                .map(|(i, _)| i)
                .collect(),
            ScreenData::Inbox(v) => v
                .iter()
                .enumerate()
                .filter(|(_, r)| matches(&r.title) || matches(&r.label_skill))
                .map(|(i, _)| i)
                .collect(),
            ScreenData::Events(v) => v
                .iter()
                .enumerate()
                .filter(|(_, r)| matches(&r.title) || matches(&r.status))
                .map(|(i, _)| i)
                .collect(),
            ScreenData::Search(v) => v
                .iter()
                .enumerate()
                .filter(|(_, r)| matches(&r.page_id) || matches(&r.snippet))
                .map(|(i, _)| i)
                .collect(),
            ScreenData::Entity(_) | ScreenData::Empty => Vec::new(),
        }
    }

    /// The screen produced by drilling into the selected row, if any.
    fn drill_target(&self) -> Option<Screen> {
        let visible = self.filtered_indices();
        let &row = visible.get(self.selected)?;
        match &self.data {
            ScreenData::Skills(v) => v.get(row).map(|s| Screen::Instances {
                skill: s.id.clone(),
            }),
            ScreenData::Instances(v) => v.get(row).map(|i| Screen::Entity {
                page_id: i.page_id.clone(),
            }),
            // An inbox event drills into its assigned instance entity (if any).
            ScreenData::Inbox(v) => v.get(row).and_then(|it| {
                (!it.instance_page_id.is_empty()).then(|| Screen::Entity {
                    page_id: it.instance_page_id.clone(),
                })
            }),
            ScreenData::Search(v) => v.get(row).map(|r| Screen::Entity {
                page_id: r.page_id.clone(),
            }),
            // Events rows are leaves; the entity detail has its own links.
            ScreenData::Events(_) | ScreenData::Entity(_) | ScreenData::Empty => None,
        }
    }

    /// Handle a key event. Returns a [`DataRequest`] when the event loop must
    /// fetch data (after a drill-in or pop). Pure: no I/O happens here.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<DataRequest> {
        // Help overlay swallows everything except its own dismissal.
        if self.help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.help = false;
            }
            return None;
        }

        // Filter input mode.
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                    self.selected = 0;
                }
                KeyCode::Enter => {
                    self.filtering = false;
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.selected = 0;
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected = 0;
                }
                _ => {}
            }
            return None;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            KeyCode::Char('?') => {
                self.help = true;
                None
            }
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.clear();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                None
            }
            KeyCode::Enter => self
                .drill_target()
                .map(|screen| self.push_and_request(screen)),
            KeyCode::Esc | KeyCode::Char('h') => {
                if self.pop() {
                    Some(self.current_request())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn push_and_request(&mut self, screen: Screen) -> DataRequest {
        self.push(screen);
        self.current_request()
    }

    fn move_selection(&mut self, delta: i64) {
        let n = self.filtered_indices().len();
        if n == 0 {
            self.selected = 0;
            return;
        }
        let cur = self.selected as i64;
        let next = (cur + delta).clamp(0, n as i64 - 1);
        self.selected = next as usize;
    }

    // ---- rendering -------------------------------------------------------

    /// Render the whole UI: breadcrumb bar, main area, key-hint bar.
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // breadcrumb
                Constraint::Min(1),    // main
                Constraint::Length(1), // key bar
            ])
            .split(area);

        self.render_breadcrumb(frame, chunks[0]);
        self.render_main(frame, chunks[1]);
        self.render_keybar(frame, chunks[2]);

        if self.help {
            self.render_help(frame, area);
        }
    }

    fn breadcrumb(&self) -> String {
        let crumbs: Vec<String> = self
            .stack
            .iter()
            .map(|s| match s {
                Screen::Skills => "skills".to_string(),
                Screen::Instances { skill } => format!("skill:{skill}"),
                Screen::Entity { page_id } => format!("entity:{page_id}"),
                Screen::Inbox => "inbox".to_string(),
                Screen::Events { instance } => format!("events:{instance}"),
                Screen::Search { query } => format!("search:{query}"),
            })
            .collect();
        crumbs.join(" > ")
    }

    fn render_breadcrumb(&self, frame: &mut Frame, area: Rect) {
        let mut line = format!(" {} ", self.breadcrumb());
        if !self.status.is_empty() {
            line.push_str(&format!("- {} ", self.status));
        }
        let p = Paragraph::new(line).style(Style::default().add_modifier(Modifier::BOLD));
        frame.render_widget(p, area);
    }

    fn render_keybar(&self, frame: &mut Frame, area: Rect) {
        let hints = if self.filtering {
            "[type to filter]  <Enter> apply  <Esc> cancel"
        } else {
            match self.current() {
                Screen::Entity { .. } => "<Esc/h> back  </> filter  <?> help  <q> quit",
                _ => "<arrows/jk> move  <Enter> open  <Esc/h> back  </> filter  <?> help  <q> quit",
            }
        };
        let p = Paragraph::new(format!(" {hints} "))
            .style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_widget(p, area);
    }

    fn render_main(&self, frame: &mut Frame, area: Rect) {
        match &self.data {
            ScreenData::Entity(view) => self.render_entity(frame, area, view),
            _ => self.render_list(frame, area),
        }
    }

    fn list_rows(&self) -> Vec<String> {
        let visible = self.filtered_indices();
        match &self.data {
            ScreenData::Skills(v) => visible
                .iter()
                .filter_map(|&i| v.get(i))
                .map(|s| {
                    let tag = if s.event_typed { " [event]" } else { "" };
                    format!("{}{tag}  {}", s.id, s.description)
                })
                .collect(),
            ScreenData::Instances(v) => visible
                .iter()
                .filter_map(|&i| v.get(i))
                .map(|i| {
                    if i.at.is_empty() {
                        i.page_id.clone()
                    } else {
                        format!("{}  ({})", i.page_id, i.at)
                    }
                })
                .collect(),
            ScreenData::Inbox(v) => visible
                .iter()
                .filter_map(|&i| v.get(i))
                .map(|it| format!("{}  - {}", it.title, it.label_skill))
                .collect(),
            ScreenData::Events(v) => visible
                .iter()
                .filter_map(|&i| v.get(i))
                .map(|e| format!("[{}] {}", e.status, e.title))
                .collect(),
            ScreenData::Search(v) => visible
                .iter()
                .filter_map(|&i| v.get(i))
                .map(|r| format!("{}  - {}", r.page_id, r.snippet))
                .collect(),
            ScreenData::Entity(_) | ScreenData::Empty => Vec::new(),
        }
    }

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        let rows = self.list_rows();
        let title = format!(" {} ", self.current().kind());
        let items: Vec<ListItem> = if rows.is_empty() {
            vec![ListItem::new("(no rows)")]
        } else {
            rows.iter()
                .enumerate()
                .map(|(i, r)| {
                    let marker = if i == self.selected { "> " } else { "  " };
                    let style = if i == self.selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    ListItem::new(format!("{marker}{r}")).style(style)
                })
                .collect()
        };
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(list, area);

        if self.filtering || !self.filter.is_empty() {
            // Overlay the filter input on the top border line.
            let filter_area = Rect {
                x: area.x + 2,
                y: area.y,
                width: area.width.saturating_sub(4),
                height: 1,
            };
            let p = Paragraph::new(format!("/{}", self.filter));
            frame.render_widget(p, filter_area);
        }
    }

    fn render_entity(&self, frame: &mut Frame, area: Rect, view: &EntityView) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);

        // Left: title + frontmatter + body.
        let mut left: Vec<Line> = vec![
            Line::from(Span::styled(
                view.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("id: {}", view.page_id)),
        ];
        if !view.frontmatter_json.is_empty() && view.frontmatter_json != "{}" {
            left.push(Line::from(""));
            left.push(Line::from(format!(
                "frontmatter: {}",
                view.frontmatter_json
            )));
        }
        left.push(Line::from(""));
        for l in view.body.lines() {
            left.push(Line::from(l.to_string()));
        }
        let body = Paragraph::new(left)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Entity "));
        frame.render_widget(body, cols[0]);

        // Right: outgoing links + backlinks.
        let mut right: Vec<Line> = vec![Line::from(Span::styled(
            "Outgoing links",
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        if view.outgoing_links.is_empty() {
            right.push(Line::from("  (none)"));
        } else {
            for l in &view.outgoing_links {
                right.push(Line::from(format!("  -> {}", l.target)));
            }
        }
        right.push(Line::from(""));
        right.push(Line::from(Span::styled(
            "Backlinks",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if view.backlinks.is_empty() {
            right.push(Line::from("  (none)"));
        } else {
            for b in &view.backlinks {
                right.push(Line::from(format!(
                    "  <- {} ({})",
                    b.src_page, b.link_skill
                )));
            }
        }
        let links = Paragraph::new(right)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Links "));
        frame.render_widget(links, cols[1]);
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let text = vec![
            Line::from(Span::styled(
                "escurel-tui - keys",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("  up/k  up        down/j  down"),
            Line::from("  Enter  open     Esc/h  back"),
            Line::from("  /  filter       ?  toggle help"),
            Line::from("  q  quit"),
        ];
        // Centre a small box.
        let w = 40.min(area.width);
        let h = 9.min(area.height);
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(w)) / 2,
            y: area.y + (area.height.saturating_sub(h)) / 2,
            width: w,
            height: h,
        };
        let p = Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Help "));
        frame.render_widget(Clear, rect);
        frame.render_widget(p, rect);
    }
}
