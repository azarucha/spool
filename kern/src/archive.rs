//! Archive entpacken, mit dem in Windows enthaltenen tar.exe (libarchive: zip, 7z, rar).
use crate::copy::RESERVE;
use crate::music::is_audio;
use crate::names::{ext_of, is_video, stem_of, SUBS};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const ARCHIVES: &[&str] = &["zip", "7z", "rar"];

pub fn is_archive(p: &Path) -> bool {
    p.is_file() && ARCHIVES.contains(&ext_of(p).as_str())
}

fn tar() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join(r"System32\tar.exe")
}

/// Nur Videos, Musik und Untertitel behalten; alles andere (auch Programme und Verknuepfungen) loeschen.
fn sift(dir: &Path) -> Result<usize, String> {
    let mut kept = 0;
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let (p, Ok(t)) = (e.path(), e.file_type()) else { continue };
        if t.is_dir() {
            kept += sift(&p)?;
            let _ = fs::remove_dir(&p); // gelingt nur, wenn leer
        } else if t.is_file() && (is_video(&p) || is_audio(&p) || SUBS.contains(&ext_of(&p).as_str())) {
            kept += 1;
        } else {
            remove(&p)?;
        }
    }
    Ok(kept)
}

/// Loeschen ueber den ungekuerzten Pfad, damit auch Namen mit Leerzeichen am Ende verschwinden.
fn remove(p: &Path) -> Result<(), String> {
    let long = PathBuf::from(format!(r"\\?\{}", p.display()));
    let gone = fs::remove_file(&long).or_else(|_| fs::remove_dir(&long)).or_else(|_| fs::remove_file(p));
    gone.map_err(|_| format!("enthält eine Datei, die sich nicht entfernen lässt: {}", p.display()))
}

/// Entpackte Groesse laut Inhaltsverzeichnis.
fn unpacked(arc: &Path) -> Option<u64> {
    let out = quiet(Command::new(tar()).arg("-tvf").arg(arc).stdin(Stdio::null()).stderr(Stdio::null())).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    out.status.success().then(|| text.lines().filter_map(|l| l.split_whitespace().nth(4)?.parse::<u64>().ok()).sum())
}

fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // kein Konsolenfenster
    }
    cmd
}

/// Entpackt neben das Archiv und gibt den Ordner zurueck. Vorhandenes wird nicht angefasst.
pub fn extract(arc: &Path, free: Option<u64>, go_on: &dyn Fn() -> bool) -> Result<PathBuf, String> {
    let name = arc.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let lower = name.to_lowercase();
    if let Some((_, part)) = lower.trim_end_matches(".rar").rsplit_once(".part") {
        if lower.ends_with(".rar") && part.parse::<u32>().is_ok() {
            return Err(format!("{name}: mehrteilige RAR-Archive bitte vorher entpacken"));
        }
    }
    let parent = arc.parent().ok_or("Archiv ohne Ordner")?;
    let out = parent.join(stem_of(arc));
    let mark = |d: &Path| d.join(".spool-entpackt");
    let stamp = fs::metadata(arc).map_err(|e| e.to_string())?.len().to_string();
    let mut out = out;
    for n in 2.. {
        if !out.exists() {
            break;
        }
        if fs::read_to_string(mark(&out)).is_ok_and(|t| t == format!("{name} {stamp}")) {
            return Ok(out); // genau dieses Archiv ist schon entpackt
        }
        out = parent.join(format!("{} ({n})", stem_of(arc))); // fremder Ordner gleichen Namens bleibt unberuehrt
    }
    let size = unpacked(arc).unwrap_or_else(|| stamp.parse().unwrap_or(0));
    if free.is_some_and(|f| size + RESERVE > f) {
        return Err(format!("{name}: zu wenig Platz zum Entpacken"));
    }
    let tmp = parent.join(format!(".{}.entpacken", stem_of(arc)));
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| e.to_string())?;
    }
    fs::create_dir(&tmp).map_err(|e| format!("{name}: {e}"))?;
    let mut cmd = Command::new(tar());
    cmd.arg("-xf").arg(arc).arg("-C").arg(&tmp).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = quiet(&mut cmd).spawn().map_err(|e| format!("{name}: {e}"))?;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if go_on() => std::thread::sleep(std::time::Duration::from_millis(150)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&tmp);
                return Err(format!("{name}: abgebrochen"));
            }
        }
    };
    let kept = match if ok { sift(&tmp) } else { Ok(0) } {
        Ok(n) => n,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(format!("{name}: {e}"));
        }
    };
    if kept == 0 {
        let _ = fs::remove_dir_all(&tmp);
        let why = if ok { "enthält keine Videos oder Musik" } else { "nicht lesbar (Passwort, mehrteilig oder beschädigt)" };
        return Err(format!("{name}: {why}"));
    }
    fs::write(mark(&tmp), format!("{name} {stamp}")).map_err(|e| format!("{name}: {e}"))?;
    crate::copy::rename_new(&tmp, &out).map_err(|e| format!("{name}: {e}"))?;
    Ok(out)
}
