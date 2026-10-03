//! Clipboard ownership outliving the overlay.
//!
//! The overlay quits right after "copy", but on Linux the clipboard is served
//! by its owner. So a small detached `shotvibe hold …` process takes the
//! clipboard (through XWayland, which GNOME bridges to Wayland apps) and
//! exits as soon as anything else is copied.

use anyhow::{Context as _, Result, bail};
use arboard::{Clipboard, SetExtLinux};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn hold_image(png: &Path) -> Result<()> {
    spawn_holder(&["hold-image", &png.display().to_string()])
}

pub fn hold_files(files: &[&Path]) -> Result<()> {
    let mut args = vec!["hold-files".to_string()];
    args.extend(files.iter().map(|f| f.display().to_string()));
    spawn_holder(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn spawn_holder(args: &[&str]) -> Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    // The holder prints a line once it owns the clipboard.
    let mut out = child.stdout.take().context("no holder stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 3];
        let _ = tx.send(
            std::io::Read::read(&mut out, &mut buf)
                .map(|n| n > 0)
                .unwrap_or(false),
        );
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => Ok(()),
        _ => bail!("не удалось занять буфер обмена"),
    }
}

/// Entry point of the detached holder process.
pub fn run_holder(args: &[String]) -> Result<()> {
    let (kind, rest) = args.split_first().context("missing holder kind")?;
    let mut clipboard = Clipboard::new()?;
    let announce = || {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"ok\n");
        let _ = out.flush();
    };
    match kind.as_str() {
        "hold-image" => {
            let img = image::open(rest.first().context("missing image")?)?.into_rgba8();
            let data = arboard::ImageData {
                width: img.width() as usize,
                height: img.height() as usize,
                bytes: img.into_raw().into(),
            };
            // Set once without waiting so we can announce success, then block
            // in `wait()` until someone else takes the clipboard.
            clipboard.set_image(data.clone())?;
            announce();
            clipboard.set().wait().image(data)?;
        }
        "hold-files" => {
            clipboard.set().file_list(rest)?;
            announce();
            clipboard.set().wait().file_list(rest)?;
        }
        other => bail!("unknown holder kind {other}"),
    }
    Ok(())
}
