use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use image::DynamicImage;
use ratatui::layout::Rect;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::i18n;
use crate::cli::{Args, GraphicsArg};
use crate::engine::{self, Cmd, Ev, Info, PointerKind};
use crate::graphics::{Frame, Graphics, Protocol};
use crate::term::Caps;
use crate::theme::Theme;

pub struct Tab {
    pub target: String,
    pub url: String,
    pub title: String,
    pub can_back: bool,
    pub can_fwd: bool,
    pub editable: bool,
    pub zoom: f64,
    pub frame: Option<Arc<Frame>>,
    pub image: Option<Arc<DynamicImage>>,
    pub frame_seq: u64,
    pub started: bool,
}

impl Tab {
    fn new(target: String, zoom: f64) -> Self {
        Self {
            target,
            url: String::new(),
            title: String::new(),
            can_back: false,
            can_fwd: false,
            editable: false,
            zoom,
            frame: None,
            image: None,
            frame_seq: 0,
            started: false,
        }
    }

    pub fn label(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else if !self.url.is_empty() && self.url != "about:blank" {
            self.url.clone()
        } else {
            "new tab".into()
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Page,
    Url,
    Find,
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub focus: Focus,
    pub url_input: String,
    pub find_input: String,
    pub url_select_all: bool,

    pub status: String,
    pub loading: u32,
    cooldown: u8,
    pub help: bool,
    pub quit: bool,
    pub error: Option<String>,
    pub toast: Option<(String, Instant)>,
    pub tick: u64,

    pub tx: Sender<Cmd>,
    pub rx: Receiver<Ev>,
    pub info: Info,

    pub graphics: Graphics,
    pub theme: Theme,
    pub caps: Caps,
    pub content: Option<Rect>,
    pub url_rect: Option<Rect>,
    pub content_key: u64,
    /// When a character was last handed to the page. `activeElement` misses
    /// editors that live in an iframe, a shadow root or a canvas, so a recent
    /// keystroke also counts as "the page is taking text".
    typed_at: Option<Instant>,
    pub frame: Option<Arc<Frame>>,
    pub frame_image: Option<Arc<DynamicImage>>,
    pub viewport: (u32, u32),
    pub home: String,
    pub search: String,
    pub tab_hits: Vec<(u16, u16, u16, usize)>,
    dragging: bool,
    frame_seq_dump: usize,
    drag_at: (f64, f64),
}

impl App {
    pub fn new(args: &Args, caps: Caps) -> Result<App> {
        let proto = match args.graphics {
            GraphicsArg::Auto => {
                if caps.kitty {
                    Protocol::Kitty
                } else if caps.iterm {
                    Protocol::Iterm2
                } else {
                    Protocol::Blocks
                }
            }
            GraphicsArg::Kitty => Protocol::Kitty,
            GraphicsArg::Iterm => Protocol::Iterm2,
            GraphicsArg::Blocks => Protocol::Blocks,
            GraphicsArg::Off => Protocol::Off,
        };


        let search = args
            .search
            .clone()
            .unwrap_or_else(|| engine::DEFAULT_SEARCH.to_string());
        let home = args.home.clone().unwrap_or_default();

        let profile = args
            .profile
            .clone()
            .unwrap_or_else(engine::chrome::default_profile);

        let engine = engine::chrome::find(args.engine.as_deref()).ok_or_else(|| {
            anyhow::anyhow!(
                "找不到可用的 Chromium 引擎 / no Chromium engine found\n\
                 已尝试：YMW_ENGINE、BlinkTerm 自带引擎、Google Chrome、PATH\n\
                 try --engine /path/to/chrome-headless-shell"
            )
        })?;

        // blinkterm's rule, which matches what a HiDPI terminal expects:
        // two device pixels per CSS pixel once a cell is 28px tall, else one.
        let scale = match args.scale.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => {
                if caps.cell_h >= 28 {
                    2.0
                } else {
                    1.0
                }
            }
            other => other.parse::<f64>().unwrap_or(1.0).clamp(0.5, 4.0),
        };

        // Raw pixels over shared memory skip base64 and terminal-side PNG
        // decoding; only the kitty protocol has that transfer mode.
        let want_raw = match args.frames.trim().to_ascii_lowercase().as_str() {
            "raw" => true,
            "png" => false,
            _ => !is_ssh(),
        };
        let raw_frames = want_raw;

        let cfg = engine::Config {
            engine,
            profile,
            user_agent: args.user_agent.clone().or_else(|| {
                Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                      (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36"
                    .to_string())
            }),
            extra_args: args.engine_arg.clone(),
            no_sandbox: args.no_sandbox,
            search: search.clone(),
            zoom: args.zoom.clamp(0.25, 4.0),
            scale,
            fps: args.fps,
            raw_frames: raw_frames && proto == Protocol::Kitty,
        };

        let handle = engine::start(cfg)?;

        // A wrong cell height makes the screenshot look squashed once kitty
        // stretches it onto the cell grid, so let the user override it.
        let (cell_w, cell_h) = match args.cell_aspect {
            Some(r) if (1.0..=4.0).contains(&r) => (caps.cell_w, (caps.cell_w as f32 * r) as u16),
            _ => (caps.cell_w, caps.cell_h),
        };

        let app = App {
            tabs: Vec::new(),
            active: 0,
            focus: Focus::Page,
            url_input: String::new(),
            find_input: String::new(),
            url_select_all: false,
            status: i18n::t("启动中…", "starting…").into(),
            loading: 0,
            cooldown: 0,
            help: false,
            quit: false,
            error: None,
            toast: None,
            tick: 0,
            tx: handle.tx,
            rx: handle.rx,
            info: handle.info,
            graphics: Graphics::new(proto, cell_w, cell_h),
            theme: Theme::from_light(caps.light),
            caps,
            content: None,
            url_rect: None,
            content_key: 0,
            typed_at: None,
            frame: None,
            frame_image: None,
            viewport: (0, 0),
            home,
            search,
            tab_hits: Vec::new(),
            dragging: false,
            frame_seq_dump: 0,
            drag_at: (0.0, 0.0),
        };

        let start = if args.target.trim().is_empty() {
            None
        } else {
            Some(app.normalize(&args.target))
        };
        app.send(Cmd::NewTab(start));
        Ok(app)
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn target(&self) -> Option<String> {
        self.tab().map(|t| t.target.clone())
    }

    pub fn normalize(&self, input: &str) -> String {
        let s = input.trim();
        if s.is_empty() {
            return "about:blank".into();
        }
        if s.contains("://")
            || s.starts_with("about:")
            || s.starts_with("data:")
            || s.starts_with("file:")
            || s.starts_with("view-source:")
        {
            return s.to_string();
        }
        let host_like = !s.contains(' ')
            && (s.contains('.') || s.starts_with("localhost") || s.contains(":"));
        if host_like {
            format!("https://{s}")
        } else {
            self.search
                .replace("%s", &percent_encode(s))
        }
    }

    pub fn sync_viewport(&mut self) {
        if let Some(r) = self.content {
            let w = r.width as u32 * self.graphics.cell_w as u32;
            let h = r.height as u32 * self.graphics.cell_h as u32;
            if w > 0 && h > 0 && (w, h) != self.viewport {
                self.viewport = (w, h);
                self.send(Cmd::Viewport { w, h });
                self.bump();
                self.loading = self.loading.max(8);
            }
        }
    }

    fn request_capture(&mut self) {
        // Never ask for a frame before the UI told the engine how big the page
        // area is, otherwise the first screenshot comes back at the default
        // viewport and looks stretched.
        if self.viewport == (0, 0) {
            return;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return;
        }
        self.cooldown = 3;
        if let Some(t) = self.target() {
            self.send(Cmd::Capture(t));
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn on_ev(&mut self, ev: Ev) {
        match ev {
            Ev::Created {
                target,
                url,
                activate,
            } => {
                let mut tab = Tab::new(target, 1.0);
                tab.url = url;
                self.tabs.push(tab);
                if activate {
                    self.active = self.tabs.len() - 1;
                    self.loading = 25;
                }
            }
            Ev::Closed { target } => {
                if let Some(i) = self.tabs.iter().position(|t| t.target == target) {
                    let closing_active = i == self.active;
                    self.tabs.remove(i);
                    if self.tabs.is_empty() {
                        self.quit = true;
                    } else {
                        if i < self.active {
                            self.active -= 1;
                        }
                        self.active = self.active.min(self.tabs.len() - 1);
                        if closing_active {
                            // The closed tab was the one being streamed, so the
                            // survivor has no screencast: take over its cached
                            // frame and ask the engine to stream it.
                            self.graphics.invalidate();
                            self.frame = self.tab().and_then(|t| t.frame.clone());
                            self.frame_image = self.tab().and_then(|t| t.image.clone());
                            self.bump();
                            if let Some(t) = self.target() {
                                self.send(Cmd::Activate(t));
                            }
                            self.loading = 12;
                        }
                    }
                }
            }
            Ev::State {
                target,
                url,
                title,
                can_back,
                can_fwd,
                editable,
            } => {
                let zoom = self.tabs.first().map(|t| t.zoom).unwrap_or(1.0);
                if let Some(t) = self.tabs.iter_mut().find(|t| t.target == target) {
                    t.url = url.clone();
                    t.title = title;
                    t.can_back = can_back;
                    t.can_fwd = can_fwd;
                    t.editable = editable;
                    if !t.started {
                        t.started = true;
                        let is_start = url.is_empty() || url == "about:blank";
                        if is_start {
                            let home = self.home.clone();
                            let target2 = t.target.clone();
                            if home.is_empty() {
                                self.send(Cmd::StartPage(target2));
                            } else {
                                self.send(Cmd::Navigate {
                                    target: target2,
                                    url: home,
                                });
                            }
                        }
                    }
                }
                let _ = zoom;
                if self.focus == Focus::Page {
                    self.loading = self.loading.max(12);
                }
            }
            Ev::Frame { target, frame } => {
                if std::env::var_os("YMW_DUMP_FRAMES").is_some() {
                    let n = self.frame_seq_dump;
                    self.frame_seq_dump += 1;
                    let bytes = match &frame {
                        Frame::Png(b) => Some(b.clone()),
                        other => other.to_image().and_then(|img| {
                            let mut buf = Vec::new();
                            img.write_to(
                                &mut std::io::Cursor::new(&mut buf),
                                image::ImageFormat::Png,
                            )
                            .ok()
                            .map(|_| buf)
                        }),
                    };
                    if let Some(bytes) = bytes {
                        let _ = std::fs::write(format!("/tmp/ymw_frame_{n:02}.png"), bytes);
                    }
                }
                let frame = Arc::new(frame);
                // Only the pane that is on screen needs a decoded copy; the
                // others just keep their pixels until they are shown.
                let need_image = !self.graphics.is_graphics()
                    && self.tab().map(|t| t.target == target).unwrap_or(false);
                let image = if need_image {
                    frame.to_image().map(Arc::new)
                } else {
                    None
                };
                if let Some(t) = self.tabs.iter_mut().find(|t| t.target == target) {
                    t.frame_seq += 1;
                    t.frame = Some(frame.clone());
                    if let Some(img) = image.clone() {
                        t.image = Some(img);
                    }
                }
                if self.tab().map(|t| t.target == target).unwrap_or(false) {
                    let (fr, img) = self
                        .tab()
                        .map(|t| (t.frame.clone(), t.image.clone()))
                        .unwrap_or((None, None));
                    self.bump();
                    self.frame = fr;
                    self.frame_image = img;
                }
            }
            Ev::FindResult { found, .. } => {
                self.toast(if found { i18n::t("找到匹配", "match found") } else { i18n::t("没有匹配", "no match") });
            }
            Ev::Clipboard { text } => {
                if text.is_empty() {
                    self.toast(i18n::t("没有选中内容", "nothing is selected"));
                } else {
                    let n = text.chars().count();
                    let _ = copy_to_clipboard(&text);
                    self.toast(i18n::p(
                format!("已复制 {n} 个字符"),
                format!("copied {n} characters"),
            ));
                }
            }
            Ev::Restarted => {
                let urls: Vec<String> = self
                    .tabs
                    .iter()
                    .map(|t| t.url.clone())
                    .filter(|u| !u.is_empty() && u != "about:blank")
                    .collect();
                self.tabs.clear();
                self.active = 0;
                self.graphics.invalidate();
                self.bump();
                self.frame = None;
                self.frame_image = None;
                if urls.is_empty() {
                    self.send(Cmd::NewTab(None));
                } else {
                    for u in urls {
                        self.send(Cmd::NewTab(Some(u)));
                    }
                }
                self.toast(i18n::t("浏览器已重启", "the browser was restarted"));
            }
            Ev::Eval { .. } => {}
            Ev::Error(msg) => {
                self.error = Some(msg.clone());
                self.status = msg;
            }
        }
    }

    /// Anything that changes what should be on screen bumps this; the graphics
    /// layer only re-sends when the value or the pane geometry changes.
    fn bump(&mut self) {
        self.content_key = self.content_key.wrapping_add(1);
    }

    pub fn tick(&mut self) -> Result<bool> {
        while let Ok(ev) = self.rx.try_recv() {
            self.on_ev(ev);
        }

        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(k) => self.on_key(k),
                Event::Mouse(m) => self.on_mouse(m),
                Event::Resize(_, _) => {
                    self.graphics.invalidate();
                    self.bump();
                }
                _ => {}
            }
        }

        if self.loading > 0 {
            self.loading -= 1;
            // screencast keeps the page fresh; only ask for a screenshot when
            // we have nothing at all to show yet
            if self.frame.is_none() {
                self.request_capture();
            }
        }

        if let Some((_, at)) = &self.toast {
            if at.elapsed() > Duration::from_secs(2) {
                self.toast = None;
            }
        }

        self.tick += 1;
        Ok(self.quit)
    }

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Alt+1..9 cycles tabs on terminals that never send Ctrl+digit (they
        // need the kitty keyboard protocol, which not every terminal speaks).
        if alt && !ctrl {
            if let KeyCode::Char(c @ '1'..='9') = key.code {
                let idx = c as usize - '1' as usize;
                if idx < self.tabs.len() {
                    dbg(&format!("alt+{c}"));
                    self.activate(idx);
                }
                return;
            }
        }

        if self.help {
            // Nothing to restore: the picture was never taken down. It sits
            // below the text (negative z), so the page stays visible — and keeps
            // updating — around the panel.
            self.help = false;
            self.bump();
            return;
        }

        // ---- global shortcuts (Ctrl based, no Command) ----
        if ctrl && key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    if let Some(t) = self.target() {
                        self.send(Cmd::CopySelection(t));
                    }
                    return;
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    if let Some(text) = paste_from_clipboard() {
                        if let Some(t) = self.target() {
                            self.send(Cmd::Text { target: t, s: text });
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
        if ctrl {
            match key.code {
                KeyCode::Char('q') => {
                    self.quit = true;
                    return;
                }
                KeyCode::Char('l') => {
                    self.focus = Focus::Url;
                    self.url_input = self.tab().map(|t| t.url.clone()).unwrap_or_default();
                    self.url_select_all = true;
                    return;
                }
                KeyCode::Char('t') => {
                    self.send(Cmd::NewTab(None));
                    self.focus = Focus::Url;
                    self.url_input.clear();
                    self.url_select_all = true;
                    return;
                }
                KeyCode::Char('w') => {
                    if let Some(t) = self.target() {
                        self.send(Cmd::CloseTab(t));
                    }
                    return;
                }
                KeyCode::Char('r') => {
                    if let Some(t) = self.target() {
                        self.send(Cmd::Reload(t));
                    }
                    self.loading = 25;
                    return;
                }
                KeyCode::Char('f') => {
                    self.focus = Focus::Find;
                    self.find_input.clear();
                    return;
                }
                KeyCode::Char('c') => {
                    self.quit = true;
                    return;
                }
                KeyCode::Up => {
                    if let Some(t) = self.target() {
                        self.send(Cmd::Back(t));
                    }
                    self.loading = 25;
                    return;
                }
                KeyCode::Down => {
                    if let Some(t) = self.target() {
                        self.send(Cmd::Forward(t));
                    }
                    self.loading = 25;
                    return;
                }
                KeyCode::Left => {
                    self.cycle_tab(-1);
                    return;
                }
                KeyCode::Right => {
                    self.cycle_tab(1);
                    return;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    dbg(&format!("ctrl+{c}"));
                    let idx = c as usize - '1' as usize;
                    if idx < self.tabs.len() {
                        self.activate(idx);
                    }
                    return;
                }
                KeyCode::Char('=') | KeyCode::Char('+') => {
                    self.zoom_by(0.1);
                    return;
                }
                KeyCode::Char('-') | KeyCode::Char('_') => {
                    self.zoom_by(-0.1);
                    return;
                }
                KeyCode::Char('0') => {
                    self.zoom_to(1.0);
                    return;
                }
                KeyCode::Char('a') => {
                    if self.focus == Focus::Page {
                        if let Some(t) = self.target() {
                            self.send(Cmd::SelectAll(t));
                        }
                        return;
                    }
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Url => match key.code {
                KeyCode::Esc => {
                    self.focus = Focus::Page;
                    self.url_input.clear();
                }
                KeyCode::Enter => {
                    let input = self.url_input.clone();
                    if let Some(t) = self.target() {
                        self.send(Cmd::Navigate {
                            target: t,
                            url: self.normalize(&input),
                        });
                    }
                    self.focus = Focus::Page;
                    self.loading = 30;
                }
                KeyCode::Backspace => {
                    if self.url_select_all {
                        self.url_input.clear();
                        self.url_select_all = false;
                    } else {
                        self.url_input.pop();
                    }
                }
                KeyCode::Char(c) if !ctrl => {
                    if self.url_select_all {
                        self.url_input.clear();
                        self.url_select_all = false;
                    }
                    self.url_input.push(c);
                }
                _ => {}
            },
            Focus::Find => match key.code {
                KeyCode::Esc => {
                    self.focus = Focus::Page;
                    self.find_input.clear();
                }
                KeyCode::Enter => {
                    if let Some(t) = self.target() {
                        self.send(Cmd::Find {
                            target: t,
                            text: self.find_input.clone(),
                        });
                    }
                }
                KeyCode::Backspace => {
                    self.find_input.pop();
                }
                KeyCode::Char(c) if !ctrl => self.find_input.push(c),
                _ => {}
            },
            Focus::Page => {
                if key.code == KeyCode::Esc {
                    self.typed_at = None;
                    if let Some(t) = self.target() {
                        self.send(Cmd::Key {
                            target: t,
                            name: "Escape".into(),
                        });
                    }
                    return;
                }
                // `?` is a shortcut only while the page is not taking text.
                // Inside a page input it must reach the page, or typing a
                // question mark would open this dialog instead.
                if !ctrl && question_key(&key) && !self.page_is_typing() {
                    self.help = true;
                    return;
                }
                self.page_key(key, ctrl);
            }
        }
    }

    /// Whether the page is currently taking text, so a bare `?` must be typed
    /// instead of opening the help dialog.
    fn page_is_typing(&self) -> bool {
        if self.tab().map(|t| t.editable).unwrap_or(false) {
            return true;
        }
        self.typed_at
            .map(|t| t.elapsed() < Duration::from_millis(2000))
            .unwrap_or(false)
    }

    fn page_key(&mut self, key: KeyEvent, ctrl: bool) {
        let editable = self.tab().map(|t| t.editable).unwrap_or(false);
        let Some(target) = self.target() else { return };

        if editable && !ctrl {
            match key.code {
                // Terminals that report the base key rather than the glyph send
                // Shift+/ as '/', which would type a slash into the page.
                KeyCode::Char('/') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.typed_at = Some(Instant::now());
                    self.send(Cmd::Text {
                        target,
                        s: "?".into(),
                    });
                    return;
                }
                KeyCode::Char(c) => {
                    self.typed_at = Some(Instant::now());
                    self.send(Cmd::Text {
                        target,
                        s: c.to_string(),
                    });
                    return;
                }
                KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab => {
                    let name = match key.code {
                        KeyCode::Enter => "Enter",
                        KeyCode::Backspace => "Backspace",
                        _ => "Tab",
                    };
                    self.send(Cmd::Key {
                        target,
                        name: name.into(),
                    });
                    return;
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                    let name = match key.code {
                        KeyCode::Left => "ArrowLeft",
                        KeyCode::Right => "ArrowRight",
                        KeyCode::Up => "ArrowUp",
                        _ => "ArrowDown",
                    };
                    self.send(Cmd::Key {
                        target,
                        name: name.into(),
                    });
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll(-120.0),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(120.0),
            KeyCode::PageUp | KeyCode::Char('u') => self.scroll(-600.0),
            KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('d') => self.scroll(600.0),
            KeyCode::Home | KeyCode::Char('g') => self.scroll(-100000.0),
            KeyCode::End | KeyCode::Char('G') => self.scroll(100000.0),
            _ => {}
        }
    }

    fn scroll(&mut self, dy: f64) {
        let Some(target) = self.target() else { return };
        let (w, h) = self
            .content
            .map(|r| (r.width as f64, r.height as f64))
            .unwrap_or((80.0, 24.0));
        self.send(Cmd::Wheel {
            target,
            x: w * 0.5,
            y: h * 0.5,
            dy,
        });
        self.loading = self.loading.max(4);
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // tab strip click
                if let Some(hit) = self.tab_at(m.column, m.row) {
                    dbg(&format!("tab click -> {hit}"));
                    self.activate(hit);
                    return;
                }
                if let Some(rect) = self.url_rect {
                    if rect.height > 0
                        && m.column >= rect.x
                        && m.column < rect.x + rect.width
                        && m.row == rect.y
                    {
                        self.focus = Focus::Url;
                        self.url_input = self.tab().map(|t| t.url.clone()).unwrap_or_default();
                        self.url_select_all = true;
                        return;
                    }
                }
                if let Some(rect) = self.content {
                    if m.column >= rect.x
                        && m.column < rect.x + rect.width
                        && m.row >= rect.y
                        && m.row < rect.y + rect.height
                    {
                        let x = (m.column - rect.x) as f64 * self.graphics.cell_w as f64;
                        let y = (m.row - rect.y) as f64 * self.graphics.cell_h as f64;
                        if let Some(target) = self.target() {
                            self.send(Cmd::Pointer {
                                target,
                                x,
                                y,
                                kind: PointerKind::Down,
                            });
                            self.dragging = true;
                            self.drag_at = (x, y);
                        }
                        self.loading = self.loading.max(10);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let (Some(rect), Some(target)) = (self.content, self.target()) {
                    let x = (m.column.saturating_sub(rect.x)) as f64 * self.graphics.cell_w as f64;
                    let y = (m.row.saturating_sub(rect.y)) as f64 * self.graphics.cell_h as f64;
                    self.send(Cmd::Pointer {
                        target,
                        x,
                        y,
                        kind: PointerKind::Move,
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.dragging {
                    if let (Some(rect), Some(target)) = (self.content, self.target()) {
                        let x = (m.column.saturating_sub(rect.x)) as f64 * self.graphics.cell_w as f64;
                        let y = (m.row.saturating_sub(rect.y)) as f64 * self.graphics.cell_h as f64;
                        self.send(Cmd::Pointer {
                            target,
                            x,
                            y,
                            kind: PointerKind::Up,
                        });
                    }
                    self.dragging = false;
                }
            }
            MouseEventKind::ScrollDown => self.scroll(120.0),
            MouseEventKind::ScrollUp => self.scroll(-120.0),
            _ => {}
        }
    }

    /// Tab strip hit testing; the strip only exists when there are 2+ tabs.
    fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        // The row matters: without it every click whose column happened to fall
        // under a tab chip switched tabs, even clicks deep inside the page.
        self.tab_hits
            .iter()
            .find(|(r, a, b, _)| row == *r && col >= *a && col < *b)
            .map(|(_, _, _, i)| *i)
    }

    fn activate(&mut self, idx: usize) {
        dbg(&format!("activate {idx}"));
        if idx >= self.tabs.len() || idx == self.active {
            return;
        }
        self.active = idx;
        self.graphics.invalidate();
        self.bump();
        self.frame = self.tab().and_then(|t| t.frame.clone());
        self.frame_image = self.tab().and_then(|t| t.image.clone());
        if let Some(t) = self.target() {
            self.send(Cmd::Activate(t));
        }
        self.loading = 12;
    }

    fn cycle_tab(&mut self, delta: i32) {
        if self.tabs.len() < 2 {
            return;
        }
        let n = self.tabs.len() as i32;
        let mut i = self.active as i32 + delta;
        if i < 0 {
            i = n - 1;
        }
        if i >= n {
            i = 0;
        }
        self.activate(i as usize);
    }

    fn zoom_by(&mut self, delta: f64) {
        let z = self.tab().map(|t| t.zoom).unwrap_or(1.0) + delta;
        self.zoom_to(z);
    }

    fn zoom_to(&mut self, z: f64) {
        let z = (z.clamp(0.25, 4.0) * 100.0).round() / 100.0;
        if let Some(t) = self.tab_mut() {
            t.zoom = z;
        }
        if let Some(t) = self.target() {
            self.send(Cmd::Zoom { target: t, zoom: z });
        }
        // Do not re-place the current frame here: the new one arrives in a few
        // tens of milliseconds and bumps the key itself. Bumping now sent the
        // *old* picture again first, which is what made zoom flash.
        self.toast(i18n::p(
            format!("缩放 {}%", (z * 100.0).round() as i32),
            format!("zoom {}%", (z * 100.0).round() as i32),
        ));
    }

    pub fn run(&mut self) -> Result<()> {
        let backend = CrosstermBackend::new(std::io::stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        let _ = terminal.hide_cursor();

        let res = self.event_loop(&mut terminal);

        let mut out = std::io::stdout();
        let _ = self.graphics.clear_all(&mut out);
        let _ = self.tx.send(Cmd::Shutdown);
        // Make sure the browser never outlives the UI, even if the engine
        // thread is busy mid-capture.
        unsafe {
            libc::kill(-(self.info.pid as i32), libc::SIGTERM);
        }
        std::thread::sleep(Duration::from_millis(60));
        unsafe {
            libc::kill(-(self.info.pid as i32), libc::SIGKILL);
            libc::kill(self.info.pid as i32, libc::SIGKILL);
        }
        res
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        loop {
            if self.tick()? {
                break;
            }
            terminal.draw(|f| crate::ui::draw(f, self))?;
            self.sync_viewport();

            let rect = self.content;
            let key = self.content_key;
            if let (Some(rect), Some(frame)) = (rect, self.frame.clone()) {
                let mut out = std::io::stdout();
                let _ = self.graphics.present(&mut out, rect, key, &frame);
            }
        }
        Ok(())
    }
}

/// Whether the key press is a question mark, however the terminal spells it.
fn question_key(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('?') => true,
        KeyCode::Char('/') => key.modifiers.contains(KeyModifiers::SHIFT),
        _ => false,
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A shared memory object cannot cross an ssh connection, and tmux only passes
/// escape sequences through, so those sessions fall back to encoded frames.
fn dbg(msg: &str) {
    if std::env::var_os("YMW_DEBUG").is_none() {
        return;
    }
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/ymw_debug.log")
    {
        let _ = writeln!(f, "[ui] {msg}");
    }
}

fn is_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var("TERM").map(|t| t.contains("tmux")).unwrap_or(false)
}

fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()
        })
        .is_ok()
}

fn paste_from_clipboard() -> Option<String> {
    let out = std::process::Command::new("pbpaste").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}
