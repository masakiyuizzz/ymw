pub mod cdp;
pub mod chrome;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};

use cdp::Cdp;

use crate::graphics::Frame;
use crate::i18n;

#[derive(Clone)]
pub struct Config {
    pub engine: chrome::Engine,
    pub profile: PathBuf,
    pub user_agent: Option<String>,
    pub extra_args: Vec<String>,
    pub no_sandbox: bool,
    pub search: String,
    pub zoom: f64,
    /// device pixels per CSS pixel
    pub scale: f64,
    pub fps: u32,
    /// ask the engine for JPEG and hand the terminal raw pixels
    pub raw_frames: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Move,
    Up,
}

#[derive(Debug, Clone)]
pub enum Cmd {
    NewTab(Option<String>),
    StartPage(String),
    CloseTab(String),
    Activate(String),
    Navigate { target: String, url: String },
    Back(String),
    Forward(String),
    Reload(String),
    Zoom { target: String, zoom: f64 },
    Viewport { w: u32, h: u32 },
    Capture(String),
    Pointer { target: String, x: f64, y: f64, kind: PointerKind },
    Wheel { target: String, x: f64, y: f64, dy: f64 },
    Text { target: String, s: String },
    Key { target: String, name: String },
    SelectAll(String),
    CopySelection(String),
    Find { target: String, text: String },
    Eval { target: String, js: String },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Ev {
    Created {
        target: String,
        url: String,
        activate: bool,
    },
    Closed { target: String },
    State {
        target: String,
        url: String,
        title: String,
        can_back: bool,
        can_fwd: bool,
        editable: bool,
    },
    Frame { target: String, frame: Frame },
    Clipboard { text: String },
    FindResult { #[allow(dead_code)] target: String, found: bool },
    Eval { #[allow(dead_code)] target: String, value: serde_json::Value },
    Restarted,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct Info {
    pub product: String,
    pub version: String,
    pub pid: u32,
}

pub struct Handle {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<Ev>,
    pub info: Info,
}

struct Tab {
    session: String,
    zoom: f64,
    applied: (u32, u32, f64),
    /// a <video>/<audio> is currently playing
    media: bool,
    last_media_check: Instant,
}

struct Engine {
    cdp: Option<Cdp>,
    tx: Sender<Ev>,
    cfg: Config,
    tabs: HashMap<String, Tab>,
    viewport: (u32, u32),
    casting: Option<String>,
    last_frame: Instant,
    last_capture: Instant,
    /// targets this program asked for, so discovery does not adopt them twice
    pending: HashSet<String>,
    /// newest frame waiting for the frame-rate budget
    held: Option<(String, Frame)>,
    /// captures to take shortly after a navigation, when the page has painted
    settle: Vec<Instant>,
}

pub fn start(cfg: Config) -> Result<Handle> {
    let (mut cdp, product, pid) = spawn_engine(&cfg)?;
    // Popups (target=_blank, window.open, sites that open the video in a new
    // window) are separate CDP targets. Without discovery they keep running -
    // audio included - while the pane shows the old page forever.
    let _ = cdp.call(
        "Target.setDiscoverTargets",
        Some(json!({ "discover": true })),
        None,
    );
    let (ctx, crx) = channel::<Cmd>();
    let (etx, erx) = channel::<Ev>();

    let engine = Engine {
        cdp: Some(cdp),
        tx: etx,
        cfg: cfg.clone(),
        tabs: HashMap::new(),
        viewport: (900, 600),
        casting: None,
        last_frame: Instant::now(),
        last_capture: Instant::now(),
        pending: HashSet::new(),
        held: None,
        settle: Vec::new(),
    };

    std::thread::spawn(move || engine.run(crx));

    Ok(Handle {
        tx: ctx,
        rx: erx,
        info: Info {
            product,
            version: cfg.engine.version.clone(),
            pid,
        },
    })
}

fn launch_args_with(cfg: &Config, no_sandbox: bool, profile: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--headless".into(),
        "--disable-gpu".into(),
        "--disable-dev-shm-usage".into(),
        "--hide-scrollbars".into(),
        "--use-mock-keychain".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-blink-features=AutomationControlled".into(),
        "--force-device-scale-factor=1".into(),
        "--remote-debugging-pipe".into(),
        format!("--user-data-dir={}", profile.display()),
    ];
    if cfg.engine.kind == chrome::EngineKind::HeadlessShell {
        a.push("--ozone-platform=headless".into());
    }
    if let Some(ua) = &cfg.user_agent {
        a.push(format!("--user-agent={ua}"));
    }
    if no_sandbox {
        a.push("--no-sandbox".into());
    }
    a.extend(cfg.extra_args.iter().cloned());
    a.push("about:blank".into());
    a
}

fn log_path(cfg: &Config) -> PathBuf {
    cfg.profile.join("chrome.log")
}

/// Chromium's own log is the fastest reliable signal that its sandbox cannot
/// work here, and it also explains why a browser died mid-session.
fn log_complains_about_sandbox(cfg: &Config) -> bool {
    let Ok(text) = std::fs::read_to_string(log_path(cfg)) else {
        return false;
    };
    text.contains("sandbox initialization failed")
        || text.contains("GPU process isn't usable")
        || text.contains("Failed to create a ProcessSingleton")
}

fn log_tail(cfg: &Config) -> String {
    let Ok(text) = std::fs::read_to_string(log_path(cfg)) else {
        return String::new();
    };
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > 3 {
        lines = lines.split_off(lines.len() - 3);
    }
    lines.join(" | ")
}

fn spawn_once(cfg: &Config, no_sandbox: bool, profile: Option<&Path>) -> Result<(Cdp, String)> {
    let profile = profile.unwrap_or(&cfg.profile);
    let _ = std::fs::create_dir_all(profile);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path(cfg))
        .ok();
    let args = launch_args_with(cfg, no_sandbox, profile);
    let mut cdp = Cdp::spawn(&cfg.engine.path, &args, log)?;

    std::thread::sleep(Duration::from_millis(350));
    if log_complains_about_sandbox(cfg) {
        cdp.kill();
        return Err(anyhow!("sandbox unusable"));
    }

    let v = cdp.call("Browser.getVersion", None, None)?;
    let product = v["product"].as_str().unwrap_or("Chromium").to_string();
    // A browser that cannot spawn renderers still answers getVersion and dies
    // right after the first real target is created, so do that before trusting
    // it.
    cdp.call(
        "Target.createTarget",
        Some(json!({ "url": "about:blank" })),
        None,
    )?;
    std::thread::sleep(Duration::from_millis(450));
    if log_complains_about_sandbox(cfg) {
        cdp.kill();
        return Err(anyhow!("sandbox unusable"));
    }
    let again = cdp.call("Browser.getVersion", None, None)?;
    if again["product"].is_null() {
        cdp.kill();
        return Err(anyhow!("browser died during start-up"));
    }
    Ok((cdp, product))
}

fn spawn_engine(cfg: &Config) -> Result<(Cdp, String, u32)> {
    // 1. default profile, sandboxed
    // 2. default profile, no sandbox (sandbox unusable)
    // 3. throwaway profile (the real one is locked by another instance)
    let fallback = cfg.profile.with_file_name(format!(
        "{}-{}",
        cfg.profile
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "profile".into()),
        std::process::id()
    ));
    let attempts: Vec<(bool, Option<PathBuf>)> = if cfg.no_sandbox {
        vec![(true, None), (true, Some(fallback))]
    } else {
        vec![(false, None), (true, None), (true, Some(fallback))]
    };
    let mut last: Option<anyhow::Error> = None;
    for (no_sandbox, profile) in attempts {
        match spawn_once(cfg, no_sandbox, profile.as_deref()) {
            Ok((cdp, product)) => {
                let pid = cdp.pid;
                return Ok((cdp, product, pid));
            }
            Err(e) => last = Some(e),
        }
    }
    let tail = log_tail(cfg);
    let mut msg = String::from(i18n::t(
        "无法启动浏览器引擎\n",
        "cannot start the browser engine\n",
    ));
    msg.push_str(&i18n::t(
        "  没有找到可驱动的 Chromium：装一个 Chrome / Chromium / Edge，\n  \
         用 `ymw --install-engine` 存一份自己的，或用 --engine 指定路径。\n",
        "  no usable Chromium found: install Chrome / Chromium / Edge, keep a\n  \
         private copy with `ymw --install-engine`, or point at one with --engine.\n",
    ));
    if let Some(e) = last {
        msg.push_str(&format!("  {e}\n"));
    }
    if !tail.is_empty() {
        msg.push_str(&format!("  chrome.log: {tail}\n"));
    }
    msg.push_str(&format!("  log: {}\n", log_path(cfg).display()));
    Err(anyhow!(msg))
}

impl Engine {
    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Cmd::Shutdown) => break,
                Ok(cmd) => {
                    if let Err(e) = self.handle(cmd) {
                        let dead = self.cdp.as_ref().map(|c| c.is_dead()).unwrap_or(true);
                        if dead && self.restart().is_ok() {
                            // the UI rebuilds its tabs from Ev::Restarted
                        } else {
                            let _ = self.tx.send(Ev::Error(e.to_string()));
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            self.pump_events();
            self.idle_tick();
        }
        if let Some(cdp) = &mut self.cdp {
            cdp.kill();
        }
    }

    /// Chromium's screencast barely produces anything for `<video>` in headless
    /// mode (a playing video yields ~2fps while a CSS animation yields 60fps),
    /// so while media is playing we pull frames with captureScreenshot instead.
    /// Send the newest held frame if the frame-rate budget allows.
    fn flush_frame(&mut self) {
        let interval = Duration::from_millis((1000 / self.cfg.fps.max(1).min(60)) as u64);
        if self.last_frame.elapsed() < interval {
            return;
        }
        let Some((target, frame)) = self.held.take() else {
            return;
        };
        self.last_frame = Instant::now();
        debug(&format!(
            "frame -> ui ({} bytes)",
            match &frame {
                Frame::Png(b) => b.len(),
                Frame::Rgb { data, .. } => data.len(),
            }
        ));
        let _ = self.tx.send(Ev::Frame { target, frame });
    }

    fn idle_tick(&mut self) {
        self.flush_frame();

        // A navigation repaints in stages (redirect interstitial, then content)
        // and the screencast may fall silent right after the first one, so take
        // a few deliberate shots to guarantee the final state is shown.
        let now = Instant::now();
        while let Some(at) = self.settle.first().copied() {
            if now < at {
                break;
            }
            self.settle.remove(0);
            if let Some(t) = self.casting.clone() {
                let _ = self.capture(&t);
            }
        }

        let Some(target) = self.casting.clone() else { return };
        let due_check = self
            .tabs
            .get(&target)
            .map(|t| t.last_media_check.elapsed() > Duration::from_millis(700))
            .unwrap_or(false);
        if due_check {
            self.poll_media(&target);
        }
        let playing = self.tabs.get(&target).map(|t| t.media).unwrap_or(false);
        if !playing {
            return;
        }
        let interval = Duration::from_millis((1000 / self.cfg.fps.max(1).min(30)) as u64);
        if self.last_capture.elapsed() >= interval {
            self.last_capture = Instant::now();
            let _ = self.capture(&target);
        }
    }

    fn poll_media(&mut self, target: &str) {
        if let Some(tab) = self.tabs.get_mut(target) {
            tab.last_media_check = Instant::now();
        }
        let Ok(session) = self.session(target) else { return };
        let js = "[...document.querySelectorAll('video,audio')]\
                  .some(m=>!m.paused&&!m.ended&&m.readyState>2)";
        let Ok(cdp) = self.cdp() else { return };
        let res = cdp.call(
            "Runtime.evaluate",
            Some(json!({ "expression": js, "returnByValue": true })),
            Some(&session),
        );
        let playing = res
            .ok()
            .and_then(|v| v["result"]["value"].as_bool())
            .unwrap_or(false);
        if let Some(tab) = self.tabs.get_mut(target) {
            tab.media = playing;
        }
    }

    fn restart(&mut self) -> Result<()> {
        if let Some(cdp) = &mut self.cdp {
            cdp.kill();
        }
        self.cdp = None;
        self.tabs.clear();
        self.casting = None;
        let (cdp, _product, _pid) = spawn_engine(&self.cfg)?;
        self.cdp = Some(cdp);
        let _ = self.tx.send(Ev::Restarted);
        Ok(())
    }

    fn cdp(&mut self) -> Result<&mut Cdp> {
        self.cdp
            .as_mut()
            .ok_or_else(|| anyhow!("browser is not running"))
    }

    fn pump_events(&mut self) {
        let events = match &mut self.cdp {
            Some(cdp) => cdp.drain_events(),
            None => return,
        };
        for ev in events {
            if let Err(e) = self.handle_event(ev) {
                debug(&format!("event error: {e}"));
            }
        }
    }

    fn handle_event(&mut self, ev: Value) -> Result<()> {
        let method = ev
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        match method.as_str() {
            "Page.screencastFrame" => self.on_frame(&ev),
            "Target.targetCreated" | "Target.targetInfoChanged" => self.on_target_info(&ev),
            "Target.targetDestroyed" => self.on_target_gone(&ev),
            "Page.frameNavigated" => {
                // Only the top frame matters; iframes navigate constantly and
                // restarting the screencast for them would thrash.
                if !ev["params"]["frame"]["parentId"].is_null() {
                    return Ok(());
                }
                let Some(target) = self.target_of(&ev) else {
                    return Ok(());
                };
                debug(&format!(
                    "navigated {} -> {}",
                    target,
                    ev["params"]["frame"]["url"].as_str().unwrap_or("")
                ));
                // A cross-document navigation replaces the compositor, and the
                // headless screencast session dies with it. Starting it again
                // is what makes a clicked search result or video show up.
                // "already active" means Chromium kept the screencast bound to
                // the document that just went away; stop it so the next start
                // attaches to the new document and pushes its paints.
                if let Ok(session) = self.session(&target) {
                    if let Some(cdp) = &mut self.cdp {
                        let _ = cdp.call("Page.stopScreencast", None, Some(&session));
                    }
                }
                let _ = self.start_screencast(&target);
                let _ = self.sync(&target);
                let now = Instant::now();
                self.settle = vec![
                    now + Duration::from_millis(450),
                    now + Duration::from_millis(1400),
                    now + Duration::from_millis(3000),
                    now + Duration::from_millis(6000),
                ];
                Ok(())
            }
            "Page.navigatedWithinDocument" => {
                if let Some(target) = self.target_of(&ev) {
                    debug("navigated within document");
                    let _ = self.sync(&target);
                }
                Ok(())
            }
            "Page.loadEventFired" => {
                if let Some(target) = self.target_of(&ev) {
                    let _ = self.sync(&target);
                    let _ = self.capture(&target);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn target_of(&self, ev: &Value) -> Option<String> {
        let session = ev.get("sessionId")?.as_str()?;
        self.tabs
            .iter()
            .find(|(_, t)| t.session == session)
            .map(|(k, _)| k.clone())
    }

    fn on_frame(&mut self, ev: &Value) -> Result<()> {
        let frame_session = ev["params"]["sessionId"].as_i64().unwrap_or(0);
        let Some(session) = ev.get("sessionId").and_then(|s| s.as_str()).map(String::from) else {
            return Ok(());
        };
        let target = self
            .tabs
            .iter()
            .find(|(_, t)| t.session == session)
            .map(|(k, _)| k.clone());
        if let Some(cdp) = &mut self.cdp {
            let r = cdp.call(
                "Page.screencastFrameAck",
                Some(json!({ "sessionId": frame_session })),
                Some(&session),
            );
            let _ = r;
        }
        let Some(target) = target else {
            return Ok(());
        };

        let interval = Duration::from_millis((1000 / self.cfg.fps.max(1).min(60)) as u64);
        if self.last_frame.elapsed() < interval {
            debug(&format!("drop frame, last sent {:?} ago", self.last_frame.elapsed()));
            return Ok(());
        }
        let Some(data) = ev["params"]["data"].as_str() else {
            return Ok(());
        };
        let Ok(bytes) = STANDARD.decode(data) else {
            return Ok(());
        };
        self.last_frame = Instant::now();
        if let Some(frame) = self.make_frame(bytes) {
            let _ = self.tx.send(Ev::Frame { target, frame });
        }
        Ok(())
    }

    fn on_target_info(&mut self, ev: &Value) -> Result<()> {
        let info = &ev["params"]["targetInfo"];
        if info["type"].as_str() != Some("page") {
            return Ok(());
        }
        let Some(id) = info["targetId"].as_str() else {
            return Ok(());
        };
        if self.tabs.contains_key(id) || self.pending.contains(id) {
            return Ok(());
        }
        let url = info["url"].as_str().unwrap_or("").to_string();
        let popup = info.get("openerId").map(|v| v.is_string()).unwrap_or(false);
        // Every fresh target starts blank: the browser's own start page, and
        // the split second before a popup navigates. Adopting those gives the
        // user empty tabs, so wait until there is a real url to show.
        if url.is_empty() || url == "about:blank" {
            return Ok(());
        }
        debug(&format!("adopt target {id} url={url} popup={popup}"));
        self.adopt(id, &url, popup)
    }

    fn on_target_gone(&mut self, ev: &Value) -> Result<()> {
        let Some(id) = ev["params"]["targetId"].as_str() else {
            return Ok(());
        };
        if self.tabs.remove(id).is_none() {
            return Ok(());
        }
        debug(&format!("target gone {id}"));
        if self.casting.as_deref() == Some(id) {
            self.casting = None;
        }
        let _ = self.tx.send(Ev::Closed {
            target: id.to_string(),
        });
        Ok(())
    }

    /// Take over a page target that this program did not create (a popup, a
    /// link with target=_blank, or a window a script opened).
    fn adopt(&mut self, target: &str, url: &str, activate: bool) -> Result<()> {
        let att = self.cdp()?.call(
            "Target.attachToTarget",
            Some(json!({ "targetId": target, "flatten": true })),
            None,
        )?;
        let session = att["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("attach failed"))?
            .to_string();
        self.cdp()?.call("Page.enable", None, Some(&session))?;
        let _ = self.cdp()?.call("Runtime.enable", None, Some(&session));

        self.tabs.insert(
            target.to_string(),
            Tab {
                session,
                zoom: self.cfg.zoom,
                applied: (0, 0, 0.0),
                media: false,
                last_media_check: Instant::now() - Duration::from_secs(5),
            },
        );
        if let Some(prev) = self.casting.clone() {
            self.pause_media(&prev);
        }
        self.apply_metrics(target)?;
        self.start_screencast(target)?;
        let _ = self.sync(target);
        let _ = self.tx.send(Ev::Created {
            target: target.to_string(),
            url: url.to_string(),
            activate,
        });
        Ok(())
    }

    /// Silence a tab that is no longer on screen. Without this a page opened in
    /// a second tab keeps playing, and the sound stacks up with the new page.
    fn pause_media(&mut self, target: &str) {
        let Ok(session) = self.session(target) else {
            return;
        };
        debug(&format!("pause media {target}"));
        let js = "[...document.querySelectorAll('video,audio')]                  .forEach(m=>{try{m.pause()}catch(e){}})";
        if let Ok(cdp) = self.cdp() {
            let _ = cdp.call(
                "Runtime.evaluate",
                Some(json!({ "expression": js, "returnByValue": false })),
                Some(&session),
            );
        }
        if let Some(tab) = self.tabs.get_mut(target) {
            tab.media = false;
        }
    }


    fn handle(&mut self, cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::NewTab(url) => self.new_tab(url)?,
            Cmd::StartPage(target) => {
                let html = start_page(&self.cfg.search);
                self.set_content(&target, &html)?;
                self.sync(&target)?;
            }
            Cmd::CloseTab(target) => {
                let cdp = self.cdp()?;
                cdp.call(
                    "Target.closeTarget",
                    Some(json!({ "targetId": target })),
                    None,
                )?;
                self.tabs.remove(&target);
                if self.casting.as_deref() == Some(target.as_str()) {
                    self.casting = None;
                }
                let _ = self.tx.send(Ev::Closed { target });
            }
            Cmd::Activate(target) => {
                if let Some(prev) = self.casting.clone() {
                    if prev != target {
                        self.pause_media(&prev);
                    }
                }
                self.apply_metrics(&target)?;
                self.start_screencast(&target)?;
                self.sync(&target)?;
            }
            Cmd::Navigate { target, url } => {
                let session = self.session(&target)?;
                let cdp = self.cdp()?;
                cdp.call("Page.navigate", Some(json!({ "url": url })), Some(&session))?;
                self.sync(&target)?;
            }
            Cmd::Back(target) => {
                self.step_history(&target, -1)?;
                self.sync(&target)?;
            }
            Cmd::Forward(target) => {
                self.step_history(&target, 1)?;
                self.sync(&target)?;
            }
            Cmd::Reload(target) => {
                let session = self.session(&target)?;
                let cdp = self.cdp()?;
                cdp.call(
                    "Page.reload",
                    Some(json!({ "ignoreCache": true })),
                    Some(&session),
                )?;
            }
            Cmd::Zoom { target, zoom } => {
                let z = zoom.clamp(0.25, 4.0);
                if let Some(tab) = self.tabs.get_mut(&target) {
                    tab.zoom = z;
                }
                self.apply_metrics(&target)?;
                self.start_screencast(&target)?;
            }
            Cmd::Viewport { w, h } => {
                self.viewport = (w.max(64), h.max(64));
                let targets: Vec<String> = self.tabs.keys().cloned().collect();
                for t in targets {
                    self.apply_metrics(&t)?;
                }
                if let Some(t) = self.casting.clone() {
                    self.start_screencast(&t)?;
                }
            }
            Cmd::Capture(target) => self.capture(&target)?,
            Cmd::Pointer { target, x, y, kind } => self.pointer(&target, x, y, kind)?,
            Cmd::Wheel { target, x, y, dy } => {
                let session = self.session(&target)?;
                let z = self.cfg.scale * self.zoom_of(&target);
                let cdp = self.cdp()?;
                cdp.call(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": "mouseWheel", "x": x / z, "y": y / z,
                        "deltaX": 0.0, "deltaY": dy, "button": "none", "buttons": 0
                    })),
                    Some(&session),
                )?;
            }
            Cmd::Text { target, s } => {
                debug(&format!("text {target} {s:?}"));
                let session = self.session(&target)?;
                let cdp = self.cdp()?;
                cdp.call("Input.insertText", Some(json!({ "text": s })), Some(&session))?;
                self.sync(&target)?;
            }
            Cmd::Key { target, name } => {
                debug(&format!("key {target} {name}"));
                self.press(&target, &name)?;
                self.sync(&target)?;
            }
            Cmd::SelectAll(target) => {
                self.select_all(&target)?;
            }
            Cmd::CopySelection(target) => {
                let session = self.session(&target)?;
                let cdp = self.cdp()?;
                let res = cdp.call(
                    "Runtime.evaluate",
                    Some(json!({
                        "expression": "window.getSelection().toString()",
                        "returnByValue": true
                    })),
                    Some(&session),
                )?;
                let text = res["result"]["value"].as_str().unwrap_or("").to_string();
                let _ = self.tx.send(Ev::Clipboard { text });
            }
            Cmd::Find { target, text } => {
                let session = self.session(&target)?;
                let expr = format!(
                    "window.find({}, false, false, true)",
                    serde_json::to_string(&text)?
                );
                let cdp = self.cdp()?;
                let res = cdp.call(
                    "Runtime.evaluate",
                    Some(json!({ "expression": expr, "returnByValue": true })),
                    Some(&session),
                )?;
                let found = res["result"]["value"].as_bool().unwrap_or(false);
                let _ = self.tx.send(Ev::FindResult { target, found });
            }
            Cmd::Eval { target, js } => {
                let session = self.session(&target)?;
                let cdp = self.cdp()?;
                let res = cdp.call(
                    "Runtime.evaluate",
                    Some(json!({ "expression": js, "returnByValue": true, "awaitPromise": true })),
                    Some(&session),
                )?;
                let value = res["result"].get("value").cloned().unwrap_or(Value::Null);
                let _ = self.tx.send(Ev::Eval { target, value });
            }
            Cmd::Shutdown => {}
        }
        Ok(())
    }

    fn session(&self, target: &str) -> Result<String> {
        self.tabs
            .get(target)
            .map(|t| t.session.clone())
            .ok_or_else(|| anyhow!("unknown tab"))
    }

    fn zoom_of(&self, target: &str) -> f64 {
        self.tabs.get(target).map(|t| t.zoom).unwrap_or(1.0)
    }

    fn new_tab(&mut self, url: Option<String>) -> Result<()> {
        let start = url.unwrap_or_else(|| "about:blank".to_string());
        let res = self.cdp()?.call(
            "Target.createTarget",
            Some(json!({ "url": start })),
            None,
        )?;
        let target = res["targetId"]
            .as_str()
            .ok_or_else(|| anyhow!("createTarget returned no id"))?
            .to_string();
        // discovery reports this target too; do not attach to it twice
        self.pending.insert(target.clone());
        let cdp = self.cdp()?;
        let att = cdp.call(
            "Target.attachToTarget",
            Some(json!({ "targetId": target, "flatten": true })),
            None,
        )?;
        let session = att["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("attach failed"))?
            .to_string();
        cdp.call("Page.enable", None, Some(&session))?;
        cdp.call("Runtime.enable", None, Some(&session))?;

        self.tabs.insert(
            target.clone(),
            Tab {
                session,
                zoom: self.cfg.zoom,
                applied: (0, 0, 0.0),
                media: false,
                last_media_check: Instant::now() - Duration::from_secs(5),
            },
        );
        self.pending.remove(&target);
        self.apply_metrics(&target)?;
        let _ = self.tx.send(Ev::Created {
            target: target.clone(),
            url: start,
            activate: true,
        });
        self.sync(&target)?;
        self.start_screencast(&target)?;
        Ok(())
    }

    fn start_screencast(&mut self, target: &str) -> Result<()> {
        if self.casting.as_deref() != Some(target) {
            if let Some(prev) = self.casting.clone() {
                if let Ok(session) = self.session(&prev) {
                    if let Some(cdp) = &mut self.cdp {
                        let _ = cdp.call("Page.stopScreencast", None, Some(&session));
                    }
                }
            }
        }
        let session = self.session(target)?;
        let (w, h) = self.viewport;
        // JPEG is both faster to produce and a fraction of the size of PNG;
        // the pixels are decoded here and handed over as raw RGB anyway.
        let format = if self.cfg.raw_frames { "jpeg" } else { "png" };
        let cdp = self.cdp()?;
        let res = cdp.call(
            "Page.startScreencast",
            Some(json!({
                "format": format,
                "quality": 80,
                "maxWidth": w,
                "maxHeight": h,
                "everyNthFrame": 1
            })),
            Some(&session),
        );
        // Restarting after a navigation is harmless and Chromium says so; that
        // is not something the user needs to see as an error.
        if let Err(e) = res {
            let msg = e.to_string();
            if !msg.contains("already activ") {
                return Err(e);
            }
        }
        self.casting = Some(target.to_string());
        Ok(())
    }

    fn set_content(&mut self, target: &str, html: &str) -> Result<()> {
        let session = self.session(target)?;
        let cdp = self.cdp()?;
        let tree = cdp.call("Page.getFrameTree", None, Some(&session))?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or_else(|| anyhow!("no frame"))?
            .to_string();
        cdp.call(
            "Page.setDocumentContent",
            Some(json!({ "frameId": frame, "html": html })),
            Some(&session),
        )?;
        Ok(())
    }

    fn apply_metrics(&mut self, target: &str) -> Result<()> {
        let (vw, vh) = self.viewport;
        let Some(tab) = self.tabs.get_mut(target) else {
            return Ok(());
        };
        if tab.applied == (vw, vh, tab.zoom) {
            return Ok(());
        }
        let zoom = tab.zoom;
        let dsf = (self.cfg.scale * zoom).clamp(0.5, 4.0);
        let css_w = ((vw as f64 / dsf).round() as u32).max(64);
        let css_h = ((vh as f64 / dsf).round() as u32).max(64);
        let session = tab.session.clone();
        let cdp = self.cdp()?;
        cdp.call(
            "Emulation.setDeviceMetricsOverride",
            Some(json!({
                "width": css_w, "height": css_h,
                "deviceScaleFactor": dsf, "mobile": false,
                "screenWidth": css_w, "screenHeight": css_h
            })),
            Some(&session),
        )?;
        if let Some(tab) = self.tabs.get_mut(target) {
            tab.applied = (vw, vh, zoom);
        }
        Ok(())
    }

    fn sync(&mut self, target: &str) -> Result<()> {
        let session = self.session(target)?;
        let js = "JSON.stringify({url:location.href,title:document.title,\
                  editable:(()=>{const a=document.activeElement;\
                  return document.designMode==='on'||\
                  (!!a&&(a.tagName==='INPUT'||a.tagName==='TEXTAREA'||a.isContentEditable))})()})";
        let cdp = self.cdp()?;
        let res = cdp.call(
            "Runtime.evaluate",
            Some(json!({ "expression": js, "returnByValue": true })),
            Some(&session),
        )?;
        let raw = res["result"]["value"].as_str().unwrap_or("{}");
        let v: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));

        let (mut can_back, mut can_fwd) = (false, false);
        if let Ok(h) = cdp.call("Page.getNavigationHistory", None, Some(&session)) {
            let idx = h["currentIndex"].as_i64().unwrap_or(0);
            let len = h["entries"].as_array().map(|a| a.len()).unwrap_or(0) as i64;
            can_back = idx > 0;
            can_fwd = idx + 1 < len;
        }

        debug(&format!(
            "state {target} editable={} url={}",
            v["editable"].as_bool().unwrap_or(false),
            v["url"].as_str().unwrap_or("")
        ));
        let _ = self.tx.send(Ev::State {
            target: target.to_string(),
            url: v["url"].as_str().unwrap_or("").to_string(),
            title: v["title"].as_str().unwrap_or("").to_string(),
            can_back,
            can_fwd,
            editable: v["editable"].as_bool().unwrap_or(false),
        });
        Ok(())
    }

    fn step_history(&mut self, target: &str, delta: i64) -> Result<()> {
        let session = self.session(target)?;
        let cdp = self.cdp()?;
        let h = cdp.call("Page.getNavigationHistory", None, Some(&session))?;
        let idx = h["currentIndex"].as_i64().unwrap_or(0);
        let want = idx + delta;
        let Some(entries) = h["entries"].as_array() else {
            return Ok(());
        };
        if want < 0 || want as usize >= entries.len() {
            return Ok(());
        }
        let id = entries[want as usize]["id"].as_i64().unwrap_or(0);
        cdp.call(
            "Page.navigateToHistoryEntry",
            Some(json!({ "entryId": id })),
            Some(&session),
        )?;
        Ok(())
    }

    fn pointer(&mut self, target: &str, x: f64, y: f64, kind: PointerKind) -> Result<()> {
        let session = self.session(target)?;
        let z = self.cfg.scale * self.zoom_of(target);
        let (cx, cy) = (x / z, y / z);
        let cdp = self.cdp()?;
        match kind {
            PointerKind::Down => {
                cdp.call(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": "mouseMoved", "x": cx, "y": cy,
                        "button": "none", "buttons": 0
                    })),
                    Some(&session),
                )?;
                cdp.call(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": "mousePressed", "x": cx, "y": cy,
                        "button": "left", "buttons": 1, "clickCount": 1
                    })),
                    Some(&session),
                )?;
            }
            PointerKind::Move => {
                cdp.call(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": "mouseMoved", "x": cx, "y": cy,
                        "button": "left", "buttons": 1
                    })),
                    Some(&session),
                )?;
            }
            PointerKind::Up => {
                cdp.call(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": "mouseReleased", "x": cx, "y": cy,
                        "button": "left", "buttons": 0, "clickCount": 1
                    })),
                    Some(&session),
                )?;
                self.sync(target)?;
            }
        }
        Ok(())
    }

    fn press(&mut self, target: &str, name: &str) -> Result<()> {
        let session = self.session(target)?;
        let (key, code, vk, text) = match name {
            "Enter" => ("Enter", "Enter", 13, Some("\r")),
            "Backspace" => ("Backspace", "Backspace", 8, None),
            "Tab" => ("Tab", "Tab", 9, None),
            "Escape" => ("Escape", "Escape", 27, None),
            "ArrowUp" => ("ArrowUp", "ArrowUp", 38, None),
            "ArrowDown" => ("ArrowDown", "ArrowDown", 40, None),
            "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37, None),
            "ArrowRight" => ("ArrowRight", "ArrowRight", 39, None),
            "PageUp" => ("PageUp", "PageUp", 33, None),
            "PageDown" => ("PageDown", "PageDown", 34, None),
            "Home" => ("Home", "Home", 36, None),
            "End" => ("End", "End", 35, None),
            other => (other, other, 0, None),
        };
        let mut down = json!({
            "type": "keyDown", "key": key, "code": code,
            "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk
        });
        if let Some(t) = text {
            down["text"] = json!(t);
            down["unmodifiedText"] = json!(t);
        }
        let cdp = self.cdp()?;
        cdp.call("Input.dispatchKeyEvent", Some(down), Some(&session))?;
        cdp.call(
            "Input.dispatchKeyEvent",
            Some(json!({
                "type": "keyUp", "key": key, "code": code,
                "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk
            })),
            Some(&session),
        )?;
        Ok(())
    }

    /// Select everything on the page.
    ///
    /// A synthetic `Ctrl+A` alone does nothing: Chromium only runs editing
    /// commands when the key event carries `commands`, which is what the
    /// devtools frontend sets. Text areas and inputs are handled by the same
    /// command, so one path covers both.
    fn select_all(&mut self, target: &str) -> Result<()> {
        let session = self.session(target)?;
        let cdp = self.cdp()?;
        cdp.call(
            "Input.dispatchKeyEvent",
            Some(json!({
                "type": "rawKeyDown", "key": "a", "code": "KeyA",
                "windowsVirtualKeyCode": 65, "nativeVirtualKeyCode": 65,
                "modifiers": 2, "commands": ["selectAll"]
            })),
            Some(&session),
        )?;
        cdp.call(
            "Input.dispatchKeyEvent",
            Some(json!({
                "type": "keyUp", "key": "a", "code": "KeyA",
                "windowsVirtualKeyCode": 65, "nativeVirtualKeyCode": 65,
                "modifiers": 2
            })),
            Some(&session),
        )?;

        // Say whether it actually took: an empty selection is the difference
        // between "handled" and "silently ignored".
        let js = "(()=>{const a=document.activeElement;\
                  if(a&&(a.tagName==='INPUT'||a.tagName==='TEXTAREA'))\
                    return 'field:'+(a.selectionEnd-a.selectionStart);\
                  return 'page:'+(window.getSelection?window.getSelection().toString().length:0)})()";
        if let Ok(res) = cdp.call(
            "Runtime.evaluate",
            Some(json!({ "expression": js, "returnByValue": true })),
            Some(&session),
        ) {
            debug(&format!(
                "selectAll {} {}",
                target,
                res["result"]["value"].as_str().unwrap_or("?")
            ));
        }
        Ok(())
    }
    /// Chromium hands back JPEG when raw frames are on: it encodes several
    /// times faster than PNG and the result is roughly a sixth of the size.
    fn make_frame(&self, bytes: Vec<u8>) -> Option<Frame> {
        if !self.cfg.raw_frames {
            return Some(Frame::Png(bytes));
        }
        let img = image::load_from_memory(&bytes).ok()?;
        let rgb = img.to_rgb8();
        let (w, h) = rgb.dimensions();
        Some(Frame::Rgb {
            w,
            h,
            data: rgb.into_raw(),
        })
    }

    fn capture(&mut self, target: &str) -> Result<()> {
        self.apply_metrics(target)?;
        let session = self.session(target)?;
        let format = if self.cfg.raw_frames { "jpeg" } else { "png" };
        let res = self.cdp()?.call(
            "Page.captureScreenshot",
            Some(json!({
                "format": format, "quality": 80,
                "optimizeForSpeed": true, "fromSurface": true
            })),
            Some(&session),
        )?;
        let Some(b64) = res["data"].as_str() else {
            return Ok(());
        };
        let bytes = STANDARD.decode(b64)?;
        if let Some(frame) = self.make_frame(bytes) {
            debug(&format!("frame -> ui ({} bytes)", match &frame {
                Frame::Png(b) => b.len(),
                Frame::Rgb { data, .. } => data.len(),
            }));
            let _ = self.tx.send(Ev::Frame {
                target: target.to_string(),
                frame,
            });
        }
        Ok(())
    }

}

pub const DEFAULT_SEARCH: &str = "https://www.baidu.com/s?wd=%s";

pub fn start_page(search: &str) -> String {
    let prefix = search
        .split("%s")
        .next()
        .unwrap_or("https://www.bing.com/search?q=")
        .to_string();
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>ymw</title>
<style>
 html,body{{height:100%;margin:0;background:#14161c;color:#d7dde8;
   font:15px/1.6 -apple-system,BlinkMacSystemFont,"PingFang SC",sans-serif}}
 .wrap{{height:100%;display:flex;flex-direction:column;align-items:center;
   justify-content:center;gap:16px}}
 .logo{{font:600 46px/1 ui-monospace,Menlo,monospace;color:#e9b46a;letter-spacing:.1em}}
 .sub{{color:#8b95a7;font-size:13px;letter-spacing:.02em}}
 form{{width:min(600px,74vw);display:flex;gap:8px;margin-top:6px}}
 input{{flex:1;background:#1d2029;border:1px solid #333a49;color:#d7dde8;
   border-radius:9px;padding:11px 14px;font-size:14px;outline:none}}
 input:focus{{border-color:#e9b46a;box-shadow:0 0 0 3px rgba(233,180,106,.12)}}
 .keys{{display:flex;gap:14px;flex-wrap:wrap;justify-content:center;
   margin-top:8px;color:#8b95a7;font-size:12px}}
 kbd{{background:#1d2029;border:1px solid #333a49;border-radius:5px;
   padding:2px 7px;color:#b7c0cf;font:12px ui-monospace,monospace}}
</style>
<div class="wrap">
  <div class="logo">ymw</div>
  <div class="sub">终端里的浏览器 · 由 headless Chromium 渲染</div>
  <form onsubmit="event.preventDefault();go(this.q.value)">
    <input name="q" placeholder="搜索或输入网址…" autocomplete="off" spellcheck="false">
  </form>
  <script>
   function go(raw) {{
     const v = (raw || '').trim();
     if (!v) return;
     const explicit = /^[a-z][a-z0-9+.-]*:\/\//i.test(v);
     const domain = !/\s/.test(v) && (v.includes('.') || v.startsWith('localhost') || v.includes(':'));
     location.href = explicit ? v : domain ? 'https://' + v : '{prefix}' + encodeURIComponent(v);
   }}
  </script>
  <div class="keys">
    <span><kbd>Ctrl</kbd> <kbd>L</kbd> 地址栏</span>
    <span><kbd>Ctrl</kbd> <kbd>T</kbd> 新标签</span>
    <span><kbd>Ctrl</kbd> <kbd>↑</kbd> 后退</span>
    <span><kbd>Ctrl</kbd> <kbd>R</kbd> 刷新</span>
    <span><kbd>?</kbd> 帮助</span>
  </div>
</div>"#,
        prefix = prefix
    )
}

/// Copy the engine out of BlinkTerm (or a given path) so ymw keeps working
/// even if BlinkTerm is removed later.
pub fn install_engine(explicit: Option<&Path>) -> Result<PathBuf> {
    let source = explicit
        .map(|p| p.to_path_buf())
        .or_else(chrome::from_blinkterm_public);
    let Some(src) = source else {
        return Err(anyhow!(
            "没有可复制的引擎 / no local engine to copy\n\
             指定一个：ymw --install-engine --engine /path/to/chrome-headless-shell"
        ));
    };
    let dir = src
        .parent()
        .ok_or_else(|| anyhow!("engine has no parent directory"))?;
    let version = version_of(&src).unwrap_or_else(|| "unknown".to_string());
    let dest_root = chrome::home().join(".local/share/ymw/engine").join(&version);
    let dest = dest_root.join(dir.file_name().unwrap_or_default());
    let bin = dest.join("chrome-headless-shell");
    if bin.is_file() {
        return Ok(bin);
    }
    std::fs::create_dir_all(&dest_root)?;
    copy_dir(dir, &dest)?;
    if !bin.is_file() {
        return Err(anyhow!("copy finished but {} is missing", bin.display()));
    }
    Ok(bin)
}

fn version_of(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.split_whitespace().last().map(|s| s.to_string())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Opt-in tracing: `YMW_DEBUG=1 ymw ...` appends to /tmp/ymw_debug.log.
fn debug(msg: &str) {
    if std::env::var_os("YMW_DEBUG").is_none() {
        return;
    }
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/ymw_debug.log")
    {
        let _ = writeln!(f, "{msg}");
    }
}
