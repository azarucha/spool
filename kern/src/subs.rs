//! Untertitel von OpenSubtitles.com, mit eigenem Schluessel des Nutzers.
use crate::engine::Entry;
use crate::names::{parse, stem_of, Kind};
use crate::sources::{self, fetch, Req};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const BASE: &str = "https://api.opensubtitles.com/api/v1";
pub const KEY_URL: &str = "https://www.opensubtitles.com/consumers";
const UA: &str = concat!("Spool v", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct SubHit {
    pub file_id: u64,
    pub name: String,
    pub count: u64,
    pub exact: bool,
}

fn req(key: &str, url: String) -> Req<'static> {
    Req { header: Some(("Api-Key", key.to_string())), ua: Some(UA), gap_ms: 300, ..Req::get(url) }
}

/// OpenSubtitles-Pruefwert: Dateigroesse plus Summe der ersten und letzten 64 KiB.
pub fn moviehash(p: &Path) -> std::io::Result<String> {
    const N: u64 = 65536;
    let mut f = fs::File::open(p)?;
    let len = f.metadata()?.len();
    if len < N {
        return Err(std::io::Error::other("zu klein"));
    }
    let mut sum = len;
    for at in [0, len - N] {
        let mut buf = vec![0u8; N as usize];
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut buf)?;
        for c in buf.as_chunks::<8>().0 {
            sum = sum.wrapping_add(u64::from_le_bytes(*c));
        }
    }
    Ok(format!("{sum:016x}"))
}

pub fn search(base: &str, key: &str, video: &Path, lang: &str) -> Result<Vec<SubHit>, String> {
    let m = parse(video);
    let mut q: Vec<(&str, String)> = vec![]; // alphabetisch, so verlangt es der Dienst
    if let Some(e) = m.episodes.first().filter(|_| m.kind == Kind::Tv) {
        q.push(("episode_number", e.to_string()));
    }
    q.push(("languages", lang.to_lowercase()));
    if let Ok(h) = moviehash(video) {
        q.push(("moviehash", h));
    }
    q.push(("query", m.title.to_lowercase()));
    if let Some(s) = m.season {
        q.push(("season_number", s.to_string()));
    }
    if let Some(y) = m.year.filter(|_| m.kind == Kind::Movie) {
        q.push(("year", y.to_string()));
    }
    let pairs: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let data = fetch(req(key, format!("{base}/subtitles?{}", sources::query(&pairs))))?;
    let mut out: Vec<SubHit> = data["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let a = &r["attributes"];
            let file = &a["files"][0];
            let name = a["release"].as_str().filter(|s| !s.is_empty()).or(file["file_name"].as_str()).unwrap_or("ohne Namen");
            Some(SubHit {
                file_id: file["file_id"].as_u64()?,
                name: name.to_string(),
                count: a["download_count"].as_u64().unwrap_or(0),
                exact: a["moviehash_match"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    out.sort_by_key(|h| (!h.exact, std::cmp::Reverse(h.count)));
    out.truncate(12);
    Ok(out)
}

/// Laedt neben das Video, z. B. "Film.de.srt". Vorhandenes wird nie ueberschrieben.
pub fn download(base: &str, key: &str, video: &Path, file_id: u64, lang: &str) -> Result<PathBuf, String> {
    if lang.len() != 2 || !lang.bytes().all(|b| b.is_ascii_lowercase()) {
        return Err("Unbekannte Sprache".into());
    }
    let dst = video.with_file_name(format!("{}.{lang}.srt", stem_of(video)));
    if dst.exists() {
        return Err("Für diese Sprache liegt schon ein Untertitel daneben".into());
    }
    let r = Req { body: Some(json!({ "file_id": file_id, "sub_format": "srt" })), ttl: 0, tries: 1, ..req(key, format!("{base}/download")) };
    let data = fetch(r).map_err(|e| if e.ends_with("406") { "Tageslimit bei OpenSubtitles erreicht".into() } else { e })?;
    let link = data["link"].as_str().ok_or("Kein Download-Link erhalten")?;
    let host = link.strip_prefix("https://").and_then(|r| r.split('/').next()).unwrap_or("");
    let local_test = link.starts_with(base) && base != BASE;
    if !(host == "opensubtitles.com" || host.ends_with(".opensubtitles.com") || local_test) {
        return Err("Unerwarteter Download-Ort, abgebrochen".into());
    }
    let text = sources::download(link, UA, 5_000_000)?;
    if !String::from_utf8_lossy(&text).contains("-->") {
        return Err("Das ist keine Untertiteldatei".into());
    }
    let tmp = dst.with_extension("srt.tmp");
    let done = fs::write(&tmp, &text).and_then(|_| crate::copy::rename_new(&tmp, &dst));
    if let Err(e) = done {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(dst)
}

/// Verlaufseintrag, damit sich ein geladener Untertitel wieder entfernen laesst.
pub fn entry(f: &Path) -> Entry {
    Entry {
        zeit: crate::engine::stamp(),
        aktion: "untertitel".into(),
        quelle: String::new(),
        ziel: f.to_string_lossy().into(),
        bytes: fs::metadata(f).map(|m| m.len()).unwrap_or(0),
        sha256: crate::copy::sha256(f, &crate::copy::quiet).unwrap_or_default(),
        quellen: vec!["opensubtitles".into()],
        alias: String::new(),
    }
}

pub fn hit_json(h: &SubHit) -> Value {
    json!({ "id": h.file_id, "name": h.name, "count": h.count, "exact": h.exact })
}
