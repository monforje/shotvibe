//! Lightshot-style fullscreen overlay: a frozen screenshot to select an area
//! on, annotate, and then copy/save it — or record that area as video.

use crate::capture::{self, Notifier};
use crate::clipboard;
use crate::draw::{self, P, PALETTE, Shape, ShapeKind, TEXT_FONT, Tool, WIDTHS};
use crate::icons::{Assets, Icon};
use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, Application, Bounds, CursorStyle, Div, ElementId, FocusHandle,
    FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, PathBuilder, PathStyle, ScrollDelta, ScrollWheelEvent, SharedString, Stateful,
    StrokeOptions, Task, Timer, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions, canvas, div, img, point, prelude::*, px, rgb,
    rgba, size, svg,
};
use image::RgbaImage;
use lyon::tessellation::{LineCap, LineJoin};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const UI_FONT: &str = "Cantarell";
const ACCENT: u32 = 0x0a84ffff;
const RECORD: u32 = 0xff3b30ff;
const PANEL_BG: u32 = 0x1c1c22f2;
const BTN: f32 = 34.;
const TOOLS_W: f32 = BTN + 10.;
const TOOLS_H: f32 = 11. * (BTN + 2.) + 2. * 9. + 8.;
const ACTIONS_W: f32 = 236.;
const ACTIONS_H: f32 = BTN + 10.;
const MIN_SEL: f32 = 4.;

fn c(v: u32) -> Hsla {
    rgba(v).into()
}

fn pt(p: gpui::Point<gpui::Pixels>) -> P {
    (f32::from(p.x), f32::from(p.y))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Photo,
    Video,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct R {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl R {
    fn from_points(a: P, b: P) -> Self {
        let (x, y, w, h) = draw::rect_from(a, b);
        R { x, y, w, h }
    }
    fn right(&self) -> f32 {
        self.x + self.w
    }
    fn bottom(&self) -> f32 {
        self.y + self.h
    }
    fn contains(&self, p: P) -> bool {
        p.0 >= self.x && p.0 <= self.right() && p.1 >= self.y && p.1 <= self.bottom()
    }
    fn clamp_to(self, (sw, sh): P) -> Self {
        let x = self.x.clamp(0., sw);
        let y = self.y.clamp(0., sh);
        R {
            x,
            y,
            w: self.w.min(sw - x).max(0.),
            h: self.h.min(sh - y).max(0.),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

const HANDLES: [Handle; 8] = [
    Handle::NW,
    Handle::N,
    Handle::NE,
    Handle::E,
    Handle::SE,
    Handle::S,
    Handle::SW,
    Handle::W,
];

impl Handle {
    fn pos(self, r: R) -> P {
        let (cx, cy) = (r.x + r.w / 2., r.y + r.h / 2.);
        match self {
            Handle::N => (cx, r.y),
            Handle::S => (cx, r.bottom()),
            Handle::E => (r.right(), cy),
            Handle::W => (r.x, cy),
            Handle::NE => (r.right(), r.y),
            Handle::NW => (r.x, r.y),
            Handle::SE => (r.right(), r.bottom()),
            Handle::SW => (r.x, r.bottom()),
        }
    }

    fn cursor(self) -> CursorStyle {
        match self {
            Handle::N | Handle::S => CursorStyle::ResizeUpDown,
            Handle::E | Handle::W => CursorStyle::ResizeLeftRight,
            Handle::NW | Handle::SE => CursorStyle::ResizeUpLeftDownRight,
            Handle::NE | Handle::SW => CursorStyle::ResizeUpRightDownLeft,
        }
    }

    /// Moves the edges this handle controls to `p`.
    fn apply(self, r: R, p: P) -> R {
        let (mut l, mut t, mut rt, mut b) = (r.x, r.y, r.right(), r.bottom());
        match self {
            Handle::N => t = p.1,
            Handle::S => b = p.1,
            Handle::E => rt = p.0,
            Handle::W => l = p.0,
            Handle::NE => (t, rt) = (p.1, p.0),
            Handle::NW => (t, l) = (p.1, p.0),
            Handle::SE => (b, rt) = (p.1, p.0),
            Handle::SW => (b, l) = (p.1, p.0),
        }
        R::from_points((l, t), (rt, b))
    }
}

fn handle_at(r: R, p: P, edges: bool) -> Option<Handle> {
    let near = |q: P, d: f32| (q.0 - p.0).abs() <= d && (q.1 - p.1).abs() <= d;
    if let Some(h) = HANDLES.iter().find(|h| near(h.pos(r), 8.)) {
        return Some(*h);
    }
    if !edges {
        return None;
    }
    let within_x = p.0 > r.x && p.0 < r.right();
    let within_y = p.1 > r.y && p.1 < r.bottom();
    let d = 5.;
    if within_x && (p.1 - r.y).abs() <= d {
        Some(Handle::N)
    } else if within_x && (p.1 - r.bottom()).abs() <= d {
        Some(Handle::S)
    } else if within_y && (p.0 - r.x).abs() <= d {
        Some(Handle::W)
    } else if within_y && (p.0 - r.right()).abs() <= d {
        Some(Handle::E)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Drag {
    None,
    Create(P),
    Move { grab: P, start: R },
    Resize { handle: Handle, start: R },
    Draw,
}

pub struct Overlay {
    focus: FocusHandle,
    image: PathBuf,
    image_size: (u32, u32),
    base: Option<RgbaImage>,
    screen: P,
    mode: Mode,
    sel: Option<R>,
    drag: Drag,
    tool: Tool,
    color: u32,
    width_ix: usize,
    shapes: Vec<Shape>,
    redo: Vec<Shape>,
    current: Option<Shape>,
    /// Text being typed: position and content.
    text: Option<(P, String)>,
    mouse: P,
    palette_open: bool,
    record_cursor: bool,
    caret_on: bool,
    toast: Option<(SharedString, Instant)>,
    _ticker: Task<()>,
}

impl Overlay {
    fn new(
        image: PathBuf,
        image_size: (u32, u32),
        video: bool,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(530)).await;
                let alive = this.update(cx, |this, cx| {
                    this.caret_on = !this.caret_on;
                    if this.toast.as_ref().is_some_and(|t| t.1 <= Instant::now()) {
                        this.toast = None;
                    }
                    if this.text.is_some() || this.toast.is_some() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            focus: cx.focus_handle(),
            image,
            image_size,
            base: None,
            screen: (image_size.0 as f32, image_size.1 as f32),
            mode: if video { Mode::Video } else { Mode::Photo },
            sel: None,
            drag: Drag::None,
            tool: Tool::Move,
            color: PALETTE[0],
            width_ix: 1,
            shapes: Vec::new(),
            redo: Vec::new(),
            current: None,
            text: None,
            mouse: (-100., -100.),
            palette_open: false,
            record_cursor: true,
            caret_on: true,
            toast: None,
            _ticker: ticker,
        }
    }

    /// Image pixels per logical pixel.
    fn scale(&self) -> f32 {
        self.image_size.0 as f32 / self.screen.0.max(1.)
    }

    fn width(&self) -> f32 {
        WIDTHS[self.width_ix]
    }

    fn flash(&mut self, text: impl Into<SharedString>) {
        self.toast = Some((text.into(), Instant::now() + Duration::from_millis(2200)));
    }

    fn commit_text(&mut self) {
        if let Some((pos, text)) = self.text.take()
            && !text.trim().is_empty()
        {
            self.push(Shape {
                kind: ShapeKind::Text(pos, text),
                color: self.color,
                width: self.width(),
            });
        }
    }

    fn push(&mut self, shape: Shape) {
        self.shapes.push(shape);
        self.redo.clear();
    }

    fn set_tool(&mut self, tool: Tool) {
        self.commit_text();
        self.tool = tool;
        self.palette_open = false;
    }

    fn undo(&mut self) {
        if self.text.take().is_some() {
            return;
        }
        if let Some(s) = self.shapes.pop() {
            self.redo.push(s);
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.shapes.push(s);
        }
    }

    fn select_all(&mut self) {
        self.sel = Some(R {
            x: 0.,
            y: 0.,
            w: self.screen.0,
            h: self.screen.1,
        });
    }

    fn selection_or_screen(&mut self) -> R {
        if self.sel.is_none_or(|s| s.w < MIN_SEL || s.h < MIN_SEL) {
            self.select_all();
        }
        self.sel.expect("just set")
    }

    // ---- results --------------------------------------------------------------

    fn export(&mut self) -> Result<RgbaImage> {
        self.commit_text();
        let sel = self.selection_or_screen();
        if self.base.is_none() {
            self.base = Some(image::open(&self.image)?.into_rgba8());
        }
        let font = self
            .shapes
            .iter()
            .any(|s| matches!(s.kind, ShapeKind::Text(..)))
            .then(draw::load_font)
            .flatten();
        let base = self.base.as_ref().expect("loaded above");
        let scale = base.width() as f32 / self.screen.0;
        Ok(draw::render(
            base,
            (sel.x, sel.y, sel.w, sel.h),
            scale,
            &self.shapes,
            font.as_ref(),
        ))
    }

    fn put_on_clipboard(img: &RgbaImage) -> Result<()> {
        let dir = dirs::cache_dir().context("no cache dir")?.join("shotvibe");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("clipboard.png");
        img.save(&path)?;
        clipboard::hold_image(&path)
    }

    fn copy(&mut self, cx: &mut gpui::Context<Self>) {
        match self.export().and_then(|img| Self::put_on_clipboard(&img)) {
            Ok(()) => cx.quit(),
            Err(err) => self.flash(format!("Не удалось скопировать: {err:#}")),
        }
    }

    fn save(&mut self, cx: &mut gpui::Context<Self>) {
        let result = self.export().and_then(|img| {
            let dir = dirs::picture_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Pictures"))
                .join("Screenshots");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("Screenshot_{}.png", capture::timestamp()));
            img.save(&path)?;
            let _ = Self::put_on_clipboard(&img);
            Ok(path)
        });
        match result {
            Ok(path) => {
                if let Ok(n) = Notifier::new() {
                    let _ = n.notify(
                        "image-x-generic",
                        "Скриншот сохранён и скопирован",
                        &path.display().to_string(),
                        &[],
                        false,
                    );
                }
                cx.quit();
            }
            Err(err) => self.flash(format!("Не удалось сохранить: {err:#}")),
        }
    }

    fn start_recording(&mut self, cx: &mut gpui::Context<Self>) {
        let s = self.selection_or_screen();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap_or_default());
        cmd.arg("record-area")
            .args([s.x, s.y, s.w, s.h].map(|v| (v.round() as i32).to_string()));
        if !self.record_cursor {
            cmd.arg("--no-cursor");
        }
        use std::os::unix::process::CommandExt;
        match cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .process_group(0)
            .spawn()
        {
            Ok(_) => cx.quit(),
            Err(err) => self.flash(format!("Не удалось начать запись: {err}")),
        }
    }

    fn primary(&mut self, cx: &mut gpui::Context<Self>) {
        match self.mode {
            Mode::Photo => self.copy(cx),
            Mode::Video => self.start_recording(cx),
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        self.commit_text();
        self.mode = mode;
        self.palette_open = false;
    }

    // ---- input ----------------------------------------------------------------

    fn handles_visible(&self) -> bool {
        self.mode == Mode::Video || self.tool == Tool::Move
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, _: &mut Window, cx: &mut gpui::Context<Self>) {
        let p = pt(ev.position);
        self.mouse = p;
        self.palette_open = false;
        self.commit_text();

        if let Some(sel) = self.sel {
            if let Some(handle) = handle_at(sel, p, self.handles_visible()) {
                self.drag = Drag::Resize { handle, start: sel };
            } else if sel.contains(p) && self.mode == Mode::Photo && self.tool != Tool::Move {
                self.begin_shape(p, ev.modifiers.shift);
            } else if sel.contains(p) {
                self.drag = Drag::Move {
                    grab: p,
                    start: sel,
                };
            } else {
                self.sel = Some(R::from_points(p, p));
                self.drag = Drag::Create(p);
            }
        } else {
            self.sel = Some(R::from_points(p, p));
            self.drag = Drag::Create(p);
        }
        cx.notify();
    }

    fn begin_shape(&mut self, p: P, _shift: bool) {
        let kind = match self.tool {
            Tool::Text => {
                // Anchor the text so the click lands mid-line.
                let lh = draw::text_line_height(self.width());
                self.text = Some(((p.0, p.1 - lh / 2.), String::new()));
                return;
            }
            Tool::Pen => ShapeKind::Pen(vec![p]),
            Tool::Marker => ShapeKind::Marker(vec![p]),
            Tool::Line => ShapeKind::Line(p, p),
            Tool::Arrow => ShapeKind::Arrow(p, p),
            Tool::Rect => ShapeKind::Rect(p, p),
            Tool::Move => return,
        };
        self.current = Some(Shape {
            kind,
            color: self.color,
            width: self.width(),
        });
        self.drag = Drag::Draw;
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut gpui::Context<Self>) {
        let p = (
            pt(ev.position).0.clamp(0., self.screen.0),
            pt(ev.position).1.clamp(0., self.screen.1),
        );
        self.mouse = p;
        let shift = ev.modifiers.shift;
        match self.drag {
            Drag::None => {}
            Drag::Create(origin) => {
                let mut end = p;
                if shift {
                    let d = (p.0 - origin.0).abs().max((p.1 - origin.1).abs());
                    end = (
                        origin.0 + d * (p.0 - origin.0).signum(),
                        origin.1 + d * (p.1 - origin.1).signum(),
                    );
                }
                self.sel = Some(R::from_points(origin, end).clamp_to(self.screen));
            }
            Drag::Move { grab, start } => {
                let x = (start.x + p.0 - grab.0).clamp(0., self.screen.0 - start.w);
                let y = (start.y + p.1 - grab.1).clamp(0., self.screen.1 - start.h);
                self.sel = Some(R { x, y, ..start });
            }
            Drag::Resize { handle, start } => {
                self.sel = Some(handle.apply(start, p).clamp_to(self.screen));
            }
            Drag::Draw => {
                if let Some(shape) = &mut self.current {
                    match &mut shape.kind {
                        ShapeKind::Pen(pts) | ShapeKind::Marker(pts) => {
                            let last = *pts.last().expect("non-empty");
                            if (last.0 - p.0).hypot(last.1 - p.1) >= 1.5 {
                                pts.push(p);
                            }
                        }
                        ShapeKind::Line(a, b) | ShapeKind::Arrow(a, b) => {
                            *b = if shift { snap_45(*a, p) } else { p };
                        }
                        ShapeKind::Rect(a, b) => {
                            *b = if shift {
                                let d = (p.0 - a.0).abs().max((p.1 - a.1).abs());
                                (
                                    a.0 + d * (p.0 - a.0).signum(),
                                    a.1 + d * (p.1 - a.1).signum(),
                                )
                            } else {
                                p
                            };
                        }
                        ShapeKind::Text(..) => {}
                    }
                }
            }
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut gpui::Context<Self>) {
        match self.drag {
            Drag::Create(_) => {
                if self.sel.is_some_and(|s| s.w < MIN_SEL || s.h < MIN_SEL) {
                    self.sel = None;
                }
            }
            Drag::Draw => {
                if let Some(shape) = self.current.take() {
                    self.push(shape);
                }
            }
            _ => {}
        }
        self.drag = Drag::None;
        cx.notify();
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut gpui::Context<Self>) {
        let dy = match ev.delta {
            ScrollDelta::Lines(d) => d.y,
            ScrollDelta::Pixels(d) => f32::from(d.y) / 20.,
        };
        if dy.abs() < 0.3 || self.mode != Mode::Photo {
            return;
        }
        self.change_width(if dy > 0. { 1 } else { -1 });
        cx.notify();
    }

    fn change_width(&mut self, delta: isize) {
        let ix = (self.width_ix as isize + delta).clamp(0, WIDTHS.len() as isize - 1);
        self.width_ix = ix as usize;
        self.flash(format!("Толщина {}", self.width()));
    }

    fn key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut gpui::Context<Self>) {
        let ks = &ev.keystroke;
        let (ctrl, alt, shift) = (ks.modifiers.control, ks.modifiers.alt, ks.modifiers.shift);
        let key = ks.key.as_str();
        self.caret_on = true;

        if let Some((_, text)) = &mut self.text {
            match key {
                "escape" => self.text = None,
                "enter" => self.commit_text(),
                "backspace" => {
                    text.pop();
                }
                _ => {
                    if !ctrl
                        && !alt
                        && let Some(ch) = ks.key_char.as_deref()
                        && !ch.chars().any(char::is_control)
                    {
                        text.push_str(ch);
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let photo = self.mode == Mode::Photo;
        match key {
            "escape" if self.palette_open => self.palette_open = false,
            "escape" => cx.quit(),
            "enter" => self.primary(cx),
            "c" if ctrl => self.copy(cx),
            "s" if ctrl => self.save(cx),
            "z" if ctrl && shift => self.redo(),
            "y" if ctrl => self.redo(),
            "z" if ctrl => self.undo(),
            "a" if ctrl => self.select_all(),
            "tab" => self.set_mode(if photo { Mode::Video } else { Mode::Photo }),
            "v" | "m" if photo && !ctrl => self.set_tool(Tool::Move),
            "p" if photo => self.set_tool(Tool::Pen),
            "l" if photo => self.set_tool(Tool::Line),
            "a" if photo => self.set_tool(Tool::Arrow),
            "r" if photo => self.set_tool(Tool::Rect),
            "h" if photo => self.set_tool(Tool::Marker),
            "t" if photo => self.set_tool(Tool::Text),
            "c" if photo => {
                let ix = PALETTE.iter().position(|c| *c == self.color).unwrap_or(0);
                self.color = PALETTE[(ix + 1) % PALETTE.len()];
            }
            "[" | "-" => self.change_width(-1),
            "]" | "=" | "+" => self.change_width(1),
            "left" | "right" | "up" | "down" => {
                if let Some(s) = self.sel {
                    let step = if shift { 10. } else { 1. };
                    let (dx, dy) = match key {
                        "left" => (-step, 0.),
                        "right" => (step, 0.),
                        "up" => (0., -step),
                        _ => (0., step),
                    };
                    let r = if alt {
                        // Alt+arrows resize from the bottom-right corner.
                        R {
                            w: (s.w + dx).max(MIN_SEL),
                            h: (s.h + dy).max(MIN_SEL),
                            ..s
                        }
                    } else {
                        R {
                            x: (s.x + dx).clamp(0., self.screen.0 - s.w),
                            y: (s.y + dy).clamp(0., self.screen.1 - s.h),
                            ..s
                        }
                    };
                    self.sel = Some(r.clamp_to(self.screen));
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn cursor(&self) -> CursorStyle {
        match self.drag {
            Drag::Move { .. } => return CursorStyle::ClosedHand,
            Drag::Resize { handle, .. } => return handle.cursor(),
            Drag::Create(_) | Drag::Draw => return CursorStyle::Crosshair,
            Drag::None => {}
        }
        let Some(sel) = self.sel else {
            return CursorStyle::Crosshair;
        };
        if let Some(h) = handle_at(sel, self.mouse, self.handles_visible()) {
            return h.cursor();
        }
        if !sel.contains(self.mouse) {
            return CursorStyle::Crosshair;
        }
        match (self.mode, self.tool) {
            (Mode::Photo, Tool::Text) => CursorStyle::IBeam,
            (Mode::Photo, Tool::Move) | (Mode::Video, _) => CursorStyle::OpenHand,
            _ => CursorStyle::Crosshair,
        }
    }
}

fn snap_45(a: P, p: P) -> P {
    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
    let len = dx.hypot(dy);
    let angle = (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
    (a.0 + len * angle.cos(), a.1 + len * angle.sin())
}

// ---- painting -------------------------------------------------------------------

fn shape_color(s: &Shape) -> Hsla {
    let mut h: Hsla = rgb(s.color).into();
    h.a = s.alpha();
    h
}

fn paint_shape(window: &mut Window, s: &Shape) {
    let color = shape_color(s);
    let w = s.stroke_width();
    let opts = StrokeOptions::default()
        .with_line_width(w)
        .with_line_cap(LineCap::Round)
        .with_line_join(LineJoin::Round);
    let stroke = |window: &mut Window, pts: &[P], opts: StrokeOptions| {
        if pts.iter().all(|p| *p == pts[0]) {
            let r = w / 2.;
            window.paint_quad(
                gpui::fill(
                    Bounds::new(
                        point(px(pts[0].0 - r), px(pts[0].1 - r)),
                        size(px(w), px(w)),
                    ),
                    color,
                )
                .corner_radii(px(r)),
            );
            return;
        }
        let mut pb = PathBuilder::stroke(px(w)).with_style(PathStyle::Stroke(opts));
        pb.move_to(point(px(pts[0].0), px(pts[0].1)));
        for p in &pts[1..] {
            pb.line_to(point(px(p.0), px(p.1)));
        }
        if let Ok(path) = pb.build() {
            window.paint_path(path, color);
        }
    };
    match &s.kind {
        ShapeKind::Pen(pts) | ShapeKind::Marker(pts) => stroke(window, pts, opts),
        ShapeKind::Line(a, b) => stroke(window, &[*a, *b], opts),
        ShapeKind::Arrow(a, b) => {
            let (shaft_end, head) = draw::arrow_geometry(*a, *b, s.width);
            stroke(window, &[*a, shaft_end], opts);
            let mut pb = PathBuilder::fill();
            pb.add_polygon(&head.map(|p| point(px(p.0), px(p.1))), true);
            if let Ok(path) = pb.build() {
                window.paint_path(path, color);
            }
        }
        ShapeKind::Rect(a, b) => {
            let (x, y, rw, rh) = draw::rect_from(*a, *b);
            if rw < 0.5 && rh < 0.5 {
                return;
            }
            let mut pb = PathBuilder::stroke(px(w))
                .with_style(PathStyle::Stroke(opts.with_line_join(LineJoin::Miter)));
            pb.add_polygon(
                &[(x, y), (x + rw, y), (x + rw, y + rh), (x, y + rh)]
                    .map(|p| point(px(p.0), px(p.1))),
                true,
            );
            if let Ok(path) = pb.build() {
                window.paint_path(path, color);
            }
        }
        ShapeKind::Text(..) => {}
    }
}

fn text_element(
    pos: P,
    text: String,
    color: u32,
    width: f32,
    origin: P,
    caret: Option<bool>,
) -> Div {
    let lh = draw::text_line_height(width);
    div()
        .absolute()
        .left(px(pos.0 - origin.0))
        .top(px(pos.1 - origin.1))
        .h(px(lh))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .font_family(TEXT_FONT)
        .font_weight(FontWeight::BOLD)
        .text_size(px(draw::text_size(width)))
        .line_height(px(lh))
        .text_color(rgb(color))
        .child(text)
        .when_some(caret, |d, on| {
            d.child(
                div()
                    .w(px(2.))
                    .h(px(lh * 0.8))
                    .bg(rgb(color))
                    .when(!on, |d| d.opacity(0.)),
            )
        })
}

fn panel() -> Div {
    div()
        .absolute()
        .flex()
        .p(px(5.))
        .gap(px(2.))
        .rounded(px(12.))
        .bg(c(PANEL_BG))
        .border_1()
        .border_color(c(0xffffff1f))
        .shadow_lg()
        .text_color(c(0xf2f2f7ff))
        .font_family(UI_FONT)
}

fn icon_button(id: impl Into<ElementId>, icon: Icon, active: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(BTN))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .when(active, |d| d.bg(c(0x0a84ff40)))
        .hover(|s| s.bg(c(0xffffff1c)))
        .child(svg().path(icon.path()).size(px(18.)).text_color(if active {
            c(0x64b5ffff)
        } else {
            c(0xe5e5eaff)
        }))
}

fn separator(vertical_bar: bool) -> Div {
    if vertical_bar {
        div().mx(px(6.)).my(px(4.)).h(px(1.)).bg(c(0xffffff1f))
    } else {
        div().my(px(6.)).mx(px(4.)).w(px(1.)).bg(c(0xffffff1f))
    }
}

impl Overlay {
    fn tools_position(&self, s: R) -> P {
        let (sw, sh) = self.screen;
        let mut x = s.right() + 10.;
        if x + TOOLS_W > sw - 8. {
            x = s.x - TOOLS_W - 10.;
        }
        if x < 8. {
            x = s.right() - TOOLS_W - 8.;
        }
        (x, s.y.clamp(8., (sh - TOOLS_H - 8.).max(8.)))
    }

    fn actions_position(&self, s: R) -> P {
        let (sw, sh) = self.screen;
        let x = (s.right() - ACTIONS_W).clamp(8., (sw - ACTIONS_W - 8.).max(8.));
        let mut y = s.bottom() + 10.;
        if y + ACTIONS_H > sh - 8. {
            y = s.y - ACTIONS_H - 10.;
        }
        if y < 8. {
            y = s.bottom() - ACTIONS_H - 8.;
        }
        (x, y)
    }

    fn render_tools(&self, s: R, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let (x, y) = self.tools_position(s);
        let tool = |id: &'static str, t: Tool, icon: Icon| {
            icon_button(id, icon, self.tool == t).on_click(cx.listener(move |this, _, _, cx| {
                this.set_tool(t);
                cx.notify();
            }))
        };
        let dot = self.width() + 4.;
        let bar = panel()
            .id("tools")
            .occlude()
            .left(px(x))
            .top(px(y))
            .flex_col()
            .child(tool("t-move", Tool::Move, Icon::Move))
            .child(tool("t-pen", Tool::Pen, Icon::Pencil))
            .child(tool("t-line", Tool::Line, Icon::Line))
            .child(tool("t-arrow", Tool::Arrow, Icon::Arrow))
            .child(tool("t-rect", Tool::Rect, Icon::Square))
            .child(tool("t-marker", Tool::Marker, Icon::Highlighter))
            .child(tool("t-text", Tool::Text, Icon::Type))
            .child(separator(true))
            .child(
                div()
                    .id("t-color")
                    .size(px(BTN))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(0xffffff1c)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.palette_open = !this.palette_open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(18.))
                            .rounded(px(9.))
                            .bg(rgb(self.color))
                            .border_2()
                            .border_color(c(0xffffffcc)),
                    ),
            )
            .child(
                div()
                    .id("t-width")
                    .size(px(BTN))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(0xffffff1c)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.width_ix = (this.width_ix + 1) % WIDTHS.len();
                        let w = this.width();
                        this.flash(format!("Толщина {w}"));
                        cx.notify();
                    }))
                    .child(div().size(px(dot)).rounded(px(dot / 2.)).bg(c(0xe5e5eaff))),
            )
            .child(separator(true))
            .child(
                icon_button("t-undo", Icon::Undo, false).on_click(cx.listener(|this, _, _, cx| {
                    this.undo();
                    cx.notify();
                })),
            )
            .child(
                icon_button("t-redo", Icon::Redo, false).on_click(cx.listener(|this, _, _, cx| {
                    this.redo();
                    cx.notify();
                })),
            );
        bar
    }

    fn render_palette(&self, s: R, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let (tx, ty) = self.tools_position(s);
        let x = if tx - 54. > 8. {
            tx - 54.
        } else {
            tx + TOOLS_W + 10.
        };
        panel()
            .id("palette")
            .occlude()
            .left(px(x))
            .top(px(ty + 7. * (BTN + 2.) + 9.))
            .flex_col()
            .children(PALETTE.into_iter().map(|col| {
                let active = col == self.color;
                div()
                    .id(("swatch", col as usize))
                    .size(px(BTN))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(c(0xffffff1c)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.color = col;
                        this.palette_open = false;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(20.))
                            .rounded(px(10.))
                            .bg(rgb(col))
                            .border_2()
                            .border_color(if active { c(0xffffffff) } else { c(0xffffff40) }),
                    )
            }))
    }

    fn render_actions(&self, s: R, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let (x, y) = self.actions_position(s);
        let primary = |id: &'static str, label: &'static str, color: u32| {
            div()
                .id(id)
                .h(px(BTN))
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(8.))
                .cursor_pointer()
                .bg(c(color))
                .hover(|s| s.opacity(0.88))
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(0xffffffff))
                .child(label)
                .child(div().text_size(px(11.)).opacity(0.75).child("↵"))
        };
        let close = icon_button("a-close", Icon::Close, false)
            .on_click(cx.listener(|_, _, _, cx| cx.quit()));
        let bar = panel()
            .id("actions")
            .occlude()
            .left(px(x))
            .top(px(y))
            .w(px(ACTIONS_W))
            .items_center()
            .justify_end();
        match self.mode {
            Mode::Photo => bar
                .child(
                    icon_button("a-save", Icon::Download, false).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.save(cx);
                            cx.notify();
                        },
                    )),
                )
                .child(
                    primary("a-copy", "Копировать", ACCENT).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.copy(cx);
                            cx.notify();
                        },
                    )),
                )
                .child(close),
            Mode::Video => bar
                .child(
                    icon_button("a-cursor", Icon::Pointer, self.record_cursor).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.record_cursor = !this.record_cursor;
                            let msg = if this.record_cursor {
                                "Курсор будет на записи"
                            } else {
                                "Курсор скрыт на записи"
                            };
                            this.flash(msg);
                            cx.notify();
                        }),
                    ),
                )
                .child(primary("a-rec", "Записать", RECORD).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.start_recording(cx);
                        cx.notify();
                    },
                )))
                .child(close),
        }
    }

    fn render_topbar(&self, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let (sw, sh) = self.screen;
        let w = 640.;
        let x = (sw - w) / 2.;
        let at_top = !self
            .sel
            .is_some_and(|s| s.y < 80. && s.x < x + w && s.right() > x && self.drag == Drag::None);
        let hint = match (self.sel.is_some(), self.mode) {
            (false, _) => "Выделите область  ·  Enter — весь экран  ·  Esc — выход",
            (true, Mode::Photo) => {
                "Enter — копировать  ·  Ctrl+S — сохранить  ·  Ctrl+Z — отменить"
            }
            (true, Mode::Video) => "Enter — начать запись  ·  стоп: Super+Shift+R",
        };
        let segment = |id: &'static str, icon: Icon, label: &'static str, mode: Mode| {
            let active = self.mode == mode;
            div()
                .id(id)
                .h(px(30.))
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded(px(8.))
                .cursor_pointer()
                .text_size(px(13.))
                .when(active, |d| d.bg(c(0xffffff26)))
                .when(!active, |d| {
                    d.text_color(c(0xaeaeb2ff)).hover(|s| s.bg(c(0xffffff12)))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_mode(mode);
                    cx.notify();
                }))
                .child(svg().path(icon.path()).size(px(15.)).text_color(if active {
                    if mode == Mode::Video {
                        c(RECORD)
                    } else {
                        c(0xffffffff)
                    }
                } else {
                    c(0xaeaeb2ff)
                }))
                .child(label)
        };
        div()
            .absolute()
            .left_0()
            .w_full()
            .when(at_top, |d| d.top(px(16.)))
            .when(!at_top, |d| d.top(px(sh - 58.)))
            .flex()
            .justify_center()
            .child(
                panel()
                    .id("topbar")
                    .occlude()
                    .relative()
                    .items_center()
                    .gap(px(4.))
                    .child(segment("m-photo", Icon::Camera, "Скриншот", Mode::Photo))
                    .child(segment("m-video", Icon::Video, "Видео", Mode::Video))
                    .child(separator(false).h(px(20.)))
                    .child(
                        div()
                            .px(px(10.))
                            .text_size(px(12.5))
                            .text_color(c(0xaeaeb2ff))
                            .child(hint),
                    ),
            )
    }
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let vs = window.viewport_size();
        self.screen = (f32::from(vs.width), f32::from(vs.height));
        let (sw, sh) = self.screen;
        let dim = c(0x00000080);
        let scale = self.scale();

        let mut root = div()
            .id("overlay")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .cursor(self.cursor())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(c(0x000000ff))
            .font_family(UI_FONT)
            .text_color(c(0xffffffff))
            .child(
                img(self.image.clone())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            );

        let rect = |x: f32, y: f32, w: f32, h: f32| {
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w.max(0.)))
                .h(px(h.max(0.)))
                .bg(dim)
        };
        match self.sel {
            None => root = root.child(rect(0., 0., sw, sh)),
            Some(s) => {
                root = root
                    .child(rect(0., 0., sw, s.y))
                    .child(rect(0., s.bottom(), sw, sh - s.bottom()))
                    .child(rect(0., s.y, s.x, s.h))
                    .child(rect(s.right(), s.y, sw - s.right(), s.h));
            }
        }

        if let Some(s) = self.sel {
            // Annotations, clipped to the selection.
            let shapes: Vec<Shape> = self
                .shapes
                .iter()
                .cloned()
                .chain(self.current.clone())
                .collect();
            let origin = (s.x, s.y);
            let texts: Vec<_> = self
                .shapes
                .iter()
                .filter_map(|sh| match &sh.kind {
                    ShapeKind::Text(pos, t) => Some(text_element(
                        *pos,
                        t.clone(),
                        sh.color,
                        sh.width,
                        origin,
                        None,
                    )),
                    _ => None,
                })
                .collect();
            let editing = self.text.as_ref().map(|(pos, t)| {
                text_element(
                    *pos,
                    t.clone(),
                    self.color,
                    self.width(),
                    origin,
                    Some(self.caret_on),
                )
            });
            root = root.child(
                div()
                    .absolute()
                    .left(px(s.x))
                    .top(px(s.y))
                    .w(px(s.w))
                    .h(px(s.h))
                    .overflow_hidden()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, _| {
                                for shape in &shapes {
                                    paint_shape(window, shape);
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(texts)
                    .children(editing),
            );

            // Frame and handles.
            root = root.child(
                div()
                    .absolute()
                    .left(px(s.x - 1.))
                    .top(px(s.y - 1.))
                    .w(px(s.w + 2.))
                    .h(px(s.h + 2.))
                    .border_1()
                    .border_color(if self.mode == Mode::Video {
                        c(RECORD)
                    } else {
                        c(0xffffffd9)
                    }),
            );
            if self.handles_visible() && !matches!(self.drag, Drag::Create(_)) {
                root = root.children(HANDLES.iter().map(|h| {
                    let (hx, hy) = h.pos(s);
                    div()
                        .absolute()
                        .left(px(hx - 4.5))
                        .top(px(hy - 4.5))
                        .size(px(9.))
                        .rounded(px(2.))
                        .bg(c(0xffffffff))
                        .border_1()
                        .border_color(c(0x00000099))
                }));
            }

            // Size label.
            let label_y = if s.y > 30. { s.y - 28. } else { s.y + 6. };
            root = root.child(
                div()
                    .absolute()
                    .left(px(s.x.max(4.)))
                    .top(px(label_y))
                    .px(px(8.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(c(0x000000b3))
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!(
                        "{} × {}",
                        (s.w * scale).round() as u32,
                        (s.h * scale).round() as u32
                    )),
            );
        } else if self.mouse.0 >= 0. {
            // Crosshair guides with the cursor position.
            let (mx, my) = self.mouse;
            root = root
                .child(
                    div()
                        .absolute()
                        .left(px(mx))
                        .top_0()
                        .w(px(1.))
                        .h_full()
                        .bg(c(0xffffff59)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(my))
                        .left_0()
                        .h(px(1.))
                        .w_full()
                        .bg(c(0xffffff59)),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(if mx + 110. > sw { mx - 100. } else { mx + 12. }))
                        .top(px(if my + 40. > sh { my - 30. } else { my + 12. }))
                        .px(px(7.))
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(c(0x000000b3))
                        .text_size(px(11.5))
                        .child(format!(
                            "{}, {}",
                            (mx * scale).round(),
                            (my * scale).round()
                        )),
                );
        }

        let idle = matches!(self.drag, Drag::None | Drag::Draw);
        if let Some(s) = self.sel
            && idle
            && s.w >= MIN_SEL
            && s.h >= MIN_SEL
        {
            if self.mode == Mode::Photo {
                root = root.child(self.render_tools(s, cx));
                if self.palette_open {
                    root = root.child(self.render_palette(s, cx));
                }
            }
            root = root.child(self.render_actions(s, cx));
        }
        if !matches!(self.drag, Drag::Create(_)) {
            root = root.child(self.render_topbar(cx));
        }
        if let Some((text, _)) = &self.toast {
            root = root.child(
                div()
                    .absolute()
                    .bottom(px(90.))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        panel()
                            .relative()
                            .px(px(14.))
                            .py(px(8.))
                            .text_size(px(13.))
                            .child(text.clone()),
                    ),
            );
        }
        root
    }
}

pub fn run(image: PathBuf, video: bool) -> Result<()> {
    let image_size = image::image_dimensions(&image)?;
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::new(
                point(px(0.), px(0.)),
                size(px(image_size.0 as f32), px(image_size.1 as f32)),
            );
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Fullscreen(bounds)),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some("Shotvibe".into()),
                            appears_transparent: true,
                            traffic_light_position: None,
                        }),
                        focus: true,
                        show: true,
                        kind: WindowKind::Normal,
                        is_movable: false,
                        is_minimizable: false,
                        window_background: WindowBackgroundAppearance::Opaque,
                        window_decorations: Some(WindowDecorations::Client),
                        app_id: Some("shotvibe".into()),
                        ..Default::default()
                    },
                    |_, cx| cx.new(|cx| Overlay::new(image.clone(), image_size, video, cx)),
                )
                .expect("failed to open overlay window");
            window
                .update(cx, |view, window, cx| {
                    window.focus(&view.focus);
                    cx.activate(true);
                })
                .ok();
            #[cfg(debug_assertions)]
            if let Ok(script) = std::env::var("SHOTVIBE_DEMO") {
                demo(window, script, cx);
            }
        });
    Ok(())
}

/// Debug helper: `down:x,y` / `move:x,y` / `up:x,y`
/// drive the mouse handlers, anything else is a keystroke (`text:` types).
#[cfg(debug_assertions)]
fn demo(window: gpui::WindowHandle<Overlay>, script: String, cx: &mut App) {
    cx.spawn(async move |cx| {
        let ms = std::env::var("SHOTVIBE_DEMO_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        for step in script.split(';') {
            Timer::after(Duration::from_millis(ms)).await;
            let mouse = step.split_once(':').and_then(|(k, xy)| {
                let (x, y) = xy.split_once(',')?;
                Some((
                    k.to_string(),
                    point(px(x.parse().ok()?), px(y.parse().ok()?)),
                ))
            });
            if let Some((k, position)) = mouse {
                let _ = window.update(cx, |view, window, cx| match k.as_str() {
                    "down" => view.mouse_down(
                        &MouseDownEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Default::default(),
                            click_count: 1,
                            first_mouse: false,
                        },
                        window,
                        cx,
                    ),
                    "move" => view.mouse_move(
                        &MouseMoveEvent {
                            position,
                            pressed_button: Some(MouseButton::Left),
                            modifiers: Default::default(),
                        },
                        window,
                        cx,
                    ),
                    _ => view.mouse_up(
                        &MouseUpEvent {
                            button: MouseButton::Left,
                            position,
                            modifiers: Default::default(),
                            click_count: 1,
                        },
                        window,
                        cx,
                    ),
                });
                continue;
            }
            let keys: Vec<gpui::Keystroke> = match step.strip_prefix("text:") {
                Some(t) => t
                    .chars()
                    .map(|ch| gpui::Keystroke {
                        modifiers: Default::default(),
                        key: ch.to_string(),
                        key_char: Some(ch.to_string()),
                    })
                    .collect(),
                None => gpui::Keystroke::parse(step).into_iter().collect(),
            };
            let _ = cx.update_window(window.into(), |_, window, cx| {
                for k in keys {
                    window.dispatch_keystroke(k, cx);
                }
            });
        }
    })
    .detach();
}
