mod capture;
mod clipboard;
mod draw;
mod icons;
mod overlay;
mod record;

use anyhow::Result;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

const HELP: &str = "\
shotvibe — скриншоты и запись экрана в стиле Lightshot

  shotvibe                  выделить область, порисовать, скопировать/сохранить
  shotvibe --video          то же, сразу в режиме записи видео
  shotvibe --delay MS       подождать перед снимком
  shotvibe install          бинарник в ~/.local/bin + хоткеи GNOME:
                            Print — скриншот, Super+Shift+R — запись (повторно — стоп)

Повторный запуск закрывает открытое окно или останавливает идущую запись.
";

pub fn runtime_dir() -> PathBuf {
    dirs::runtime_dir().unwrap_or_else(std::env::temp_dir)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("record-area") => record::run(rest),
        Some("hold-image" | "hold-files") => clipboard::run_holder(&args),
        Some("install") => install(),
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        _ => shoot(&args),
    }
}

fn shoot(args: &[String]) -> Result<()> {
    // The same hotkey stops a running recording…
    if record::stop_if_running() {
        return Ok(());
    }
    // …and closes an open overlay.
    let sock = runtime_dir().join("shotvibe.sock");
    if let Ok(mut stream) = UnixStream::connect(&sock) {
        let _ = stream.write_all(b"close\n");
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    {
        let sock = sock.clone();
        std::thread::spawn(move || {
            if listener.accept().is_ok() {
                let _ = std::fs::remove_file(sock);
                std::process::exit(0);
            }
        });
    }

    let video = args.iter().any(|a| a == "--video");
    let delay = args
        .iter()
        .position(|a| a == "--delay")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    std::thread::sleep(Duration::from_millis(delay));

    let result = capture_and_show(video);
    let _ = std::fs::remove_file(&sock);
    if let Err(err) = &result
        && let Ok(n) = capture::Notifier::new()
    {
        let _ = n.notify(
            "dialog-error",
            "Скриншот не удался",
            &format!("{err:#}"),
            &[],
            false,
        );
    }
    result
}

fn capture_and_show(video: bool) -> Result<()> {
    let shot = capture::screenshot()?;
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("shotvibe");
    std::fs::create_dir_all(&dir)?;
    let screen = dir.join("screen.png");
    if std::fs::rename(&shot, &screen).is_err() {
        std::fs::copy(&shot, &screen)?;
        let _ = std::fs::remove_file(&shot);
    }
    overlay::run(screen, video)
}

fn install() -> Result<()> {
    let home = dirs::home_dir().expect("no home dir");
    let bin_dir = home.join(".local/bin");
    std::fs::create_dir_all(&bin_dir)?;
    let bin = bin_dir.join("shotvibe");
    let exe = std::env::current_exe()?;
    if exe != bin {
        let tmp = bin.with_extension("new");
        std::fs::copy(&exe, &tmp)?;
        std::fs::rename(&tmp, &bin)?;
    }
    let bin = bin.display().to_string();
    println!("✓ бинарник: {bin}");

    let apps = home.join(".local/share/applications");
    std::fs::create_dir_all(&apps)?;
    std::fs::write(
        apps.join("shotvibe.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=Shotvibe\nComment=Скриншот области\n\
             Exec={bin}\nIcon=applets-screenshooter\nTerminal=false\nCategories=Utility;Graphics;\n\
             Actions=video;\n\n[Desktop Action video]\nName=Записать видео\nExec={bin} --video\n"
        ),
    )?;
    println!("✓ ярлык в меню приложений");

    let gs = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("gsettings")
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    // Print belongs to GNOME's screenshot UI by default: move it to Super+Print.
    let shell = "org.gnome.shell.keybindings";
    if gs(&["get", shell, "show-screenshot-ui"]).is_some_and(|v| v.contains("'Print'")) {
        gs(&["set", shell, "show-screenshot-ui", "['<Super>Print']"]);
        println!("✓ скриншотер GNOME перенесён на Super+Print");
    }
    let base = "org.gnome.settings-daemon.plugins.media-keys";
    let bindings = [
        (
            "shotvibe",
            "Shotvibe — скриншот",
            bin.clone(),
            "Print",
            "Print — скриншот",
        ),
        (
            "shotvibe-video",
            "Shotvibe — запись экрана",
            format!("{bin} --video"),
            "<Super><Shift>r",
            "Super+Shift+R — запись (повторно — стоп)",
        ),
    ];
    for (id, name, command, binding, label) in bindings {
        let path =
            format!("/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/{id}/");
        let Some(list) = gs(&["get", base, "custom-keybindings"]) else {
            break;
        };
        if !list.contains(&format!("'{path}'")) {
            let new_list = if list.contains("[]") {
                format!("['{path}']")
            } else {
                list.trim_end_matches(']').to_string() + &format!(", '{path}']")
            };
            gs(&["set", base, "custom-keybindings", &new_list]);
        }
        let schema = format!("{base}.custom-keybinding:{path}");
        gs(&["set", &schema, "name", name]);
        gs(&["set", &schema, "command", &command]);
        gs(&["set", &schema, "binding", binding]);
        println!("✓ хоткей GNOME: {label}");
    }
    Ok(())
}
