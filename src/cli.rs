//! Command line: one table of options, rendered in the chosen language.
//!
//! `-h` prints one line per option, `--help` prints the full text; `-v` prints
//! the version and `--version` the build details. Both pairs come from the same
//! table, and clap's own help is switched off so the layout, wording and
//! language are ours.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::i18n::{self, t, Lang};

pub const NAME: &str = "ymw";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GraphicsArg {
    /// Probe the terminal and pick the best backend
    Auto,
    /// Force the Kitty graphics protocol
    Kitty,
    /// Force the iTerm2 inline image protocol
    Iterm,
    /// Half-block characters (blurry, last resort)
    Blocks,
    /// Do not draw the page at all
    Off,
}

#[derive(Debug, Parser)]
#[command(name = "ymw", disable_help_flag = true, disable_version_flag = true)]
pub struct Args {
    #[arg(default_value = "")]
    pub target: String,

    #[arg(long)]
    pub home: Option<String>,

    #[arg(long, value_name = "URL")]
    pub search: Option<String>,

    #[arg(long, value_name = "PATH")]
    pub engine: Option<PathBuf>,

    #[arg(long, value_name = "DIR")]
    pub profile: Option<PathBuf>,

    #[arg(long, value_name = "TEXT")]
    pub user_agent: Option<String>,

    #[arg(long, value_name = "FLAG")]
    pub engine_arg: Vec<String>,

    #[arg(long)]
    pub no_sandbox: bool,

    #[arg(long, default_value_t = 1.0)]
    pub zoom: f64,

    #[arg(long, default_value = "auto", value_name = "N|auto")]
    pub scale: String,

    #[arg(long, default_value = "auto", value_name = "MODE")]
    pub frames: String,

    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
    pub fps: u32,

    #[arg(short, long, value_enum, default_value = "auto")]
    pub graphics: GraphicsArg,

    #[arg(long, value_name = "RATIO")]
    pub cell_aspect: Option<f32>,

    #[arg(long)]
    pub install_engine: bool,

    #[arg(long)]
    pub probe: bool,


    #[arg(long, hide = true)]
    pub render: bool,

    #[arg(long, hide = true)]
    pub selftest: bool,

    #[arg(long, hide = true)]
    pub dump_start: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            target: String::new(),
            home: None,
            search: None,
            engine: None,
            profile: None,
            user_agent: None,
            engine_arg: Vec::new(),
            no_sandbox: false,
            frames: "auto".into(),
            scale: "auto".into(),
            zoom: 1.0,
            fps: 30,
            install_engine: false,
            graphics: GraphicsArg::Auto,
            cell_aspect: None,
            probe: false,
            render: false,
            selftest: false,
            dump_start: false,
        }
    }
}

struct Item {
    group: u8,
    flags: &'static str,
    flags_cn: Option<&'static str>,
    cn: (&'static str, &'static str),
    en: (&'static str, &'static str),
}

const GROUPS: [(&str, &str); 5] = [
    ("目标", "TARGET"),
    ("引擎", "ENGINE"),
    ("画面", "DISPLAY"),
    ("诊断", "DIAGNOSTICS"),
    ("其它", "MISC"),
];

const ITEMS: &[Item] = &[
    Item {
        group: 0,
        flags: "[target]",
        flags_cn: Some("[网址或搜索词]"),
        cn: (
            "要打开的网址或搜索词（默认起始页）",
            "看起来像网址就直接打开，否则当成搜索词拼进搜索模板。\n\
             不填就打开内置起始页。",
        ),
        en: (
            "URL or search words (default: start page)",
            "Looks like a URL opens as-is, anything else is quoted into the search\n\
             template. Empty opens the built-in start page.",
        ),
    },
    Item {
        group: 0,
        flags: "    --home <URL>",
        flags_cn: None,
        cn: (
            "起始页地址",
            "替换内置起始页；不影响直接给网址的情况。",
        ),
        en: (
            "start page URL",
            "Replaces the built-in start page; does not affect an explicit URL.",
        ),
    },
    Item {
        group: 0,
        flags: "    --search <URL>",
        flags_cn: None,
        cn: (
            "搜索地址模板，%s 占位",
            "比如 https://www.bing.com/search?q=%s，默认是百度。",
        ),
        en: (
            "search URL template, %s is replaced",
            "e.g. https://www.bing.com/search?q=%s. Default is Baidu.",
        ),
    },
    Item {
        group: 1,
        flags: "    --engine <PATH>",
        flags_cn: None,
        cn: (
            "指定 Chromium 可执行文件",
            "默认按顺序找 ymw 自己的引擎目录、chrome-headless-shell、Chrome、\n\
             Chromium、Edge。找不到时会提示怎么装。",
        ),
        en: (
            "Chromium binary to drive",
            "By default ymw looks in its own engine folder, then for\n\
             chrome-headless-shell, Chrome, Chromium and Edge. Missing engines get\n\
             an install hint.",
        ),
    },
    Item {
        group: 1,
        flags: "    --profile <DIR>",
        flags_cn: None,
        cn: (
            "用户数据目录",
            "登录状态、Cookie 都在这里；换个目录就是一套干净的身份。\n\
             默认在 ~/.local/share/ymw/profile。",
        ),
        en: (
            "profile directory",
            "Cookies and logins live here; pointing it elsewhere gives you a clean\n\
             identity. Default: ~/.local/share/ymw/profile",
        ),
    },
    Item {
        group: 1,
        flags: "    --user-agent <TEXT>",
        flags_cn: None,
        cn: (
            "覆盖 User-Agent",
            "有些站点按 UA 发不同的页面，必要时改这里。",
        ),
        en: (
            "override the User-Agent",
            "Some sites serve a different page per UA; this is the knob for that.",
        ),
    },
    Item {
        group: 1,
        flags: "    --engine-arg <FLAG>",
        flags_cn: None,
        cn: (
            "给浏览器追加一个参数",
            "可以重复给多个，原样传给 Chromium 命令行。",
        ),
        en: (
            "pass one more flag to the browser",
            "Repeatable; passed straight through to the Chromium command line.",
        ),
    },
    Item {
        group: 1,
        flags: "    --no-sandbox",
        flags_cn: None,
        cn: (
            "关闭 Chromium 沙盒",
            "只在受限环境（容器、某些 CI）里需要；正常桌面不要加。",
        ),
        en: (
            "launch Chromium without its sandbox",
            "Only needed in restricted environments (containers, some CI); leave it\n\
             off on a normal desktop.",
        ),
    },
    Item {
        group: 1,
        flags: "    --install-engine",
        flags_cn: None,
        cn: (
            "把引擎复制到 ymw 自己的目录",
            "拷到 ~/.local/share/ymw/engine，之后删掉原来的浏览器也不影响 ymw。\n\
             参数给的是哪个引擎就拷哪个。",
        ),
        en: (
            "copy the engine into ymw's own folder",
            "Copies it to ~/.local/share/ymw/engine so deleting the original browser\n\
             later does not break ymw. Whatever --engine names is what gets copied.",
        ),
    },
    Item {
        group: 2,
        flags: "    --zoom <N>",
        flags_cn: None,
        cn: (
            "初始缩放比例（默认 1.0）",
            "0.5 到 3.0。运行时用 Ctrl+= / Ctrl+- 调整。\n\
             默认：1.0",
        ),
        en: (
            "initial page zoom (default 1.0)",
            "0.5 to 3.0. Adjust while browsing with Ctrl+= / Ctrl+-.\n\
             Default: 1.0",
        ),
    },
    Item {
        group: 2,
        flags: "    --scale <N|auto>",
        flags_cn: None,
        cn: (
            "物理像素与 CSS 像素比（默认 auto）",
            "auto 在单元格高 28px 以上时取 2，否则取 1。\n\
             字太小或太糊时手动指定 1 / 2 / 3。\n\
             默认：auto",
        ),
        en: (
            "device pixels per CSS pixel (default auto)",
            "auto uses 2 when a cell is 28 px or taller, otherwise 1. Set 1 / 2 / 3\n\
             by hand if text looks too small or too soft.\n\
             Default: auto",
        ),
    },
    Item {
        group: 2,
        flags: "    --frames <MODE>",
        flags_cn: None,
        cn: (
            "帧传输方式 raw / png / auto",
            "raw  像素写进共享内存，最快，需要 Kitty 图形协议\n\
             png  发编码好的图片，兼容性好一些\n\
             auto 能 raw 就 raw\n\
             默认：auto",
        ),
        en: (
            "how frames reach the terminal: raw / png / auto",
            "raw   write pixels into shared memory, fastest, needs kitty graphics\n\
             png   send encoded images, more portable\n\
             auto  raw when possible\n\
             Default: auto",
        ),
    },
    Item {
        group: 2,
        flags: "    --fps <N>",
        flags_cn: None,
        cn: (
            "连续渲染的帧率上限，1-60（默认 30）",
            "视频和动画会连续重绘；这个值决定最多多久重绘一次。\n\
             默认：30",
        ),
        en: (
            "screencast frame rate cap, 1-60 (default 30)",
            "Video and animation redraw continuously; this caps how often.\n\
             Default: 30",
        ),
    },
    Item {
        group: 2,
        flags: "-g, --graphics <BACKEND>",
        flags_cn: None,
        cn: (
            "画面渲染方式 auto / kitty / iterm / blocks / off",
            "auto    先探测 Kitty，再试 iTerm2，都不行用半块字符\n\
             kitty   强制 Kitty 图形协议\n\
             iterm   强制 iTerm2 内联图片协议\n\
             blocks  半块字符，很糊，仅兜底\n\
             off     完全不画页面\n\
             默认：auto",
        ),
        en: (
            "page backend: auto / kitty / iterm / blocks / off",
            "auto    probe kitty first, then iTerm2, else half blocks\n\
             kitty   force the kitty graphics protocol\n\
             iterm   force the iTerm2 inline image protocol\n\
             blocks  half-block characters, blurry, last resort\n\
             off     no page at all\n\
             Default: auto",
        ),
    },
    Item {
        group: 2,
        flags: "    --cell-aspect <RATIO>",
        flags_cn: None,
        cn: (
            "单元格高宽比，探测失败时用",
            "一般不用管；页面明显被拉长或压扁时再调。",
        ),
        en: (
            "cell height/width ratio when detection fails",
            "Rarely needed; touch it only if the page looks stretched.",
        ),
    },
    Item {
        group: 3,
        flags: "    --probe",
        flags_cn: None,
        cn: (
            "打印终端与引擎探测结果后退出",
            "终端能力、选中了哪个 Chromium、版本目录、profile 路径。\n\
             排查「打不开网页」先跑这个。",
        ),
        en: (
            "print terminal and engine capabilities and exit",
            "Terminal support, which Chromium was picked, its version folder and the\n\
             profile path. First thing to run when pages do not open.",
        ),
    },
    Item {
        group: 4,
        flags: "    --lang <LANG>",
        flags_cn: None,
                cn: (
            "界面语言；单独使用只写配置、不启动界面",
            "影响 --help、--version、界面提示和报错。\n\
             单独执行 --lang en 只写入配置并退出，不启动界面。\n\
             配置保存在 ~/.config/ymw/config（$XDG_CONFIG_HOME 优先）。\n\
             以下写法等价：--lang en、--lang=en、--language en。\n\
             值不合法会直接报错。\n\
             默认：cn",
        ),
        en: (
            "UI language; alone it only saves the preference",
            "Applies to --help, --version, the interface and error messages.\n\
             On its own, --lang en only saves the preference and exits.\n\
             The file is ~/.config/ymw/config (XDG_CONFIG_HOME wins).\n\
             Equivalent: --lang en, --lang=en, --language en.\n\
             An unknown value is an error.\n\
             Default: cn",
        ),
    },
    Item {
        group: 4,
        flags: "-h",
        flags_cn: None,
        cn: (
            "这份简洁说明",
            "每个功能一句话，适合查参数名。\n要看取值、默认值和示例用 --help。",
        ),
        en: (
            "this summary",
            "One line per option: good for remembering a flag name.\n\
             Values, defaults and examples live in --help.",
        ),
    },
    Item {
        group: 4,
        flags: "    --help",
        flags_cn: None,
        cn: ("完整说明：取值、默认值、示例、排查", "即本页内容。"),
        en: ("full text: values, defaults, examples", "What you are reading."),
    },
    Item {
        group: 4,
        flags: "-v, --version",
        flags_cn: None,
        cn: (
            "版本（--version 给完整信息）",
            "-v 打一行版本号（`-V` 同义）。\n\
             --version 打完整信息：构建目标、引擎查找顺序、终端要求。",
        ),
        en: (
            "version (--version prints the full block)",
            "-v prints one line (`-V` works too).\n\
             --version prints build target, engine lookup order and terminal\n\
             requirements.",
        ),
    },
];

fn tagline() -> &'static str {
    t(
        "终端浏览器：headless Chromium 渲染，画面写入终端",
        "a terminal browser rendered by headless Chromium",
    )
}
/// Parse the command line.
///
/// The language flag is resolved first and then removed from the argument list,
/// so clap never sees it. Leaving it in is what let `--lang en` be read as
/// "`--lang` plus the positional `en`", which opened `./en` instead of switching
/// language.
pub fn parse() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (rest, wanted) = take_lang(&argv);

    // Explicit flag beats the saved preference, which beats Chinese.
    let lang = wanted
        .or_else(crate::config::read_lang)
        .unwrap_or(Lang::Cn);
    i18n::set(lang);

    // `-h` and `--version` use the language but must not change it.
    if let Some(code) = exits(&rest) {
        std::process::exit(code);
    }

    if let Some(l) = wanted {
        match crate::config::write_lang(l) {
            Ok(path) => {
                // Asking only for a language means "configure", not "start".
                if rest.is_empty() {
                    println!(
                        "{}",
                        i18n::p(
                            format!("语言已设为 {}，写入 {}", l.code(), path.display()),
                            format!("language set to {} in {}", l.code(), path.display()),
                        )
                    );
                    std::process::exit(0);
                }
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    i18n::p(
                        format!("{NAME}: 无法写入配置文件：{e}"),
                        format!("{NAME}: cannot write the config file: {e}"),
                    )
                );
                if rest.is_empty() {
                    std::process::exit(1);
                }
            }
        }
    }
    match Args::try_parse_from(std::iter::once(NAME.to_string()).chain(rest)) {
        Ok(a) => a,
        Err(e) => {
            report(e);
            std::process::exit(2);
        }
    }
}

/// Pull `--lang` / `--language` out of the arguments and settle the language.
fn take_lang(argv: &[String]) -> (Vec<String>, Option<Lang>) {
    let mut rest = Vec::with_capacity(argv.len());
    let mut lang: Option<Lang> = None;
    let mut bad: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if let Some(v) = a
            .strip_prefix("--lang=")
            .or_else(|| a.strip_prefix("--language="))
        {
            match Lang::parse(v) {
                Some(l) => lang = Some(l),
                None => bad = Some(v.to_string()),
            }
            i += 1;
        } else if a == "--lang" || a == "--language" {
            match argv.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    match Lang::parse(next) {
                        Some(l) => lang = Some(l),
                        None => bad = Some(next.clone()),
                    }
                    i += 2;
                }
                // A bare `--lang` is a mistake worth naming, not a silent cn.
                _ => {
                    bad = Some(String::new());
                    i += 1;
                }
            }
        } else {
            rest.push(a.clone());
            i += 1;
        }
    }

    if let Some(v) = bad {
        // The language is exactly what could not be settled, so say it twice.
        if v.is_empty() {
            eprintln!("{NAME}: --lang 需要一个值，取 cn 或 en / --lang needs a value: cn or en");
        } else {
            eprintln!(
                "{NAME}: 无法识别的语言 \"{v}\"，取 cn 或 en / unknown language \"{v}\", use cn or en"
            );
        }
        std::process::exit(2);
    }
    (rest, lang)
}

/// `-h`, `--help`, `-v`, `--version` and `--lang` are handled before clap so
/// the language is settled first and help can be printed in it.
fn exits(argv: &[String]) -> Option<i32> {
    for (i, a) in argv.iter().enumerate() {
        // `-hv` / `-vh`: a cluster made only of help/version letters means help.
        let cluster = a.len() > 2
            && a.starts_with('-')
            && !a.starts_with("--")
            && a[1..].chars().all(|c| matches!(c, 'h' | 'v' | 'V'));
        match a.as_str() {
            "-h" => {
                short_help();
                return Some(0);
            }
            "--help" => {
                long_help();
                return Some(0);
            }
            "-v" | "-V" => {
                // `-v 80` used to mean volume; say why it printed a version
                // instead of silently swallowing the number.
                if let Some(next) = argv.get(i + 1) {
                    if !next.is_empty() && next.chars().all(|c| c.is_ascii_digit()) {
                        eprintln!(
                            "{}",
                            t(
                                "ymw 提示：音量已改为 --volume，-v 现在只打印版本号",
                                "ymw: volume is --volume now; -v only prints the version"
                            )
                        );
                    }
                }
                version_line();
                return Some(0);
            }
            "--version" => {
                long_version();
                return Some(0);
            }
            _ if cluster => {
                if a.contains('h') {
                    short_help();
                } else {
                    version_line();
                }
                return Some(0);
            }
            _ => {}
        }
    }
    None
}

fn flag_of(it: &Item, en: bool) -> &'static str {
    if en {
        it.flags
    } else {
        it.flags_cn.unwrap_or(it.flags)
    }
}

fn pad(s: &str, width: usize) -> String {
    let w = i18n::width(s);
    if w >= width {
        format!("{s} ")
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

fn version_line() {
    println!("{NAME} {VERSION}");
}

fn short_help() {
    let en = i18n::is_en();
    println!("{NAME} {VERSION} · {}", tagline());
    println!();
    println!("{}", t("用法", "Usage"));
    println!(
        "  {NAME} {} [{}]",
        t("[选项]", "[options]"),
        t("网址或搜索词", "url or search words")
    );
    println!();
    println!("{}", t("选项", "Options"));
    for it in ITEMS {
        let line = if en { it.en.0 } else { it.cn.0 };
        println!("  {}{}", pad(flag_of(it, en), 30), line);
    }
    println!();
    println!(
        "{}",
        t(
            "取值、默认值和示例：ymw --help　　Ctrl+L 输入网址，? 看全部快捷键",
            "values, defaults and examples: ymw --help    Ctrl+L for a URL, ? for all keys"
        )
    );
}

fn long_help() {
    let en = i18n::is_en();
    println!("{NAME} {VERSION} · {}", tagline());
    println!();
    println!("{}", t("用法", "Usage"));
    println!(
        "  {NAME} {} [{}]",
        t("[选项]", "[options]"),
        t("网址或搜索词", "url or search words")
    );
    println!();

    let mut group = u8::MAX;
    for it in ITEMS {
        if it.group != group {
            group = it.group;
            let (cn, en_t) = GROUPS[group as usize];
            println!("{}", if en { en_t } else { cn });
        }
        println!("  {}", flag_of(it, en));
        let text = if en { it.en.1 } else { it.cn.1 };
        for line in text.lines() {
            println!("      {line}");
        }
        println!();
    }

    println!("{}", t("示例", "Examples"));
    if en {
        println!("  ymw                         open the start page");
        println!("  ymw example.com             open a URL");
        println!("  ymw 'rust kitty protocol'   search");
        println!("  ymw --search 'https://www.bing.com/search?q=%s'   another engine");
        println!("  ymw --install-engine        keep a private copy of Chromium");
        println!("  ymw --probe                 terminal + engine report");
    } else {
        println!("  ymw                         打开起始页");
        println!("  ymw example.com             直接打开网址");
        println!("  ymw 'rust kitty protocol'   搜索");
        println!("  ymw --search 'https://www.bing.com/search?q=%s'   换个搜索引擎");
        println!("  ymw --install-engine        把 Chromium 拷一份到自己目录");
        println!("  ymw --probe                 终端与引擎报告");
    }
    println!();
    println!("{}", t("依赖", "Requirements"));
    if en {
        println!("  a Chromium build ymw can drive (chrome-headless-shell, Chrome, Chromium");
        println!("  or Edge) and a terminal with the kitty graphics protocol.");
        println!("  `ymw --install-engine` copies the one it finds into its own folder.");
    } else {
        println!("  需要一份能被驱动的 Chromium（chrome-headless-shell / Chrome / Chromium /");
        println!("  Edge 任一），以及支持 Kitty 图形协议的终端。");
        println!("  `ymw --install-engine` 会把找到的那份拷进 ymw 自己的目录。");
    }
    println!();
    println!("{}", t("退出码", "Exit codes"));
    if en {
        println!("  0 ok   1 runtime error   2 bad arguments");
    } else {
        println!("  0 正常   1 运行出错   2 参数有误");
    }
}

fn long_version() {
    let target = format!(
        "{}-{}",
        std::env::consts::ARCH,
        match std::env::consts::OS {
            "macos" => "apple-darwin",
            "linux" => "unknown-linux-gnu",
            other => other,
        }
    );
    println!("{NAME} {VERSION}");
    println!("{}", tagline());
    println!();
    if i18n::is_en() {
        println!("build target   {target}");
        println!("engine         headless Chromium over a CDP pipe");
        println!("lookup order   ~/.local/share/ymw/engine, chrome-headless-shell,");
        println!("               Google Chrome, Chromium, Microsoft Edge");
        println!("terminal       kitty graphics protocol for the page");
        println!("licence        MIT");
        println!("note           AI-generated, macOS only, no issue/PR support");
    } else {
        println!("构建目标    {target}");
        println!("引擎        headless Chromium，走 CDP 管道");
        println!("查找顺序    ~/.local/share/ymw/engine、chrome-headless-shell、");
        println!("            Google Chrome、Chromium、Microsoft Edge");
        println!("终端要求    Kitty 图形协议（页面是靠画图显示的）");
        println!("许可        MIT");
        println!("说明        由 AI 生成，仅适配 macOS，不提供 issue / PR 支持");
    }
}

fn report(e: clap::Error) {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    let what = match e.kind() {
        ErrorKind::UnknownArgument => t("未知参数", "unknown argument"),
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => t("参数值不合法", "invalid value"),
        ErrorKind::MissingRequiredArgument => t("缺少必需的参数", "missing argument"),
        ErrorKind::TooManyValues => t("给了太多值", "too many values"),
        ErrorKind::NoEquals => t("这里需要等号", "expected '='"),
        _ => t("参数有误", "bad arguments"),
    };
    let arg = match e.get(ContextKind::InvalidArg) {
        Some(ContextValue::String(s)) => format!(" `{s}`"),
        _ => String::new(),
    };
    let bad = match e.get(ContextKind::InvalidValue) {
        Some(ContextValue::String(s)) => format!(" = {s}"),
        _ => String::new(),
    };
    // Listing the accepted values is the difference between "it does not work"
    // and "oh, shuffle is spelt with one f".
    let mut valid: Vec<String> = Vec::new();
    for (kind, value) in e.context() {
        if kind == ContextKind::ValidValue {
            if let ContextValue::String(s) = value {
                valid.push(s.clone());
            }
        }
    }
    if valid.is_empty() {
        // clap renders the accepted values into the message instead of the
        // context; lift them out so they can be shown in either language.
        if let Some(rest) = e.to_string().split("possible values: ").nth(1) {
            let line = rest.lines().next().unwrap_or("");
            let line = line.trim().trim_start_matches('[').trim_end_matches(']');
            valid = line
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    eprintln!("{NAME}: {what}{arg}{bad}");
    if !valid.is_empty() {
        eprintln!("       {} {}", t("可选：", "valid:"), valid.join(" / "));
    }
    eprintln!(
        "       {}",
        t(
            "用 -h 看简洁用法，--help 看完整说明",
            "run `-h` for a summary, `--help` for the full text"
        )
    );
}
