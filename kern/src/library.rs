//! Plex-Ordner finden und pruefen.
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TV: &[&str] = &["TV Shows", "Serien", "TV-Serien", "TV-Sendungen", "Series", "Shows", "TV"];
const MOVIES: &[&str] = &["Movies", "Filme", "Films"];
const MUSIC: &[&str] = &["Music", "Musik"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    pub tv: Option<PathBuf>,
    pub movies: Option<PathBuf>,
    pub music: Option<PathBuf>,
}

/// Uebliche Unterordner eines Hauptordners erkennen.
pub fn inside(root: &Path) -> Found {
    let pick = |names: &[&str]| names.iter().map(|n| root.join(n)).find(|p| p.is_dir());
    Found { tv: pick(TV), movies: pick(MOVIES), music: pick(MUSIC) }
}

/// Alle Laufwerke nach einer Plex-Bibliothek absuchen. Haengende Netzlaufwerke halten nichts auf.
pub fn search_drives(wait: Duration) -> Vec<Found> {
    let (tx, rx) = mpsc::channel();
    for d in b'C'..=b'Z' {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let root = PathBuf::from(format!("{}:\\", d as char));
            let all: Vec<Found> = [root.clone(), root.join("Plex"), root.join("Media"), root.join("Medien")].iter().map(|r| inside(r)).collect();
            let _ = tx.send((d, all));
        });
    }
    drop(tx);
    let end = Instant::now() + wait;
    let mut out = vec![];
    while let Ok((d, all)) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
        out.extend(all.into_iter().filter(|f| f.tv.is_some() && f.movies.is_some()).map(|f| (d, f)));
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.0));
    out.into_iter().map(|x| x.1).collect()
}

/// Eingetippten oder gewaehlten Pfad pruefen und vereinheitlichen.
pub fn check(text: &str) -> Result<String, String> {
    let t = text.trim().trim_matches('"').trim().replace('/', "\\");
    let t = if t.len() > 3 { t.trim_end_matches('\\').to_string() } else { t };
    let p = Path::new(&t);
    let full = std::path::absolute(p).ok().filter(|_| p.is_absolute());
    let Some(full) = full else {
        return Err("Bitte den vollständigen Pfad angeben, z. B. D:\\Plex\\Filme oder \\\\server\\plex\\Filme".into());
    };
    if !full.is_dir() {
        return Err(format!("Ordner nicht erreichbar: {t}"));
    }
    // Schreiben und Umbenennen wirklich ausprobieren, sonst scheitert erst die Uebertragung
    let name = format!(".spool-test-{}", std::process::id());
    let (a, b) = (full.join(&name), full.join(format!("{name}-2")));
    let ok = std::fs::write(&a, b"x").and_then(|_| crate::copy::rename_new(&a, &b));
    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
    if let Err(e) = ok {
        return Err(format!("In diesem Ordner darf Spool nicht schreiben ({e})"));
    }
    let s = full.to_string_lossy().into_owned();
    Ok(if s.len() > 3 { s.trim_end_matches('\\').to_string() } else { s })
}
