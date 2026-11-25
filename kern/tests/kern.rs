#![allow(clippy::type_complexity)]
use kern::copy::{self, quiet};
use kern::engine::*;
use kern::names::*;
use kern::sources::{self, Episodes, Hit, Online, Source};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

static HOME: LazyLock<TempDir> = LazyLock::new(|| {
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("SPOOL_HOME", d.path());
    d
});

// ---------------------------------------------------------------- Namen

fn p(s: &str) -> Media {
    parse(Path::new(s))
}

#[test]
fn episodes() {
    let cases: &[(&str, &str, Option<i32>, u32, &[u32])] = &[
        ("x/DE - Nordlicht Zero (2026) (DE) - S01E08 - - S01E08 - Episode 8.mkv", "Nordlicht Zero", Some(2026), 1, &[8]),
        ("x/The.Pitt.S01E05.1080p.WEB.mkv", "The Pitt", None, 1, &[5]),
        ("x/Boardwalk Empire 03x10.mkv", "Boardwalk Empire", None, 3, &[10]),
        ("x/show.s06e23e24.mkv", "show", None, 6, &[23, 24]),
        ("Staffel 2/Serie Folge 7.mp4", "Serie", None, 2, &[7]),
        ("Mein Urlaub/Season 02/S02E03.mkv", "Mein Urlaub", None, 2, &[3]),
    ];
    for (f, t, y, s, e) in cases {
        let m = p(f);
        assert_eq!((m.kind, m.title.as_str(), m.year, m.season, m.episodes.as_slice()), (Kind::Tv, *t, *y, Some(*s), *e), "{f}");
    }
    assert_eq!(p("DE - A (2026) - S01E06 - - S01E06 - Devil.mkv").ep_title, "Devil");
    assert_eq!(p("A.S01E06.1080p.mkv").ep_title, "");
}

#[test]
fn unknown_is_not_guessed() {
    for f in ["Serie Folge 7.mp4", "irgendwas.mp4", "C/MEGAsync/S01E01.mkv"] {
        assert_eq!(p(f).kind, Kind::Unknown, "{f}");
    }
}

#[test]
fn films() {
    for (f, t, y) in [
        ("Old.2021.1080p.mkv", "Old", 2021),
        ("2001.A.Space.Odyssey.1968.1080p.mkv", "2001 A Space Odyssey", 1968),
        ("1917 (2019).mkv", "1917", 2019),
        ("12.Years.a.Slave.2013.mkv", "12 Years a Slave", 2013),
        ("01.Mein.Film.2001.mkv", "Mein Film", 2001),
        ("Blade Runner 2049 (2017).mkv", "Blade Runner 2049", 2017),
        ("Avatar_ Fire and Ash (2025) (2025).mkv", "Avatar - Fire and Ash", 2025),
        ("Größe über Äpfel (2020).mkv", "Größe über Äpfel", 2020),
    ] {
        let m = p(&format!("x/{f}"));
        assert_eq!((m.kind, m.title.as_str(), m.year), (Kind::Movie, t, Some(y)), "{f}");
    }
}

#[test]
fn helpers() {
    assert_eq!(safe_name("A: B? \"C\"/.."), "A - B C");
    assert_eq!(safe_name("con"), "con_");
    assert_eq!(norm("Shōgun & Größe: Teil’s"), "shogun and grosse teils");
    assert_eq!(split_folder("Film (2020) {tmdb-12}"), ("Film".into(), Some(2020)));
    assert!((ratio("nordlicht zero", "nordlicht") - 0.7826).abs() < 0.001);
    assert_eq!(ratio("abc", "abc"), 1.0);
}

// ---------------------------------------------------------------- Abgleich

struct Fake {
    id: String,
    hits: Vec<Hit>,
    eps: Vec<(&'static str, Vec<(u32, u32, &'static str)>)>,
    boom: Option<&'static str>,
    late: bool,
    calls: AtomicU32,
}

impl Source for Fake {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.id
    }
    fn fallback(&self) -> bool {
        self.late
    }
    fn search(&self, kind: Kind, _: &str, _: &str) -> Result<Vec<Hit>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.boom {
            Some("panic") => panic!("kaputt"),
            Some(e) => Err(e.into()),
            None => Ok(self.hits.iter().filter(|h| h.kind == kind).cloned().collect()),
        }
    }
    fn episodes(&self, hit: &Hit, _: &BTreeSet<u32>, _: &str) -> Result<Episodes, String> {
        let list = self.eps.iter().find(|e| e.0 == hit.id).map(|e| e.1.clone()).unwrap_or_default();
        Ok(list.into_iter().map(|(s, e, t)| ((s, e), t.to_string())).collect())
    }
}

fn fake(id: &str, hits: Vec<Hit>, eps: Vec<(&'static str, Vec<(u32, u32, &'static str)>)>) -> Fake {
    Fake { id: id.into(), hits, eps, boom: None, late: false, calls: AtomicU32::new(0) }
}

fn tv(src: &str, id: &str, title: &str, year: i32) -> Hit {
    Hit::new(src, id, Kind::Tv, title, Some(year))
}

struct World {
    _t: TempDir,
    src: PathBuf,
    tv: PathBuf,
    mv: PathBuf,
    cfg: Settings,
}

fn world() -> World {
    LazyLock::force(&HOME);
    let t = tempfile::tempdir().unwrap();
    let [src, tv, mv] = ["src", "tv", "mv"].map(|d| t.path().join(d));
    for d in [&src, &tv, &mv] {
        fs::create_dir(d).unwrap();
    }
    let cfg = Settings { tv: tv.to_string_lossy().into(), movies: mv.to_string_lossy().into(), ..Default::default() };
    World { _t: t, src, tv, mv, cfg }
}

impl World {
    fn file(&self, name: &str, data: &[u8], age: u64) -> PathBuf {
        let f = self.src.join(name);
        fs::write(&f, data).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age);
        fs::OpenOptions::new().write(true).open(&f).unwrap().set_modified(t).unwrap();
        f
    }
    fn row(&self, name: &str) -> Row {
        Row::new(&self.file(name, &[b'x'; 50], 3600)).unwrap()
    }
    fn run(&self, rows: &mut [Row], sources: Vec<Fake>) -> Finder {
        self.run_arc(rows, sources.into_iter().map(|s| Arc::new(s) as Arc<dyn Source>).collect())
    }
    fn run_arc(&self, rows: &mut [Row], sources: Vec<Arc<dyn Source>>) -> Finder {
        let f = Finder::new(sources, "en", &HashMap::new());
        match_rows(rows, &self.cfg, &f, &|_, _, _| {}, &|_| {});
        f
    }
    fn one(&self, name: &str, sources: Vec<Fake>) -> Row {
        let mut r = [self.row(name)];
        self.run(&mut r, sources);
        let [r] = r;
        r
    }
    fn rel(&self, r: &Row) -> String {
        let d = r.dst.as_ref().unwrap();
        d.strip_prefix(&self.tv).or(d.strip_prefix(&self.mv)).unwrap().to_string_lossy().replace('\\', "/")
    }
}

fn silo() -> Fake {
    fake("a", vec![tv("a", "1", "Silo", 2023)], vec![("1", (1..9).map(|n| (1, n, "x")).collect())])
}

#[test]
fn merges_sources_and_names_episode() {
    let w = world();
    let id = || vec![("imdb".to_string(), "tt1".to_string())];
    let a = fake("a", vec![Hit { ids: id(), ..tv("a", "1", "Silo", 2023) }], vec![("1", vec![(1, 3, "Machines")])]);
    let b = fake("b", vec![Hit { ids: id(), ..tv("b", "9", "Silo - Das Letzte", 2023) }], vec![]);
    let r = w.one("Silo.S01E03.mkv", vec![a, b]);
    assert_eq!((r.status, r.checked, r.cand().unwrap().sources()), (Status::Safe, true, vec!["a", "b"]));
    assert_eq!(w.rel(&r), "Silo (2023)/Season 01/Silo (2023) - S01E03 - Machines.mkv");
}

#[test]
fn similar_title_wrong_year_is_never_safe() {
    let w = world();
    let a = fake("a", vec![tv("a", "1", "Nordlicht", 2017)], vec![("1", vec![(1, 1, "Brüder")])]);
    let r = w.one("Nordlicht Zero (2026) - S01E01 - Episode 1.mkv", vec![a]);
    assert_eq!((r.status, r.checked), (Status::Local, false));
    assert!(w.rel(&r).starts_with("Nordlicht Zero (2026)/"));
}

#[test]
fn ambiguous_name_needs_review_unless_library_decides() {
    let w = world();
    let a = || fake("a", vec![tv("a", "1", "Utopia", 2013), tv("a", "2", "Utopia", 2020)], vec![("1", vec![(1, 1, "x")]), ("2", vec![(1, 1, "y")])]);
    let r = w.one("Utopia.S01E01.mkv", vec![a()]);
    assert_eq!((r.status, r.checked), (Status::Check, false));
    fs::create_dir(w.tv.join("Utopia (2020)")).unwrap();
    let r = w.one("Utopia.S01E01.mkv", vec![a()]);
    assert_eq!((r.status, r.cand().unwrap().year()), (Status::Safe, Some(2020)));
}

#[test]
fn unknown_episode_and_missing_season_need_review() {
    let w = world();
    assert_eq!(w.one("Silo.S01E99.mkv", vec![silo()]).status, Status::Check);
    let a = fake("a", vec![tv("a", "1", "Mini", 2023)], vec![("1", vec![(1, 3, "Drei")])]);
    let r = w.one("Mini Folge 3.mp4", vec![a]);
    assert_eq!((r.status, w.rel(&r)), (Status::Check, "Mini (2023)/Season 01/Mini (2023) - S01E03 - Drei.mp4".into()));
}

#[test]
fn chosen_title_does_not_hide_missing_episode() {
    let w = world();
    let mut r = w.one("Silo.S01E99.mkv", vec![silo()]);
    let no = HashMap::new();
    let again = |r: &mut Row| resolve(r, &w.cfg, &Library::new(&w.tv, &no), &Library::new(&w.mv, &no));
    r.approved = true;
    again(&mut r);
    assert_eq!((r.status, r.checked), (Status::Check, false));
    r.confirmed = true;
    again(&mut r);
    assert_eq!((r.status, r.checked), (Status::Safe, true));
}

#[test]
fn learns_only_confirmed_foreign_titles() {
    let w = world();
    fs::create_dir(w.tv.join("Berlin ER")).unwrap();
    let h = Hit { titles: vec![("de".into(), "KRANK Berlin".into()), ("en".into(), "Berlin ER".into())], ..tv("a", "1", "Berlin ER", 2025) };
    let mut r = w.one("KRANK Berlin (2025) - S01E01.mkv", vec![fake("a", vec![h], vec![("1", vec![(1, 1, "x")])])]);
    assert_eq!(r.folder, "Berlin ER");
    assert_eq!(learned(&r), None);
    r.approved = true;
    let got = learned(&r).unwrap();
    assert_eq!((got.0.as_str(), got.1.folder.as_str()), ("krank berlin", "Berlin ER"));
    assert!(got.1.ids.contains(&("a".into(), "1".into())));
    r.folder = "KRANK Berlin (2025)".into();
    assert_eq!(learned(&r), None);
}

#[test]
fn plex_folder_is_checked_and_detected() {
    use kern::library;
    let w = world();
    let root = w.tv.parent().unwrap();
    for d in ["Serien", "Movies", "Musik"] {
        fs::create_dir(root.join(d)).unwrap();
    }
    let f = library::inside(root);
    assert_eq!((f.tv, f.movies, f.music), (Some(root.join("Serien")), Some(root.join("Movies")), Some(root.join("Musik"))));
    let t = root.join("Serien").to_string_lossy().into_owned();
    assert_eq!(library::check(&format!("  \"{}\\\" ", t.replace('\\', "/"))).unwrap(), t);
    assert!(library::check("Filme").unwrap_err().contains("vollständigen"));
    assert!(library::check(&format!("{t}\\weg")).unwrap_err().contains("nicht erreichbar"));
    assert!(library::check("").is_err());
}

#[test]
fn german_title_finds_english_library_folder() {
    let w = world();
    fs::create_dir(w.mv.join("System Crasher (2019)")).unwrap();
    let titles = vec![("de".into(), "Systemsprenger".into()), ("en".into(), "System Crasher".into())];
    let h = Hit { titles, ..Hit::new("a", "1", Kind::Movie, "System Crasher", Some(2019)) };
    let r = w.one("Systemsprenger 2019.mp4", vec![fake("a", vec![h], vec![])]);
    assert_eq!((r.status, w.rel(&r)), (Status::Safe, "System Crasher (2019)/System Crasher (2019).mp4".into()));
}

#[test]
fn movie_without_year_needs_approval() {
    let w = world();
    let a = fake("a", vec![Hit::new("a", "1", Kind::Movie, "Der Killer", Some(2023))], vec![]);
    let mut r = w.one("Der Killer.mp4", vec![a]);
    assert_eq!((r.status, r.checked), (Status::Check, false));
    r.approved = true;
    let no = HashMap::new();
    resolve(&mut r, &w.cfg, &Library::new(&w.tv, &no), &Library::new(&w.mv, &no));
    assert_eq!((r.status, r.checked), (Status::Safe, true));
}

#[test]
fn broken_sources_do_not_stop_others() {
    let w = world();
    let mut rows = [w.row("Silo.S01E01.mkv")];
    let bad = Fake { boom: Some("weg"), ..fake("bad", vec![], vec![]) };
    let worse = Fake { boom: Some("panic"), ..fake("worse", vec![], vec![]) };
    let f = w.run(&mut rows, vec![bad, worse, silo()]);
    assert_eq!((rows[0].status, f.seen_errors().as_str()), (Status::Check, "weg"));
}

#[test]
fn outage_is_reported_as_outage() {
    let w = world();
    let r = w.one("Silo.S01E01.mkv", vec![Fake { boom: Some("weg"), ..fake("bad", vec![], vec![]) }]);
    assert_eq!((r.status, r.note.as_str()), (Status::Local, "weg"));
}

#[test]
fn outage_never_creates_new_folder_unasked() {
    let w = world();
    let bad = || Fake { boom: Some("weg"), ..fake("bad", vec![], vec![]) };
    let r = w.one("Silo.S01E01.mkv", vec![bad(), silo()]);
    assert_eq!((r.status, r.checked), (Status::Check, false));
    fs::create_dir(w.tv.join("Silo")).unwrap();
    assert_eq!(w.one("Silo.S01E01.mkv", vec![bad(), silo()]).status, Status::Safe);
}

#[test]
fn wikidata_batch_serves_many_titles_with_one_request() {
    let (base, log) = server(vec![
        ("/sparql", 200, r#"{"results":{"bindings":[
          {"kind":{"value":"movie"},"item":{"value":"http://www.wikidata.org/entity/Q1"},"tmdb":{"value":"5"},"year":{"value":"2019"},"de":{"value":"Systemsprenger"},"en":{"value":"System Crasher"},"hits":{"value":"Systemsprenger"},"art":{"value":"System Crasher"}},
          {"kind":{"value":"tv"},"item":{"value":"x/Q2"},"tmdb":{"value":"6"},"year":{"value":"2025"},"de":{"value":"KRANK Berlin"},"en":{"value":"Berlin ER"},"hits":{"value":"KRANK Berlin"}}]}}"#),
        ("/w/api.php", 200, r#"{"query":{"pages":{"1":{"title":"System Crasher","thumbnail":{"source":"https://x/p.jpg?utm=1"}}}}}"#),
    ]);
    sources::WIKI_POSTERS.store(true, Ordering::SeqCst);
    let s = Online::with_base("wikidata", &base);
    s.prefetch(&[(Kind::Movie, "Systemsprenger".into()), (Kind::Tv, "KRANK Berlin".into())]);
    let sent = log.lock().unwrap().len();
    let a = &s.search(Kind::Movie, "systemsprenger", "en").unwrap()[0];
    assert_eq!((a.title.as_str(), a.year, a.poster.as_str(), a.id.as_str()), ("System Crasher", Some(2019), "https://x/p.jpg", "Q1"));
    assert_eq!(s.search(Kind::Tv, "KRANK Berlin", "en").unwrap()[0].title, "Berlin ER");
    assert_eq!((sent, log.lock().unwrap().len()), (2, 2));
}

#[test]
fn fallback_only_when_needed() {
    let w = world();
    let late = Arc::new(Fake { late: true, ..fake("late", vec![], vec![]) });
    w.run_arc(&mut [w.row("Silo.S01E01.mkv")], vec![Arc::new(silo()), late.clone()]);
    assert_eq!(late.calls.load(Ordering::SeqCst), 0);
    w.run_arc(&mut [w.row("Anderes.S01E01.mkv")], vec![Arc::new(silo()), late.clone()]);
    assert!(late.calls.load(Ordering::SeqCst) > 0);
}

#[test]
fn existing_conflict_busy_duplicate() {
    let w = world();
    let s = w.tv.join("Silo/Season 1");
    fs::create_dir_all(&s).unwrap();
    fs::write(s.join("Silo - S01E01 - a.mkv"), [b'x'; 50]).unwrap();
    fs::write(s.join("Silo - S01E02 - a.mkv"), [b'x'; 9]).unwrap();
    let mut rows = vec![w.row("Silo.S01E01.mkv"), w.row("Silo.S01E02.mkv")];
    rows.push(Row::new(&w.file("Silo.S01E03.mkv", b"x", 3)).unwrap());
    rows.extend([w.row("Silo.S01E04.mkv"), w.row("Silo.S01E04.1080p.mp4")]);
    w.run(&mut rows, vec![silo()]);
    let got: Vec<_> = rows.iter().map(|r| r.status).collect();
    assert_eq!(got, [Status::Exists, Status::Conflict, Status::Busy, Status::Safe, Status::Conflict]);
    assert_eq!(w.rel(&rows[3]), "Silo/Season 1/Silo - S01E04 - x.mkv");
}

#[test]
fn braces_in_library_folder_are_kept() {
    let w = world();
    fs::create_dir(w.tv.join("Silo (2023) {tvdb-1}")).unwrap();
    let r = w.one("Silo.S01E02.mkv", vec![silo()]);
    assert_eq!(w.rel(&r), "Silo (2023) {tvdb-1}/Season 01/Silo (2023) {tvdb-1} - S01E02 - x.mkv");
}

#[test]
fn formats() {
    let mk = |n: &str, t: &str| -> Values {
        HashMap::from([
            ("n", n.into()), ("y", "".into()), ("t", t.into()), ("s00e00", "S01E01".into()), ("folder", "A".into()),
            ("plex_format", "{folder}/{folder} - {s00e00} - {t}".into()),
        ])
    };
    let r = |f: &str, v: &Values| render(f, v).map(|p| p.to_string_lossy().replace('\\', "/"));
    let v = mk("A", "");
    assert_eq!(r("{n} ({y})/{s00e00} - {t}", &v).unwrap(), "A/S01E01");
    assert_eq!(r("{plex}", &v).unwrap(), "A/A - S01E01");
    assert_eq!(r("../../x/{n}", &v).unwrap(), "x/A");
    assert_eq!(r("{n}", &mk("..\\..\\weg", "")).unwrap(), "weg");
    assert!(!r("C:/x/{n}", &v).unwrap().contains(':'));
    assert!(r("{quatsch}", &v).is_err());
    assert!(r("//", &v).is_err());
}

// ---------------------------------------------------------------- Kopieren

fn noise(n: usize) -> Vec<u8> {
    let mut x = 0x9E3779B97F4A7C15u64;
    (0..n).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8 }).collect()
}

fn verified(a: &Path, b: &Path) -> std::io::Result<String> {
    copy::copy_verified(&copy::hold(a)?, b, &quiet, 2, Duration::ZERO)
}

#[test]
fn copy_resume_and_corrupt_partial() {
    let w = world();
    let data = noise(9_000_001);
    let a = w.file("a.mkv", &data, 100);
    let b = w.mv.join("neu/b.mkv");
    verified(&a, &b).unwrap();
    assert!(fs::read(&b).unwrap() == data);
    assert!(fs::metadata(&b).unwrap().modified().unwrap() == fs::metadata(&a).unwrap().modified().unwrap());
    for start in [data[..5_000_003].to_vec(), vec![0; 5_000_003], vec![7; 9_900_000]] {
        fs::remove_file(&b).unwrap();
        fs::write(copy::partial(&b), start).unwrap();
        verified(&a, &b).unwrap();
        assert!(fs::read(&b).unwrap() == data && !copy::partial(&b).exists());
    }
}

#[test]
fn copy_never_overwrites() {
    let w = world();
    let a = w.file("a.mkv", b"neu", 100);
    let b = w.mv.join("b.mkv");
    fs::write(&b, b"alt").unwrap();
    assert!(verified(&a, &b).is_err());
    fs::write(w.mv.join("c"), b"x").unwrap();
    assert!(copy::rename_new(&w.mv.join("c"), &b).is_err());
    assert_eq!(fs::read(&b).unwrap(), b"alt");
}

#[test]
fn long_and_umlaut_paths() {
    let w = world();
    let a = w.file("a.mkv", b"hallo", 100);
    let b = w.mv.join("ä".repeat(100)).join("ö".repeat(100)).join(format!("{}.mkv", "ü".repeat(100)));
    verified(&a, &b).unwrap();
    assert_eq!(fs::read(&b).unwrap(), b"hallo");
}

fn film(w: &World) -> Row {
    let f = w.file("Film 2020.mkv", &noise(5000), 3600);
    fs::write(w.src.join("Film 2020.de.srt"), "hi").unwrap();
    let mut r = [Row::new(&f).unwrap()];
    w.run(&mut r, vec![fake("a", vec![Hit::new("a", "1", Kind::Movie, "Film", Some(2020))], vec![])]);
    let [r] = r;
    assert_eq!(r.status, Status::Safe);
    r
}

#[test]
fn execute_copy_move_and_undo() {
    let w = world();
    let mut log: Vec<Entry> = vec![];
    let mut r = film(&w);
    let (dst, data) = (r.dst.clone().unwrap(), fs::read(&r.src).unwrap());
    execute(&mut r, "copy", &mut |e| { log.push(e); Ok(()) }, &quiet).unwrap();
    assert!(r.src.exists() && dst.exists() && dst.with_extension("de.srt").exists());
    assert_eq!((r.status, log.len()), (Status::Done, 2));
    for e in std::mem::take(&mut log) {
        undo(&e, &w.cfg, &mut |_| Ok(()), &quiet).unwrap();
    }
    assert!(!dst.exists() && w.mv.exists() && r.src.exists());

    let mut r = film(&w);
    execute(&mut r, "move", &mut |e| { log.push(e); Ok(()) }, &quiet).unwrap();
    assert!(!r.src.exists() && fs::read(&dst).unwrap() == data);
    undo(&log[0], &w.cfg, &mut |_| Ok(()), &quiet).unwrap();
    assert!(fs::read(&r.src).unwrap() == data && !dst.exists());
}

#[test]
fn move_keeps_source_when_copy_fails_and_undo_refuses_changed_target() {
    let w = world();
    let mut r = film(&w);
    let dst = r.dst.clone().unwrap();
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::write(&dst, b"inzwischen da").unwrap();
    assert!(execute(&mut r, "move", &mut |_| Ok(()), &quiet).is_err());
    assert!(r.src.exists() && fs::read(&dst).unwrap() == b"inzwischen da");
    fs::remove_file(&dst).unwrap();
    let mut log = vec![];
    execute(&mut r, "copy", &mut |e: Entry| { log.push(e); Ok(()) }, &quiet).unwrap();
    fs::write(&dst, b"anders").unwrap();
    assert!(undo(&log[0], &w.cfg, &mut |_| Ok(()), &quiet).is_err() && dst.exists());
}

// ---------------------------------------------------------------- Dienste mit Schluessel (nachgebauter Server)

type Log = Arc<Mutex<Vec<(String, String, String)>>>; // Pfad+Query, Kopfzeilen, Inhalt

fn server(replies: Vec<(&'static str, u16, &'static str)>) -> (String, Log) {
    LazyLock::force(&HOME);
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let log: Log = Default::default();
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
            let path = url.split('?').next().unwrap();
            let (code, data) = replies.iter().find(|x| x.0 == path).map_or((404, "{}"), |x| (x.1, x.2));
            seen.lock().unwrap().push((url.clone(), head, String::from_utf8(body).unwrap()));
            let _ = write!(&s, "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}", data.len());
        }
    });
    (base, log)
}

#[test]
fn tmdb() {
    let (base, log) = server(vec![
        ("/search/tv", 200, r#"{"results":[{"id":7,"name":"Silo","original_name":"Silo","first_air_date":"2023-05-04","poster_path":"/p.jpg"}]}"#),
        ("/tv/7/season/1", 200, r#"{"episodes":[{"season_number":1,"episode_number":3,"name":"Maschinen"}]}"#),
    ]);
    let s = Online::with_base("tmdb", &base);
    s.set_key("geheim");
    let h = &s.search(Kind::Tv, "Silo & Söhne", "de").unwrap()[0];
    assert_eq!((h.title.as_str(), h.year), ("Silo", Some(2023)));
    assert_eq!(s.episodes(h, &BTreeSet::from([1]), "de").unwrap()[&(1, 3)], "Maschinen");
    assert!(log.lock().unwrap()[0].0.contains("api_key=geheim") && log.lock().unwrap()[0].0.contains("Silo%20%26%20S%C3%B6hne"));
    for f in fs::read_dir(sources::DATA.join("cache")).unwrap().flatten() {
        assert!(!fs::read_to_string(f.path()).unwrap_or_default().contains("geheim"));
    }
    s.set_key("eyJtoken");
    s.search(Kind::Tv, "Anderes", "de").unwrap();
    let last = log.lock().unwrap().last().unwrap().clone();
    assert!(last.1.contains("Bearer eyJtoken") && !last.0.contains("api_key"));
}

#[test]
fn tvdb_and_omdb() {
    let (base, log) = server(vec![
        ("/login", 200, r#"{"data":{"token":"T"}}"#),
        ("/search", 200, r#"{"data":[{"tvdb_id":"5","name":"Der Pass","year":"2019","translations":{"eng":"Pagan Peak","deu":"Der Pass"}}]}"#),
        ("/series/5/episodes/default/eng", 200, r#"{"data":{"episodes":[{"seasonNumber":1,"number":2,"name":"Red"}]},"links":{"next":null}}"#),
        ("/", 200, r#"{"Search":[{"imdbID":"tt1","Title":"Old","Year":"2021","Poster":"N/A"}]}"#),
    ]);
    let s = Online::with_base("tvdb", &base);
    s.set_key("k:1234");
    let h = &s.search(Kind::Tv, "Der Pass", "en").unwrap()[0];
    assert_eq!((h.title.as_str(), h.ids[0].1.as_str()), ("Pagan Peak", "5"));
    assert_eq!(s.episodes(h, &BTreeSet::from([1]), "en").unwrap()[&(1, 2)], "Red");
    let l = log.lock().unwrap().clone();
    assert_eq!(l.iter().filter(|x| x.0 == "/login").count(), 1);
    assert_eq!(l[0].2, r#"{"apikey":"k","pin":"1234"}"#);
    let o = Online::with_base("omdb", &base);
    o.set_key("k");
    let h = &o.search(Kind::Movie, "Old", "en").unwrap()[0];
    assert_eq!((h.id.as_str(), h.year, h.poster.as_str()), ("tt1", Some(2021), ""));
}

#[test]
fn errors_are_not_cached_and_hide_secrets() {
    let (base, log) = server(vec![("/x", 200, r#"{"error":{"code":"ratelimited"}}"#), ("/y", 401, "{}")]);
    let get = |p: &str| sources::fetch(sources::Req { tries: 1, secret: Some(("k", "geheim".into())), ..sources::Req::get(format!("{base}{p}")) });
    assert!(get("/x").is_err() && get("/x").is_err());
    assert_eq!(log.lock().unwrap().len(), 2);
    let e = get("/y").unwrap_err();
    assert!(e.contains("ungültig") && !e.contains("geheim"));
}

#[cfg(windows)]
#[test]
fn keys_are_encrypted() {
    LazyLock::force(&HOME);
    let keys = HashMap::from([("tmdb".to_string(), "geheim123".to_string()), ("omdb".to_string(), String::new())]);
    sources::save_keys(&keys).unwrap();
    let raw = fs::read(sources::DATA.join("keys.bin")).unwrap();
    assert!(!raw.windows(9).any(|w| w == b"geheim123"));
    assert_eq!(sources::load_keys(), HashMap::from([("tmdb".to_string(), "geheim123".to_string())]));
}
