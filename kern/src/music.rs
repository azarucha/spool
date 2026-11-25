//! Musik: Ziel aus den eingebetteten Tags, Plex-Aufbau Kuenstler/Album/01 - Titel.
use crate::engine::{occupied, Row, Settings, Status};
use crate::names::{ext_of, is_dir, norm, safe_name, short, stem_of, AUDIO};
use lofty::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub year: Option<i32>,
    pub no: Option<u32>,
    pub disc: Option<u32>,
}

pub fn is_audio(p: &Path) -> bool {
    AUDIO.contains(&ext_of(p).as_str())
}

pub fn read(p: &Path) -> Option<Track> {
    let file = lofty::read_from_path(p).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    let text = |v: Option<std::borrow::Cow<'_, str>>| v.map(|s| s.trim().to_string()).unwrap_or_default();
    let album_artist = tag.get_string(&ItemKey::AlbumArtist).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let date = tag.get_string(&ItemKey::RecordingDate).or_else(|| tag.get_string(&ItemKey::Year)).unwrap_or("");
    Some(Track {
        artist: album_artist.unwrap_or_else(|| text(tag.artist())),
        album: text(tag.album()),
        title: text(tag.title()),
        year: date.get(..4).and_then(|y| y.parse().ok()),
        no: tag.track().filter(|n| *n > 0),
        disc: tag.disk().filter(|n| *n > 0),
    })
}

/// Vorhandenen Ordner gleichen Namens weiterbenutzen.
fn folder(root: &Path, name: &str) -> PathBuf {
    // Ordnerlisten kurz merken: ein Album fragt sonst je Titel dasselbe ueber das Netz ab
    type Listing = HashMap<PathBuf, (Instant, HashMap<String, String>)>;
    static SEEN: LazyLock<Mutex<Listing>> = LazyLock::new(Default::default);
    let key = norm(name);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.retain(|_, v| v.0.elapsed() < Duration::from_secs(10));
    let (_, names) = seen.entry(root.into()).or_insert_with(|| {
        let mut list: Vec<String> = fs::read_dir(root).into_iter().flatten().flatten().filter(is_dir).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        list.sort_by(|a, b| b.cmp(a));
        (Instant::now(), list.into_iter().map(|n| (norm(&n), n)).collect())
    });
    let found = names.get(&key).filter(|_| !key.is_empty()).cloned();
    root.join(found.unwrap_or_else(|| short(&safe_name(name), 150)))
}

pub fn resolve(row: &mut Row, cfg: &Settings) {
    (row.dst, row.checked, row.note, row.folder) = (None, false, String::new(), String::new());
    let root = Path::new(&cfg.music);
    let Some(t) = row.track.clone().filter(|t| !t.artist.is_empty() && !t.title.is_empty()) else {
        (row.status, row.note) = (Status::None, "Musik-Tags fehlen (Künstler oder Titel)".into());
        return;
    };
    if cfg.music.is_empty() || !root.is_dir() {
        let why = if cfg.music.is_empty() { "Musikordner noch nicht gewählt (Einstellungen)".into() } else { format!("Musikordner nicht erreichbar: {}", cfg.music) };
        (row.status, row.note) = (Status::None, why);
        return;
    }
    let album = if t.album.is_empty() { "Unbekanntes Album".into() } else { t.album.clone() };
    let dir = folder(&folder(root, &t.artist), &album);
    let disc = t.disc.filter(|d| *d > 1).map(|d| format!("{d}-")).unwrap_or_default();
    let number = t.no.map(|n| format!("{disc}{n:02} - ")).unwrap_or_default();
    let stem = match &row.manual {
        Some(m) => m.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        None => short(&safe_name(&format!("{number}{}", t.title)), 180),
    };
    let dst = dir.join(format!("{stem}.{}", ext_of(&row.src)));
    row.folder = dir.strip_prefix(root).unwrap_or(&dir).to_string_lossy().into_owned();
    let twin = fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).find(|p| is_audio(p) && stem_of(p).to_lowercase() == stem.to_lowercase());
    if let Some(p) = twin {
        let same = fs::metadata(&p).is_ok_and(|m| m.len() == row.size);
        row.status = if same { Status::Exists } else { Status::Conflict };
        row.note = if same { "liegt schon in Plex, gleiche Größe".into() } else { "anderer Titel gleichen Namens vorhanden".into() };
        row.dst = Some(p);
        return;
    }
    row.dst = Some(dst);
    if let Some(why) = occupied(row, cfg) {
        (row.status, row.note) = (Status::Busy, why);
        return;
    }
    let weak = t.album.is_empty() || t.no.is_none();
    if weak && !row.approved && !row.confirmed {
        (row.status, row.note) = (Status::Check, "Album oder Titelnummer fehlt in den Tags".into());
    } else {
        (row.status, row.checked) = (Status::Safe, true);
    }
}
