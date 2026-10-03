//! A screen recording session: `shotvibe record-area X Y W H [--no-cursor]`.
//! Runs headless until stopped by the hotkey (any `shotvibe` run while a
//! recording is active) or by the "Остановить" button in the notification.

use crate::capture::{self, Notifier, Screencast};
use crate::clipboard;
use anyhow::{Context as _, Result};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn socket() -> std::path::PathBuf {
    crate::runtime_dir().join("shotvibe-rec.sock")
}

/// If a recording is running, asks it to stop and returns true.
pub fn stop_if_running() -> bool {
    UnixStream::connect(socket()).is_ok()
}

enum Event {
    Stop,
    Action(u32, String),
}

pub fn run(args: &[String]) -> Result<()> {
    let nums: Vec<i32> = args.iter().filter_map(|a| a.parse().ok()).collect();
    let [x, y, w, h] = nums[..] else {
        anyhow::bail!("usage: shotvibe record-area X Y W H [--no-cursor]");
    };
    let cursor = !args.iter().any(|a| a == "--no-cursor");
    if stop_if_running() {
        return Ok(());
    }

    let dir = dirs::video_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Videos"))
        .join("Screencasts");
    std::fs::create_dir_all(&dir)?;
    // GNOME appends the container extension itself.
    let file = dir.join(format!("Screencast_{}", capture::timestamp()));

    let sock = socket();
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;

    let notifier = Notifier::new()?;
    // Let the selection overlay disappear before the first frame.
    std::thread::sleep(Duration::from_millis(300));
    let (cast, file) = match Screencast::start((x, y, w, h), &file, cursor) {
        Ok(started) => started,
        Err(err) => {
            let _ = std::fs::remove_file(&sock);
            let _ = notifier.notify(
                "dialog-error",
                "Запись не началась",
                &format!("{err:#}"),
                &[],
                false,
            );
            return Err(err);
        }
    };
    let started = Instant::now();
    let rec_id = notifier.notify(
        "media-record",
        "Идёт запись экрана",
        &format!("{w}×{h} · Super+Shift+R или кнопка ниже — остановить"),
        &[("stop", "Остановить")],
        true,
    )?;

    let (tx, rx) = mpsc::channel();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                let _ = tx.send(Event::Stop);
            }
        });
    }
    {
        let tx = tx.clone();
        notifier.on_action(move |id, key| {
            let _ = tx.send(Event::Action(id, key));
        })?;
    }

    loop {
        match rx.recv().context("event channel closed")? {
            Event::Stop => break,
            Event::Action(id, key) if id == rec_id && key == "stop" => break,
            Event::Action(..) => {}
        }
    }
    let _ = std::fs::remove_file(&sock);
    cast.stop()?;
    notifier.close(rec_id);
    let secs = started.elapsed().as_secs();

    // GNOME finalizes the file asynchronously; wait until it has data.
    for _ in 0..50 {
        if std::fs::metadata(&file)
            .map(|m| m.len() > 1024)
            .unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let copied = clipboard::hold_files(&[&file]).is_ok();

    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let done_id = notifier.notify(
        "video-x-generic",
        if copied {
            "Запись сохранена и скопирована"
        } else {
            "Запись сохранена"
        },
        &format!("{name} · {}:{:02}", secs / 60, secs % 60),
        &[("open", "Открыть"), ("folder", "Показать в папке")],
        false,
    )?;
    // Stay around for a while so the notification buttons keep working.
    let deadline = Instant::now() + Duration::from_secs(90);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Action(id, key)) if id == done_id => {
                match key.as_str() {
                    "open" => {
                        let _ = std::process::Command::new("xdg-open").arg(&file).spawn();
                    }
                    "folder" => capture::show_in_folder(&file),
                    _ => {}
                }
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}
