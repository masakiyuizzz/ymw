use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::i18n;
use crate::app::{App, Focus};
use crate::graphics::placeholder;

fn w(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn fit(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if w(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

fn pretty_url(url: &str) -> String {
    let s = url.trim();
    if let Some(rest) = s.strip_prefix("https://") {
        return rest.trim_end_matches('/').to_string();
    }
    if let Some(rest) = s.strip_prefix("http://") {
        return rest.trim_end_matches('/').to_string();
    }
    if s == "about:blank" || s.is_empty() {
        return String::new();
    }
    s.to_string()
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let t = app.theme.clone();
    f.render_widget(
        Block::default().style(Style::default().fg(Color::Reset).bg(Color::Reset)),
        area,
    );
    app.content = None;
    app.url_rect = None;
    app.tab_hits.clear();

    if area.width < 30 || area.height < 8 {
        f.render_widget(
            Paragraph::new("terminal too small")
                .style(Style::default().fg(t.muted))
                .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let frame = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border));
    let inner = frame.inner(area);
    f.render_widget(frame, area);

    // Drawn as a paragraph over the blanked top rule: wide characters need a
    // clean row underneath, otherwise the second half of each glyph keeps the
    // border character that was there before.
    let top = Rect {
        x: area.x + 1,
        y: area.y,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Clear, top);
    f.render_widget(Paragraph::new(top_line(app, inner.width as usize)), top);

    if inner.width < 20 || inner.height < 4 {
        return;
    }

    let content = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };

    let multi = app.tabs.len() > 1;
    let mut constraints = Vec::new();
    if multi {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1)); // url bar
    constraints.push(Constraint::Min(2)); // page
    constraints.push(Constraint::Length(1)); // status
    let rows = Layout::vertical(constraints).split(content);

    let mut i = 0;
    if multi {
        draw_tabs(f, app, rows[0]);
        i = 1;
    }
    app.url_rect = Some(rows[i]);
    draw_urlbar(f, app, rows[i]);
    let page = rows[i + 1];
    draw_page(f, app, page);
    draw_status(f, app, rows[i + 2]);

    if app.help {
        draw_help(f, app, area);
    }
}

fn top_line(app: &App, inner_w: usize) -> Line<'static> {
    let t = &app.theme;

    let title = app
        .tab()
        .map(|x| {
            if x.url.starts_with("about:") || x.url.is_empty() {
                i18n::t("新标签页", "new tab").to_string()
            } else if x.title.is_empty() || x.title == "ymw" {
                pretty_url(&x.url)
            } else {
                x.title.clone()
            }
        })
        .unwrap_or_default();

    let mut right = String::new();
    if app.loading > 0 {
        right.push_str(i18n::t("加载中 · ", "loading · "));
    }
    right.push_str(&format!("{} tab", app.tabs.len()));
    if app.tabs.len() > 1 {
        right.push('s');
    }
    let zoom = app.tab().map(|x| x.zoom).unwrap_or(1.0);
    right.push_str(&format!(" · {}%", (zoom * 100.0).round() as i32));

    let border = Style::default().fg(t.border);
    let brand = Span::styled(
        " ymw ",
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    );

    let room = inner_w.saturating_sub(1 + 5 + 2 + 2 + w(&right));
    let title = fit(&title, room);
    // "─" + " ymw " + " title " + fill + " right " must be exactly inner_w
    let used = 1 + 5 + 2 + w(&title) + 2 + w(&right);
    let fill = inner_w.saturating_sub(used);

    Line::from(vec![
        Span::styled("─", border),
        brand,
        Span::styled(format!(" {} ", title), Style::default().fg(t.muted)),
        Span::styled("─".repeat(fill), border),
        Span::styled(format!(" {} ", right), Style::default().fg(t.dim)),
    ])
}

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;
    for (i, tab) in app.tabs.iter().enumerate() {
        let active = i == app.active;
        let label = format!(" {} {} ", i + 1, fit(&tab.label(), 24));
        let width = w(&label) as u16;
        if x + width > area.x + area.width {
            break;
        }
        let style = if active {
            Style::default()
                .fg(Color::Rgb(0x16, 0x18, 0x1e))
                .bg(t.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(t.dim)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
        app.tab_hits.push((area.y, x, x + width + 1, i));
        x += width + 1;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_urlbar(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let focused = app.focus == Focus::Url;
    let mut spans: Vec<Span> = Vec::new();

    if app.focus == Focus::Find {
        spans.push(Span::styled(
            " ⌕ ",
            Style::default().fg(t.accent2).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            app.find_input.clone(),
            Style::default().fg(t.text),
        ));
        spans.push(Span::styled("█", Style::default().fg(t.accent2)));
        spans.push(Span::styled(
            i18n::t("   enter 查找 · esc 取消", "   enter to find · esc to cancel"),
            Style::default().fg(t.dim),
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let arrow = if focused { " ▸ " } else { "   " };
    spans.push(Span::styled(
        arrow,
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    ));

    if focused {
        let text = if app.url_select_all {
            Span::styled(
                app.url_input.clone(),
                Style::default().fg(Color::Rgb(0x16, 0x18, 0x1e)).bg(t.accent),
            )
        } else {
            Span::styled(app.url_input.clone(), Style::default().fg(t.text))
        };
        spans.push(text);
        spans.push(Span::styled("█", Style::default().fg(t.accent)));
        let used: usize = spans.iter().map(|s| w(&s.content)).sum();
        if used + 24 < area.width as usize {
            spans.push(Span::styled(
                i18n::t("   enter 打开 · esc 取消", "   enter to open · esc to cancel"),
                Style::default().fg(t.dim),
            ));
        }
    } else {
        let url = app.tab().map(|x| x.url.clone()).unwrap_or_default();
        let shown = if url == "about:blank" || url.is_empty() {
            if app.tab().map(|x| x.frame_seq > 0).unwrap_or(false) {
                i18n::t("新标签页", "new tab").to_string()
            } else {
                "about:blank".to_string()
            }
        } else {
            pretty_url(&url)
        };
        spans.push(Span::styled(
            fit(&shown, area.width.saturating_sub(4) as usize),
            Style::default().fg(t.muted),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_page(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    if area.width < 4 || area.height < 2 {
        return;
    }
    app.content = Some(area);

    if app.graphics.is_graphics() {
        // the image is painted over these cells by the caller after draw()
        return;
    }

    match (&app.frame_image, app.graphics.proto) {
        (Some(img), crate::graphics::Protocol::Blocks) => {
            let lines = app
                .graphics
                .blocks(img, area.width, area.height, false);
            f.render_widget(Paragraph::new(lines), area);
        }
        _ => {
            let lines = placeholder(area.width, area.height, t.accent, t.dim);
            f.render_widget(Paragraph::new(lines), area);
        }
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let mut spans: Vec<Span> = Vec::new();

    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    if app.loading > 0 {
        let s = spinner[(app.tick as usize) % spinner.len()];
        spans.push(Span::styled(
            format!(" {s} "),
            Style::default().fg(t.accent),
        ));
    } else {
        spans.push(Span::styled(" ● ", Style::default().fg(t.accent2)));
    }

    if let Some((msg, _)) = &app.toast {
        spans.push(Span::styled(msg.clone(), Style::default().fg(t.accent2)));
    } else if let Some(err) = &app.error {
        spans.push(Span::styled(
            fit(err, area.width.saturating_sub(30) as usize),
            Style::default().fg(Color::Red),
        ));
    } else if app.tab().map(|x| x.editable).unwrap_or(false) {
        spans.push(Span::styled(i18n::t("页面输入中", "typing into the page"), Style::default().fg(t.accent)));
        spans.push(Span::styled(
            i18n::t("  ·  直接打字 · esc 退出输入", "  ·  type away · esc to leave"),
            Style::default().fg(t.dim),
        ));
    } else {
        spans.push(Span::styled(
            if app.focus == Focus::Url {
                i18n::t("编辑地址", "editing the address")
            } else {
                i18n::t("就绪", "ready")
            },
            Style::default().fg(t.muted),
        ));
        spans.push(Span::styled(
            i18n::t(
        "  ·  滚轮/↑↓ 滚动 · 点击可交互 · ? 帮助",
        "  ·  wheel/↑↓ scrolls · clicks interact · ? help",
    ),
            Style::default().fg(t.dim),
        ));
    }

    let right = format!("{}  {}  ", app.graphics.proto.label(), clamp_engine(&app.info.version));
    let used: usize = spans.iter().map(|s| w(&s.content)).sum();
    let gap = (area.width as usize).saturating_sub(used + w(&right));
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.push(Span::styled(right, Style::default().fg(t.faint)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn clamp_engine(v: &str) -> String {
    if v.len() > 18 {
        format!("{}…", &v[..18])
    } else {
        v.to_string()
    }
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let keys: &[(&str, &str)] = if i18n::is_en() {
        &[
            ("Ctrl+L", "focus the address bar"),
            ("Ctrl+T", "new tab"),
            ("Ctrl+W", "close tab"),
            ("Ctrl+1~9", "switch to tab N"),
            ("Ctrl+←/→", "previous / next tab"),
            ("Ctrl+↑/↓", "back / forward"),
            ("Ctrl+R", "reload"),
            ("Ctrl+F", "find in page"),
            ("Ctrl+A", "select all"),
            ("Ctrl+Shift+C", "copy the selection"),
            ("Ctrl+Shift+V", "paste into the page"),
            ("Ctrl+=/-", "zoom in / out"),
            ("Ctrl+0", "reset zoom"),
            ("Esc", "cancel / leave typing"),
            ("↑↓ PgUp PgDn", "scroll the page"),
            ("拖拽鼠标", "select text"),
            ("?", "this help (not while typing in a page)"),
            ("Ctrl+Q", "quit"),
        ]
    } else {
        &[
            ("Ctrl+L", "聚焦地址栏"),
            ("Ctrl+T", "新建标签页"),
            ("Ctrl+W", "关闭标签页"),
            ("Ctrl+1~9", "切换到第 N 个标签"),
            ("Ctrl+←/→", "上一个 / 下一个标签"),
            ("Ctrl+↑/↓", "后退 / 前进"),
            ("Ctrl+R", "刷新页面"),
            ("Ctrl+F", "页内查找"),
            ("Ctrl+A", "全选页面内容"),
            ("Ctrl+Shift+C", "复制选中文字"),
            ("Ctrl+Shift+V", "粘贴到页面"),
            ("Ctrl+=/-", "放大 / 缩小"),
            ("Ctrl+0", "重置缩放"),
            ("Esc", "取消 / 退出输入"),
            ("↑↓ PgUp PgDn", "滚动页面"),
            ("拖拽鼠标", "选中文字"),
            ("?", "本帮助（页面输入中不触发，先 Esc）"),
            ("Ctrl+Q", "退出"),
        ]
    };

    let wdt = 60.min(area.width.saturating_sub(4));
    let hgt = ((keys.len() as u16) + 6).min(area.height.saturating_sub(4));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(wdt)) / 2,
        y: area.y + (area.height.saturating_sub(hgt)) / 2,
        width: wdt,
        height: hgt,
    };
    f.render_widget(Clear, rect);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    for (k, d) in keys {
        let pad = 14usize.saturating_sub(w(k));
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {k}{}", " ".repeat(pad)),
                Style::default().fg(t.accent),
            ),
            Span::styled(*d, Style::default().fg(t.muted)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  render: {}   cell: {}x{}px",
            app.graphics.proto.label(),
            app.caps.cell_w,
            app.caps.cell_h
        ),
        Style::default().fg(t.dim),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.accent))
        .title(Span::styled(" ymw · keys ", Style::default().fg(t.accent)))
        .style(Style::default().bg(t.sel_bg));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}
