mod app;
mod cli;
mod config;
mod engine;
mod graphics;
mod i18n;
mod selftest;
mod shm;
mod term;
mod theme;
mod ui;

use anyhow::Result;
use i18n::t;

fn main() {
    let args = cli::parse();

    if args.install_engine {
        match engine::install_engine(args.engine.as_deref()) {
            Ok(path) => {
                println!("{}", t("引擎已安装", "engine installed"));
                println!("  {}", path.display());
                println!("  {}", engine::chrome::default_profile().display());
                println!(
                    "{}",
                    t(
                        "用 ymw --probe 可以看到现在选中的引擎",
                        "`ymw --probe` shows which engine is picked now"
                    )
                );
            }
            Err(e) => {
                eprintln!("ymw: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if args.dump_start {
        let search = args.search.as_deref().unwrap_or(engine::DEFAULT_SEARCH);
        println!("{}", engine::start_page(search));
        std::process::exit(0);
    }

    if args.selftest {
        if let Err(e) = selftest::run(&args) {
            eprintln!("ymw selftest: {e}");
            std::process::exit(1);
        }
        return;
    }

    if args.probe {
        probe(&args);
        return;
    }

    if args.render {
        if let Err(e) = render(&args) {
            eprintln!("ymw render: {e}");
            std::process::exit(1);
        }
        return;
    }

    let caps = term::probe();
    let kbd = caps.kbd;

    let app = match app::App::new(&args, caps) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ymw: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = term::enter(kbd) {
        eprintln!(
            "{}",
            i18n::p(
                format!("ymw: 终端初始化失败: {e}"),
                format!("ymw: cannot initialise terminal: {e}")
            )
        );
        std::process::exit(1);
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = term::leave();
        default_hook(info);
    }));

    let mut app = app;
    let result = app.run();
    let _ = term::leave();

    if let Err(e) = result {
        eprintln!("ymw: {e}");
        std::process::exit(1);
    }
}

fn probe(args: &cli::Args) {
    let caps = term::probe();
    term::print_caps(&caps);
    println!();
    println!("{}", t("引擎", "engine"));
    match engine::chrome::find(args.engine.as_deref()) {
        Some(e) => {
            println!("  binary            {}", e.path.display());
            println!("  kind              {:?}", e.kind);
            println!("  version dir       {}", e.version);
        }
        None => {
            println!(
                "  binary            {}",
                t("没找到", "NOT FOUND")
            );
            println!(
                "  {}",
                t(
                    "  处理办法：装一个 Chrome / Chromium / Edge，\n  \
                     或者 `ymw --install-engine` 把自己找到的那份拷进 ymw 目录，\n  \
                     或者 `ymw --engine /path/to/chrome-headless-shell` 直接指定。",
                    "  fix: install Chrome / Chromium / Edge, or run `ymw --install-engine`\n  \
                     to keep a private copy, or point at one directly with\n  \
                     `ymw --engine /path/to/chrome-headless-shell`."
                )
            );
        }
    }
    println!(
        "  profile           {}",
        args.profile
            .clone()
            .unwrap_or_else(engine::chrome::default_profile)
            .display()
    );
}

/// Developer aid: boot the real browser, let it settle, dump the UI as text.
fn render(args: &cli::Args) -> Result<()> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let w: u16 = std::env::var("YMW_RENDER_W")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let h: u16 = std::env::var("YMW_RENDER_H")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(34);

    let caps = term::Caps::default();
    let mut app = app::App::new(args, caps)?;

    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend)?;
    for _ in 0..80 {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        app.sync_viewport();
        if app.tabs.first().map(|t| t.frame_seq > 0).unwrap_or(false) {
            break;
        }
        let _ = app.tick();
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    if let Ok(path) = std::env::var("YMW_RENDER_SHOT") {
        if let Some(frame) = &app.frame {
            let bytes = match frame.as_ref() {
                crate::graphics::Frame::Png(b) => Some(b.clone()),
                other => other.to_image().and_then(|img| {
                    let mut buf = Vec::new();
                    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
                        .ok()
                        .map(|_| buf)
                }),
            };
            if let Some(bytes) = bytes {
                let _ = std::fs::write(&path, bytes);
            }
            println!("frame written to {path}");
        }
    }

    terminal.draw(|f| ui::draw(f, &mut app))?;
    let buf = terminal.backend().buffer();
    println!("+{}+", "-".repeat(w as usize));
    for y in 0..h {
        let mut line = String::new();
        for x in 0..w {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("|{}|", line.trim_end());
    }
    println!("+{}+", "-".repeat(w as usize));
    println!(
        "engine: {}  proto: {}  content: {:?}",
        app.info.product,
        app.graphics.proto.label(),
        app.content
    );
    Ok(())
}
