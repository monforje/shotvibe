//! Inline Lucide-style icons served through gpui's asset system.

use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

#[derive(Clone, Copy)]
pub enum Icon {
    Move,
    Pencil,
    Line,
    Arrow,
    Square,
    Highlighter,
    Type,
    Undo,
    Redo,
    Camera,
    Video,
    Download,
    Close,
    Pointer,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Icon::Move => "icons/move.svg",
            Icon::Pencil => "icons/pencil.svg",
            Icon::Line => "icons/line.svg",
            Icon::Arrow => "icons/arrow.svg",
            Icon::Square => "icons/square.svg",
            Icon::Highlighter => "icons/highlighter.svg",
            Icon::Type => "icons/type.svg",
            Icon::Undo => "icons/undo.svg",
            Icon::Redo => "icons/redo.svg",
            Icon::Camera => "icons/camera.svg",
            Icon::Video => "icons/video.svg",
            Icon::Download => "icons/download.svg",
            Icon::Close => "icons/close.svg",
            Icon::Pointer => "icons/pointer.svg",
        }
    }
}

fn body(path: &str) -> Option<&'static str> {
    Some(match path {
        "icons/move.svg" => {
            r#"<path d="M12 2v20M2 12h20M9 5l3-3 3 3M9 19l3 3 3-3M5 9l-3 3 3 3M19 9l3 3-3 3"/>"#
        }
        "icons/pencil.svg" => {
            r#"<path d="M21.17 6.81a2.83 2.83 0 0 0-4-4L3.84 16.17a2 2 0 0 0-.5.83l-1.32 4.35a.5.5 0 0 0 .62.62l4.35-1.32a2 2 0 0 0 .83-.5z"/><path d="m15 5 4 4"/>"#
        }
        "icons/line.svg" => r#"<path d="M5 19 19 5"/>"#,
        "icons/arrow.svg" => r#"<path d="M7 17 17 7M8 7h9v9"/>"#,
        "icons/square.svg" => r#"<rect x="4" y="4" width="16" height="16" rx="1.5"/>"#,
        "icons/highlighter.svg" => {
            r#"<path d="m9 11-6 6v3h9l3-3"/><path d="m22 12-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4"/>"#
        }
        "icons/type.svg" => r#"<path d="M4 7V4h16v3M9 20h6M12 4v16"/>"#,
        "icons/undo.svg" => {
            r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>"#
        }
        "icons/redo.svg" => {
            r#"<path d="m15 14 5-5-5-5"/><path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>"#
        }
        "icons/camera.svg" => {
            r#"<path d="M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3l-2.5-3z"/><circle cx="12" cy="13" r="3"/>"#
        }
        "icons/video.svg" => {
            r#"<path d="m16 13 5.22 3.48a.5.5 0 0 0 .78-.42V7.87a.5.5 0 0 0-.76-.43L16 10.5"/><rect x="2" y="6" width="14" height="12" rx="2"/>"#
        }
        "icons/download.svg" => {
            r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3"/>"#
        }
        "icons/close.svg" => r#"<path d="M18 6 6 18M6 6l12 12"/>"#,
        "icons/pointer.svg" => {
            r#"<path d="M4.04 4.95 10.37 20.6a.5.5 0 0 0 .93-.04l2.33-6.86 6.86-2.33a.5.5 0 0 0 .04-.93L4.95 4.04a.5.5 0 0 0-.91.91z"/>"#
        }
        _ => return None,
    })
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(body(path).map(|b| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{b}</svg>"#
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
