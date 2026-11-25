//! Musik, Archive, Untertitel.
use kern::engine::*;
use kern::{archive, music, names, subs};
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

fn wav(p: &Path, tags: &[(ItemKey, &str)], no: Option<u32>) {
    let data = vec![0u8; 8000];
    let mut w = b"RIFF".to_vec();
    w.extend((36 + data.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    for x in [1u16, 1] {
        w.extend(x.to_le_bytes());
    }
    w.extend(8000u32.to_le_bytes());
    w.extend(8000u32.to_le_bytes());
    for x in [1u16, 8] {
        w.extend(x.to_le_bytes());
    }
    w.extend(b"data");
    w.extend((data.len() as u32).to_le_bytes());
    w.extend(data);
    fs::write(p, w).unwrap();
    if !tags.is_empty() {
        let mut t = Tag::new(TagType::Id3v2);
        for (k, v) in tags {
            t.insert_text(k.clone(), v.to_string());
        }
        if let Some(n) = no {
            t.set_track(n);
        }
        t.save_to_path(p, WriteOptions::default()).unwrap();
    }
    fs::OpenOptions::new().write(true).open(p).unwrap().set_modified(SystemTime::now() - Duration::from_secs(9000)).unwrap();
}

#[test]
fn music_goes_by_tags() {
    let t = tempfile::tempdir().unwrap();
    let (src, lib) = (t.path().join("src"), t.path().join("music"));
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(lib.join("björk").join("post")).unwrap();
    let cfg = Settings { music: lib.to_string_lossy().into(), ..Default::default() };
    let full = [(ItemKey::TrackArtist, "Björk"), (ItemKey::AlbumTitle, "Post"), (ItemKey::TrackTitle, "Army of Me: Live?")];
    let (a, b, c) = (src.join("a.wav"), src.join("b.wav"), src.join("c.wav"));
    wav(&a, &full, Some(1));
    wav(&b, &full[..1], None);
    wav(&c, &[(ItemKey::TrackArtist, "X"), (ItemKey::TrackTitle, "Y")], None);
    let go = |p: &Path| {
        let mut r = Row::new(p).unwrap();
        music::resolve(&mut r, &cfg);
        r
    };
    let r = go(&a);
    assert_eq!(r.dst.as_deref(), Some(lib.join("björk").join("post").join("01 - Army of Me - Live.wav").as_path()));
    assert_eq!((r.status, r.checked, r.kind()), (Status::Safe, true, names::Kind::Music));
    assert_eq!(go(&b).status, Status::None);
    assert_eq!(go(&c).status, Status::Check);
    let mut r = go(&a);
    execute(&mut r, "copy", &mut |_| Ok(()), &kern::copy::quiet).unwrap();
    assert_eq!(go(&a).status, Status::Exists);
    fs::write(r.dst.unwrap(), b"anders").unwrap();
    assert_eq!(go(&a).status, Status::Conflict);
    let no = Settings { music: t.path().join("weg").to_string_lossy().into(), ..Default::default() };
    let mut r = Row::new(&a).unwrap();
    music::resolve(&mut r, &no);
    assert_eq!((r.status, r.dst), (Status::None, None));
}

#[test]
fn music_only_when_asked() {
    let t = tempfile::tempdir().unwrap();
    wav(&t.path().join("a.wav"), &[], None);
    let dir = [t.path().to_path_buf()];
    assert_eq!(names::scan(&dir, 0, false).len(), 0);
    assert_eq!(names::scan(&dir, 0, true).len(), 1);
}

fn tar(args: &[&str], dir: &Path) {
    let exe = Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\tar.exe");
    assert!(Command::new(exe).args(args).current_dir(dir).status().unwrap().success());
}

#[cfg(windows)]
#[test]
fn archives_keep_only_media() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    fs::create_dir_all(d.join("in/Größe/tief")).unwrap();
    fs::write(d.join("in/Größe/Film (2020).mkv"), b"video").unwrap();
    fs::write(d.join("in/Größe/Film (2020).de.srt"), b"sub").unwrap();
    fs::write(d.join("in/böse.exe"), b"MZ").unwrap();
    fs::write(d.join("in/Größe/tief/lies.txt"), b"x").unwrap();
    for ext in ["zip", "7z"] {
        let arc = d.join(format!("paket {ext}.{ext}"));
        tar(&["-a", "-cf", arc.to_str().unwrap(), "-C", "in", "."], d);
        assert!(archive::is_archive(&arc));
        let out = archive::extract(&arc, Some(1 << 50), &|| true).unwrap();
        let found = names::scan(std::slice::from_ref(&out), 0, true);
        assert_eq!(found.len(), 1, "{ext}");
        assert_eq!(fs::read(&found[0]).unwrap(), b"video");
        assert_eq!(names::sidecars(&found[0]).len(), 1);
        assert!(!out.join("böse.exe").exists() && !out.join("Größe/tief").exists());
        assert_eq!(archive::extract(&arc, None, &|| true).unwrap(), out); // zweites Mal: nichts anfassen
    }
    assert!(archive::extract(&d.join("paket zip.zip"), Some(10), &|| true).is_ok());
}

#[cfg(windows)]
#[test]
fn bad_archives_leave_nothing() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    fs::write(d.join("kaputt.rar"), b"kein archiv").unwrap();
    fs::write(d.join("voll.zip"), b"PK").unwrap();
    fs::write(d.join("x.part1.rar"), b"R").unwrap();
    fs::create_dir(d.join("in")).unwrap();
    fs::write(d.join("in/lies.txt"), b"x").unwrap();
    tar(&["-a", "-cf", "leer.zip", "-C", "in", "."], d);
    for (n, why) in [("kaputt.rar", "nicht lesbar"), ("x.part1.rar", "mehrteilig"), ("leer.zip", "enthält keine")] {
        assert!(archive::extract(&d.join(n), None, &|| true).unwrap_err().contains(why), "{n}");
    }
    assert!(archive::extract(&d.join("voll.zip"), Some(10), &|| true).unwrap_err().contains("Platz"));
    let left: Vec<PathBuf> = fs::read_dir(d).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    assert_eq!(left, [d.join("in")]);
}

#[cfg(windows)]
#[test]
fn archive_cannot_escape_its_folder() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("a/b");
    fs::create_dir_all(&d).unwrap();
    // ZIP von Hand gebaut; die Gegenprobe mit harmlosem Namen zeigt, dass es gueltig ist
    for (file, name) in [("ok.zip", b"harmlos.mkv".as_slice()), ("x.zip", b"../../flucht.mkv".as_slice())] {
    let data = b"boese".as_slice();
    let mut crc = crc32fast::Hasher::new();
    crc.update(data);
    let crc = crc.finalize();
    let head = |sig: u32, central: bool| {
        let mut h = sig.to_le_bytes().to_vec();
        if central {
            h.extend(20u16.to_le_bytes());
        }
        h.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        h.extend(crc.to_le_bytes());
        h.extend((data.len() as u32).to_le_bytes());
        h.extend((data.len() as u32).to_le_bytes());
        h.extend((name.len() as u16).to_le_bytes());
        h.extend(0u16.to_le_bytes());
        h
    };
    let mut z = head(0x04034b50, false);
    z.extend(name);
    z.extend(data);
    let at = z.len() as u32;
    let mut c = head(0x02014b50, true);
    c.extend([0; 10]);
    c.extend(0u32.to_le_bytes());
    c.extend(name);
    let size = c.len() as u32;
    z.extend(c);
    z.extend(0x06054b50u32.to_le_bytes());
    z.extend([0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend(size.to_le_bytes());
    z.extend(at.to_le_bytes());
    z.extend([0, 0]);
    fs::write(d.join(file), z).unwrap();
    }
    assert!(archive::extract(&d.join("ok.zip"), None, &|| true).unwrap().join("harmlos.mkv").exists());
    let _ = archive::extract(&d.join("x.zip"), None, &|| true);
    for p in [t.path().join("flucht.mkv"), t.path().join("a/flucht.mkv"), d.join("flucht.mkv")] {
        assert!(!p.exists(), "{}", p.display());
    }
}

type Log = Arc<Mutex<Vec<(String, String, String)>>>;

fn server(reply: fn(&str, &str) -> (u16, String)) -> (String, Log) {
    let dir = std::env::temp_dir().join(format!("spool-test-{}", std::process::id()));
    std::env::set_var("SPOOL_HOME", dir);
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (log, b): (Log, String) = (Default::default(), base.clone());
    let seen = log.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(&s);
            let (mut first, mut head, mut len) = (String::new(), String::new(), 0);
            r.read_line(&mut first).unwrap();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) < 3 {
                    break;
                }
                if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head += &h;
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let url = first.split(' ').nth(1).unwrap().to_string();
            let (code, data) = reply(url.split('?').next().unwrap(), &b);
            seen.lock().unwrap().push((url, head, String::from_utf8(body).unwrap()));
            let _ = write!(&s, "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len());
        }
    });
    (base, log)
}

#[test]
fn subtitles_search_and_download() {
    let (base, log) = server(|path, base| match path {
        "/subtitles" => (200, r#"{"data":[
            {"attributes":{"release":"Viel","download_count":900,"moviehash_match":false,"files":[{"file_id":1}]}},
            {"attributes":{"release":"Genau","download_count":5,"moviehash_match":true,"files":[{"file_id":2}]}},
            {"attributes":{"release":"Ohne Datei","files":[]}}]}"#.into()),
        "/download" => (200, format!(r#"{{"link":"{base}/datei.srt"}}"#)),
        "/datei.srt" => (200, "1\n00:00:01,000 --> 00:00:02,000\nHallo\n".into()),
        _ => (404, "{}".into()),
    });
    let t = tempfile::tempdir().unwrap();
    let v = t.path().join("Silo (2023) - S01E03 - Machines.mkv");
    fs::write(&v, vec![1u8; 200_000]).unwrap();
    let hits = subs::search(&base, "geheim", &v, "de").unwrap();
    assert_eq!(hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["Genau", "Viel"]);
    let first = log.lock().unwrap()[0].clone();
    assert!(first.0.contains("episode_number=3&languages=de&moviehash=") && first.0.contains("&query=silo&season_number=1"), "{}", first.0);
    assert!(first.1.contains("Api-Key: geheim") && first.1.contains("Spool v") && !first.0.contains("geheim"));
    let f = subs::download(&base, "geheim", &v, 2, "de").unwrap();
    assert_eq!(f.file_name().unwrap().to_str(), Some("Silo (2023) - S01E03 - Machines.de.srt"));
    assert!(fs::read_to_string(&f).unwrap().contains("Hallo"));
    assert!(subs::download(&base, "geheim", &v, 2, "de").unwrap_err().contains("schon"));
    assert!(subs::download(&base, "geheim", &v, 2, "../x").is_err());
}

#[test]
fn subtitle_link_must_stay_at_provider() {
    let (base, log) = server(|_, _| (200, r#"{"link":"https://opensubtitles.com.evil.example/x.srt"}"#.into()));
    let t = tempfile::tempdir().unwrap();
    let v = t.path().join("a.mkv");
    fs::write(&v, b"x").unwrap();
    assert!(subs::download(&base, "k", &v, 1, "de").unwrap_err().contains("abgebrochen"));
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[test]
fn moviehash_reference() {
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("a.bin");
    fs::write(&f, vec![0u8; 131072]).unwrap();
    assert_eq!(subs::moviehash(&f).unwrap(), "0000000000020000"); // nur die Groesse
    let mut d = vec![0u8; 200_000];
    d[0] = 1;
    *d.last_mut().unwrap() = 1;
    fs::write(&f, &d).unwrap();
    let want = 200_000u64.wrapping_add(1).wrapping_add(1 << 56);
    assert_eq!(subs::moviehash(&f).unwrap(), format!("{want:016x}"));
    fs::write(&f, b"kurz").unwrap();
    assert!(subs::moviehash(&f).is_err());
}

#[cfg(windows)]
#[test]
fn archive_never_adopts_a_foreign_folder_and_can_be_cancelled() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    fs::create_dir(d.join("in")).unwrap();
    fs::write(d.join("in/Film.mkv"), b"video").unwrap();
    tar(&["-a", "-cf", "paket.zip", "-C", "in", "."], d);
    fs::create_dir(d.join("paket")).unwrap();
    fs::write(d.join("paket/Fremd.mkv"), b"anderes").unwrap();
    let out = archive::extract(&d.join("paket.zip"), None, &|| true).unwrap();
    assert_eq!(out, d.join("paket (2)"));
    assert_eq!(fs::read(out.join("Film.mkv")).unwrap(), b"video");
    let e = archive::extract(&d.join("paket.zip"), None, &|| false);
    assert!(e.is_ok()); // schon entpackt, also kein zweites Mal
    fs::copy(d.join("paket.zip"), d.join("neu.zip")).unwrap();
    let _ = archive::extract(&d.join("neu.zip"), None, &|| false);
    assert!(!d.join(".neu.entpacken").exists());
}
