//! Talking to GNOME over D-Bus: screenshots through the xdg portal (no dialog
//! for non-interactive shots), screen recording through GNOME Shell's own
//! screencast service, and desktop notifications.
//!
//! The screencast is bound to the D-Bus connection that started it — if the
//! connection goes away, GNOME stops recording — so one `Screencast` must live
//! for the whole recording.

use anyhow::{Context as _, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

/// Grabs the whole screen. Returns the PNG GNOME wrote (the caller owns it).
pub fn screenshot() -> Result<PathBuf> {
    let conn = Connection::session()?;
    let sender = conn
        .unique_name()
        .context("no unique bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let token = format!("shotvibe{}", std::process::id());
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    // Subscribe before calling, otherwise a fast portal could answer first.
    let request = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut responses = request.receive_signal("Response")?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("interactive", Value::from(false));
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Screenshot"),
        "Screenshot",
        &("", options),
    )?;

    let reply = responses.next().context("portal closed the request")?;
    let (code, mut results): (u32, HashMap<String, OwnedValue>) = reply.body().deserialize()?;
    if code != 0 {
        bail!("скриншот отменён (код {code})");
    }
    let uri = String::try_from(results.remove("uri").context("portal returned no uri")?)?;
    Ok(PathBuf::from(percent_decode(
        uri.strip_prefix("file://").unwrap_or(&uri),
    )))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Screencast {
    conn: Connection,
}

impl Screencast {
    /// Starts recording the given area (logical screen coordinates).
    /// Returns the file GNOME actually writes to.
    pub fn start(
        (x, y, w, h): (i32, i32, i32, i32),
        file: &Path,
        cursor: bool,
    ) -> Result<(Self, PathBuf)> {
        let conn = Connection::session()?;
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("draw-cursor", Value::from(cursor));
        options.insert("framerate", Value::from(30i32));
        let template = file.to_string_lossy().to_string();
        let reply = conn.call_method(
            Some("org.gnome.Shell.Screencast"),
            "/org/gnome/Shell/Screencast",
            Some("org.gnome.Shell.Screencast"),
            "ScreencastArea",
            &(
                x,
                y,
                w.max(2) & !1,
                h.max(2) & !1,
                template.as_str(),
                options,
            ),
        )?;
        let (ok, used): (bool, String) = reply.body().deserialize()?;
        if !ok {
            bail!("GNOME отказался начинать запись (уже идёт другая?)");
        }
        Ok((Self { conn }, PathBuf::from(used)))
    }

    pub fn stop(&self) -> Result<()> {
        self.conn.call_method(
            Some("org.gnome.Shell.Screencast"),
            "/org/gnome/Shell/Screencast",
            Some("org.gnome.Shell.Screencast"),
            "StopScreencast",
            &(),
        )?;
        Ok(())
    }
}

pub struct Notifier {
    conn: Connection,
}

const NOTIFY_DEST: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";

impl Notifier {
    pub fn new() -> Result<Self> {
        Ok(Self {
            conn: Connection::session()?,
        })
    }

    /// `actions` are `(key, label)` pairs. A `persistent` notification stays
    /// until closed and survives clicking its actions.
    pub fn notify(
        &self,
        icon: &str,
        summary: &str,
        body: &str,
        actions: &[(&str, &str)],
        persistent: bool,
    ) -> Result<u32> {
        let actions: Vec<&str> = actions.iter().flat_map(|(k, l)| [*k, *l]).collect();
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from("shotvibe"));
        if persistent {
            hints.insert("resident", Value::from(true));
            hints.insert("urgency", Value::from(2u8));
        }
        let reply = self.conn.call_method(
            Some(NOTIFY_DEST),
            NOTIFY_PATH,
            Some(NOTIFY_DEST),
            "Notify",
            &(
                "Shotvibe",
                0u32,
                icon,
                summary,
                body,
                actions,
                hints,
                if persistent { 0i32 } else { -1i32 },
            ),
        )?;
        Ok(reply.body().deserialize()?)
    }

    pub fn close(&self, id: u32) {
        let _ = self.conn.call_method(
            Some(NOTIFY_DEST),
            NOTIFY_PATH,
            Some(NOTIFY_DEST),
            "CloseNotification",
            &(id,),
        );
    }

    /// Calls `f(notification_id, action_key)` for every clicked action, on a
    /// background thread.
    pub fn on_action(&self, f: impl Fn(u32, String) + Send + 'static) -> Result<()> {
        let conn = self.conn.clone();
        let proxy = Proxy::new(&conn, NOTIFY_DEST, NOTIFY_PATH, NOTIFY_DEST)?;
        let signals = proxy.receive_signal("ActionInvoked")?;
        std::thread::spawn(move || {
            for msg in signals {
                if let Ok((id, key)) = msg.body().deserialize::<(u32, String)>() {
                    f(id, key);
                }
            }
        });
        Ok(())
    }
}

/// Opens a file manager window with the file selected.
pub fn show_in_folder(file: &Path) {
    let uri = format!("file://{}", file.display());
    let ok = Connection::session().and_then(|conn| {
        conn.call_method(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            Some("org.freedesktop.FileManager1"),
            "ShowItems",
            &(vec![uri.as_str()], ""),
        )
    });
    if ok.is_err()
        && let Some(dir) = file.parent()
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Local time as `2026-10-03_16-30-05`.
pub fn timestamp() -> String {
    // SAFETY: plain libc time calls with valid out-pointers.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}
