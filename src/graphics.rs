use std::collections::VecDeque;
use std::io::{self, Write};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use image::{imageops::FilterType, DynamicImage, RgbImage};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::shm::Shm;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Iterm2,
    Blocks,
    Off,
}

impl Protocol {
    pub fn label(&self) -> &'static str {
        match self {
            Protocol::Kitty => "kitty",
            Protocol::Iterm2 => "iterm2",
            Protocol::Blocks => "blocks",
            Protocol::Off => "off",
        }
    }

    pub fn can_draw_image(&self) -> bool {
        matches!(self, Protocol::Kitty | Protocol::Iterm2)
    }
}

/// Stacking order for the page picture.
///
/// Kitty paints images above text by default, which is why a dialog drawn over
/// the page used to be invisible. A negative z puts the picture under the text;
/// going below `i32::MIN / 2` additionally puts it under cells that have a
/// non-default background, so a dialog with a solid background really covers
/// the page while the rest of it keeps showing — and keeps updating.
const PAGE_Z: i32 = -1_100_000_000;

/// One rendered page. `Rgb` is what the kitty shared-memory path wants: raw
/// pixels cost the terminal no decoding at all. `Png` is the fallback for
/// terminals that only take encoded data.
#[derive(Clone)]
pub enum Frame {
    Png(Vec<u8>),
    Rgb { w: u32, h: u32, data: Vec<u8> },
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.dims();
        match self {
            Frame::Png(b) => write!(f, "Frame::Png({w}x{h}, {} bytes)", b.len()),
            Frame::Rgb { data, .. } => write!(f, "Frame::Rgb({w}x{h}, {} bytes)", data.len()),
        }
    }
}

impl Frame {
    pub fn dims(&self) -> (u32, u32) {
        match self {
            Frame::Png(bytes) => {
                if bytes.len() >= 24 && &bytes[1..4] == b"PNG" {
                    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
                    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
                    (w.max(1), h.max(1))
                } else {
                    (1, 1)
                }
            }
            Frame::Rgb { w, h, .. } => ((*w).max(1), (*h).max(1)),
        }
    }

    pub fn to_image(&self) -> Option<DynamicImage> {
        match self {
            Frame::Png(bytes) => image::load_from_memory(bytes).ok(),
            Frame::Rgb { w, h, data } => {
                let buf = RgbImage::from_raw(*w, *h, data.clone())?;
                Some(DynamicImage::ImageRgb8(buf))
            }
        }
    }
}

pub struct Graphics {
    pub proto: Protocol,
    pub cell_w: u16,
    pub cell_h: u16,
    id: u32,
    /// (frame key, area width, area height) of the placement on screen
    last: Option<(u64, u16, u16)>,
    placed: Option<(u16, u16, u16, u16)>,
    /// shared memory objects handed to the terminal, kept alive briefly
    shm: VecDeque<Shm>,
    pub visible: bool,
}

impl Graphics {
    pub fn new(proto: Protocol, cell_w: u16, cell_h: u16) -> Self {
        Self {
            proto,
            cell_w: cell_w.max(1),
            cell_h: cell_h.max(1),
            id: 1,
            last: None,
            placed: None,
            shm: VecDeque::new(),
            visible: true,
        }
    }

    pub fn is_graphics(&self) -> bool {
        self.proto.can_draw_image()
    }

    pub fn invalidate(&mut self) {
        self.last = None;
        self.placed = None;
    }

    pub fn delete<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.proto == Protocol::Kitty {
            write!(out, "\x1b_Ga=d,d=i,i={}\x1b\\", self.id)?;
        }
        self.last = None;
        self.placed = None;
        Ok(())
    }

    pub fn clear_all<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        if self.proto == Protocol::Kitty {
            write!(out, "\x1b_Ga=d,d=A\x1b\\")?;
        }
        self.last = None;
        self.placed = None;
        self.shm.clear();
        Ok(())
    }

    /// Fit the picture into the pane without ever changing its proportions.
    ///
    /// The old frame stays on screen for one frame after a resize, and if it
    /// were stretched to the new cell grid the page would visibly snap from
    /// wrong to right. Letterboxing it keeps every frame honest.
    fn place(&self, area: Rect, w: u32, h: u32) -> (u16, u16, u16, u16) {
        let cw = self.cell_w as f32;
        let ch = self.cell_h as f32;
        let avail_w = area.width as f32 * cw;
        let avail_h = area.height as f32 * ch;
        let scale = (avail_w / w as f32).min(avail_h / h as f32);
        let cols = ((w as f32 * scale / cw).round() as u16).clamp(1, area.width.max(1));
        let rows = ((h as f32 * scale / ch).round() as u16).clamp(1, area.height.max(1));
        let x = area.x + area.width.saturating_sub(cols) / 2;
        let y = area.y + area.height.saturating_sub(rows) / 2;
        (x, y, cols, rows)
    }

    pub fn present<W: Write>(
        &mut self,
        out: &mut W,
        area: Rect,
        key: u64,
        frame: &Frame,
    ) -> io::Result<()> {
        if !self.is_graphics() || !self.visible || area.width < 2 || area.height < 2 {
            if self.last.is_some() {
                let _ = self.delete(out);
            }
            return Ok(());
        }
        if self.last == Some((key, area.width, area.height)) {
            return Ok(());
        }

        let (w, h) = frame.dims();
        let (x, y, cols, rows) = self.place(area, w, h);
        if self.placed != Some((x, y, cols, rows)) {
            // Re-placing the same image id moves the picture in place; there is
            // no need to delete it first, and deleting is what caused the flash
            // on every resize.
            self.placed = Some((x, y, cols, rows));
        }

        write!(out, "\x1b[{};{}H", y + 1, x + 1)?;
        match (self.proto, frame) {
            (Protocol::Kitty, Frame::Rgb { w, h, data }) => {
                match Shm::new("ymw", data.len()) {
                    Ok(mut shm) => {
                        shm.write(data);
                        let name = STANDARD.encode(shm.name().as_bytes());
                        write!(
                            out,
                            "\x1b_Ga=T,f=24,s={},v={},i={},p=1,c={},r={},C=1,q=2,z={},t=s,S={};{}\x1b\\",
                            w,
                            h,
                            self.id,
                            cols,
                            rows,
                            PAGE_Z,
                            data.len(),
                            name
                        )?;
                        // keep a couple of objects around: the terminal may still
                        // be reading the previous frame
                        self.shm.push_back(shm);
                        while self.shm.len() > 3 {
                            self.shm.pop_front();
                        }
                    }
                    Err(_) => {
                        // No shared memory here: fall back to an encoded frame.
                        if let Some(img) = frame.to_image() {
                            let mut png = Vec::new();
                            if img.write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png).is_ok() {
                                self.send_png(out, cols, rows, &png)?;
                            }
                        }
                    }
                }
            }
            (Protocol::Kitty, Frame::Png(png)) => self.send_png(out, cols, rows, png)?,
            (Protocol::Iterm2, frame) => {
                let png = match frame {
                    Frame::Png(b) => Some(b.clone()),
                    Frame::Rgb { .. } => frame.to_image().and_then(|img| {
                        let mut buf = Vec::new();
                        img.write_to(&mut io::Cursor::new(&mut buf), image::ImageFormat::Png)
                            .ok()
                            .map(|_| buf)
                    }),
                };
                if let Some(png) = png {
                    let b64 = STANDARD.encode(png);
                    write!(
                        out,
                        "\x1b]1337;File=inline=1;width={};height={};preserveAspectRatio=1;doNotMoveCursor=1:{}\x07",
                        cols, rows, b64
                    )?;
                }
            }
            _ => {}
        }
        out.flush()?;
        self.last = Some((key, area.width, area.height));
        Ok(())
    }

    fn send_png<W: Write>(&mut self, out: &mut W, cols: u16, rows: u16, png: &[u8]) -> io::Result<()> {
        let b64 = STANDARD.encode(png);
        let mut chunks = b64.as_bytes().chunks(4096).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = if chunks.peek().is_some() { 1 } else { 0 };
            if first {
                write!(
                    out,
                    "\x1b_Ga=T,f=100,t=d,i={},p=1,C=1,c={},r={},q=2,z={},m={};",
                    self.id, cols, rows, PAGE_Z, more
                )?;
                first = false;
            } else {
                write!(out, "\x1b_Gm={};", more)?;
            }
            out.write_all(chunk)?;
            write!(out, "\x1b\\")?;
        }
        Ok(())
    }

    /// Half block rendering: two vertical pixels per cell.
    pub fn blocks(&self, img: &DynamicImage, cols: u16, rows: u16, dim: bool) -> Vec<Line<'static>> {
        if cols == 0 || rows == 0 {
            return Vec::new();
        }
        let px_w = cols as u32;
        let px_h = rows as u32 * 2;
        let small = img.resize_exact(px_w, px_h, FilterType::Triangle).to_rgba8();
        let mut lines = Vec::with_capacity(rows as usize);
        for r in 0..rows as u32 {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(cols as usize);
            for c in 0..cols as u32 {
                let top = small.get_pixel(c, r * 2);
                let bot = small.get_pixel(c, (r * 2 + 1).min(px_h - 1));
                let mut fg = Color::Rgb(top[0], top[1], top[2]);
                let mut bg = Color::Rgb(bot[0], bot[1], bot[2]);
                if dim {
                    fg = scale(fg, 0.55);
                    bg = scale(bg, 0.55);
                }
                spans.push(Span::styled("▀", Style::default().fg(fg).bg(bg)));
            }
            lines.push(Line::from(spans));
        }
        lines
    }
}

fn scale(c: Color, f: f32) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * f) as u8,
            (g as f32 * f) as u8,
            (b as f32 * f) as u8,
        ),
        other => other,
    }
}

/// Neutral placeholder shown before the first frame arrives.
pub fn placeholder(cols: u16, rows: u16, accent: Color, dim: Color) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let inner_w = cols.saturating_sub(2) as usize;
    for r in 0..rows {
        let mut s = String::new();
        if r == 0 || r + 1 == rows {
            s.push(' ');
            s.push_str(&"·".repeat(inner_w.max(1)));
            s.push(' ');
        } else if r == rows / 2 && rows > 4 {
            let label = " loading ";
            let pad = inner_w.saturating_sub(label.chars().count()) / 2;
            s.push(' ');
            s.push_str(&" ".repeat(pad));
            s.push_str(label);
            s.push_str(&" ".repeat(inner_w.saturating_sub(pad + label.chars().count())));
            s.push(' ');
        } else {
            s.push(' ');
            s.push_str(&" ".repeat(inner_w.max(1)));
            s.push(' ');
        }
        let style = if r == rows / 2 && rows > 4 {
            Style::default().fg(accent)
        } else {
            Style::default().fg(dim)
        };
        lines.push(Line::from(Span::styled(s, style)));
    }
    lines
}
