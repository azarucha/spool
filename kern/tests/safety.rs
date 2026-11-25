//! Regressionstests zum unabhaengigen Review: Dateisicherheit, falsche Sicherheit, Schnittstelle.
use kern::api::{Api, Dialogs};
use kern::copy::{self, quiet};
use kern::engine::*;
use kern::names::*;
use kern::sources::{Episodes, Hit, Source, Sources};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};
use tempfile::TempDir;

static HOME: LazyLock<TempDir> = LazyLock::new(|| {
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("SPOOL_HOME", d.path());
    d
});
static ONE: Mutex<()> = Mutex::new(()); // Einstellungen und Verlauf sind je Prozess einmal da

fn fresh() -> MutexGuard<'static, ()> {
    let g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    LazyLock::force(&HOME);
    for e in fs::read_dir(HOME.path()).unwrap().flatten() {
        let _ = fs::remove_file(e.path());
    }
    g
}

struct Lib {
    _t: TempDir,
    root: PathBuf,
}

impl Lib {
    fn new() -> Lib {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        for d in ["src", "tv", "mv"] {
            fs::create_dir(root.join(d)).unwrap();
        }
        Lib { _t: t, root }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
    fn file(&self, rel: &str, data: &[u8]) -> PathBuf {
        let f = self.p(rel);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, data).unwrap();
        age(&f, -3600);
        f
    }
    fn cfg(&self) -> Settings {
        Settings { tv: s(&self.p("tv")), movies: s(&self.p("mv")), min_size_mb: 0, ..Default::default() }
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn age(f: &Path, secs: i64) {
    let d = Duration::from_secs(secs.unsigned_abs());
    let t = if secs < 0 { SystemTime::now() - d } else { SystemTime::now() + d };
    fs::OpenOptions::new().write(true).open(f).unwrap().set_modified(t).unwrap();
}

struct Fake(&'static str, Vec<Hit>, Vec<(u32, u32, &'static str)>, AtomicU64);

impl Source for Fake {
    fn id(&self) -> &str {
        self.0
    }
    fn name(&self) -> &str {
        self.0
    }
    fn search(&self, kind: Kind, _: &str, _: &str) -> Result<Vec<Hit>, String> {
        self.3.fetch_add(1, Ordering::SeqCst);
        Ok(self.1.iter().filter(|h| h.kind == kind).cloned().collect())
    }
    fn episodes(&self, _: &Hit, _: &BTreeSet<u32>, _: &str) -> Result<Episodes, String> {
        Ok(self.2.iter().map(|e| ((e.0, e.1), e.2.to_string())).collect())
    }
}

fn source(hits: Vec<Hit>, eps: Vec<(u32, u32, &'static str)>) -> Arc<Fake> {
    Arc::new(Fake("tvmaze", hits, eps, AtomicU64::new(0)))
}

fn tv(id: &str, title: &str, year: i32) -> Hit {
    Hit::new("tvmaze", id, Kind::Tv, title, Some(year))
}

fn film(id: &str, title: &str, year: Option<i32>) -> Hit {
    Hit::new("tvmaze", id, Kind::Movie, title, year)
}

fn eps() -> Vec<(u32, u32, &'static str)> {
    (1..9).map(|n| (1, n, "Titel")).collect()
}

fn matched(l: &Lib, files: &[PathBuf], src: Arc<Fake>, aliases: &Aliases) -> Vec<Row> {
    let mut rows: Vec<Row> = files.iter().map(|f| Row::new(f).unwrap()).collect();
    let f = Finder::new(vec![src as Arc<dyn Source>], "en", aliases);
    match_rows(&mut rows, &l.cfg(), &f, &|_, _, _| {}, &|_| {});
    rows
}

fn keep(log: &mut Vec<Entry>) -> impl FnMut(Entry) -> std::io::Result<()> + '_ {
    |e| {
        log.push(e);
        Ok(())
    }
}

#[cfg(windows)]
fn locked(f: &Path, share: u32) -> fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new().read(true).share_mode(share).open(f).unwrap()
}

// ---------------------------------------------------------------- C1: nur ein Schreiber je Teildatei

#[test]
fn second_writer_cannot_touch_a_running_copy() {
    let l = Lib::new();
    let data = vec![7u8; 12_000_000];
    let (a, dst) = (l.file("src/a.mkv", &data), l.p("mv/a.mkv"));
    let (part, seen) = (copy::partial(&dst), Mutex::new(None));
    let probe = |phase: &str, _: u64, _: u64| {
        let mut s = seen.lock().unwrap();
        if phase == "verify" && s.is_none() {
            let rival = fs::OpenOptions::new().append(true).open(&part);
            let again = copy::hold(&l.p("src/a.mkv")).and_then(|h| copy::copy_verified(&h, &l.p("mv/a.mkv"), &quiet, 1, Duration::ZERO));
            *s = Some((rival.is_err(), again.is_err()));
        }
        true
    };
    copy::copy_verified(&copy::hold(&a).unwrap(), &dst, &probe, 1, Duration::ZERO).unwrap();
    assert_eq!(*seen.lock().unwrap(), Some((true, true)));
    assert!(fs::read(&dst).unwrap() == data);
}

#[test]
fn cancel_keeps_partial_and_resumes() {
    let l = Lib::new();
    let data: Vec<u8> = (0..20_000_000u32).map(|n| (n / 7) as u8).collect();
    let (a, dst) = (l.file("src/a.mkv", &data), l.p("mv/a.mkv"));
    let e = copy::copy(&copy::hold(&a).unwrap(), &dst, &|_, pos, _| pos < 8_000_000).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::Interrupted);
    assert!(!dst.exists() && copy::partial(&dst).exists());
    assert_eq!(copy::leftovers(&[l.p("mv")], copy::PARTIAL), [copy::partial(&dst)]);
    copy::copy(&copy::hold(&a).unwrap(), &dst, &quiet).unwrap();
    assert!(fs::read(&dst).unwrap() == data && copy::leftovers(&[l.p("mv")], copy::PARTIAL).is_empty());
}

#[test]
fn hopeless_errors_fail_at_once() {
    let l = Lib::new();
    let a = l.file("src/a.mkv", b"x");
    fs::write(l.p("mv/datei"), b"x").unwrap();
    let t = Instant::now();
    assert!(copy::copy(&copy::hold(&a).unwrap(), &l.p("mv/datei/a.mkv"), &quiet).is_err());
    assert!(t.elapsed() < Duration::from_secs(2));
}

#[test]
fn rename_handles_dotted_paths() {
    let l = Lib::new();
    let a = l.file("src/a.mkv", b"x");
    copy::copy(&copy::hold(&a).unwrap(), &l.p("tv/../mv/./a.mkv"), &quiet).unwrap();
    assert!(l.p("mv/a.mkv").exists());
    assert_eq!(kern::library::check(&s(&l.p("tv/../mv"))).unwrap(), s(&l.p("mv")));
    assert_eq!(fs::read_dir(l.p("mv")).unwrap().count(), 1); // Schreibprobe raeumt auf
}

// ---------------------------------------------------------------- H1, M16: Dateien in Arbeit

#[cfg(windows)]
#[test]
fn file_open_for_writing_is_never_offered_or_moved() {
    let l = Lib::new();
    let a = l.file("src/Old 2021.mkv", &[1; 5000]);
    let mut w = fs::OpenOptions::new().write(true).open(&a).unwrap();
    age(&a, -7200);
    let src = || source(vec![film("1", "Old", Some(2021))], vec![]);
    let r = &matched(&l, std::slice::from_ref(&a), src(), &HashMap::new())[0];
    assert_eq!((r.status, r.checked), (Status::Busy, false));
    let mut forced = r.clone();
    forced.confirmed = true;
    forced.dst = Some(l.p("mv/Old (2021)/Old (2021).mkv"));
    assert!(execute(&mut forced, "move", &mut keep(&mut vec![]), &quiet).is_err());
    assert!(a.exists() && !l.p("mv/Old (2021)/Old (2021).mkv").exists());
    w.seek(SeekFrom::Start(0)).unwrap();
    w.write_all(b"fertig").unwrap();
    drop(w);
    age(&a, -7200);
    assert_eq!(matched(&l, &[a], src(), &HashMap::new())[0].status, Status::Safe);
}

#[test]
fn download_marker_future_clock_and_override() {
    let l = Lib::new();
    let src = || source(vec![film("1", "Old", Some(2021)), film("2", "Neu", Some(2022)), film("3", "Jung", Some(2023))], vec![]);
    let (a, b, c) = (l.file("src/Old 2021.mkv", b"x"), l.file("src/Neu 2022.mkv", b"x"), l.file("src/Jung 2023.mkv", b"x"));
    l.file("src/Old 2021.mkv.part", b"x");
    age(&b, 90_000); // Uhr des NAS geht vor
    age(&c, -2);
    let mut r = matched(&l, &[a, b, c], src(), &HashMap::new());
    assert_eq!(r.iter().map(|r| r.status).collect::<Vec<_>>(), [Status::Busy, Status::Safe, Status::Busy]);
    r[2].rested = true;
    let no = HashMap::new();
    resolve(&mut r[2], &l.cfg(), &Library::new(&l.p("tv"), &no), &Library::new(&l.p("mv"), &no));
    assert_eq!(r[2].status, Status::Safe);
}

#[test]
fn skipping_the_wait_does_not_confirm_the_title() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![film("1", "Der Killer", Some(2023)), film("2", "Der Killer", Some(1989))], vec![]), Ui::default());
    let f = l.file("src/Der Killer.mp4", b"x");
    age(&f, -2);
    a.add(vec![f], false);
    assert_eq!(idle(&a)["rows"][0]["status"], "laeuft");
    a.call("approve", &[json!([1])]);
    let r = idle(&a)["rows"][0].clone();
    assert_eq!((r["status"].as_str(), r["checked"].as_bool()), (Some("pruefen"), Some(false)));
}

#[test]
fn folders_cannot_change_during_a_job() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![], vec![]), Ui::default());
    a.add(vec![l.p("src")], false);
    let m = a.call("set", &[json!("movies"), json!(s(&l.p("tv")))]);
    let busy = a.busy();
    if busy {
        assert!(m.as_str().unwrap().contains("warten"));
    }
    idle(&a);
}

// ---------------------------------------------------------------- H9: Verschieben in der richtigen Reihenfolge

#[cfg(windows)]
#[test]
fn locked_original_still_ends_in_history_with_undo() {
    let l = Lib::new();
    let a = l.file("src/Old 2021.mkv", &[1; 5000]);
    let mut r = matched(&l, std::slice::from_ref(&a), source(vec![film("1", "Old", Some(2021))], vec![]), &HashMap::new()).remove(0);
    let reader = locked(&a, 1); // Abspielprogramm: Lesen erlaubt, Loeschen nicht
    let mut log = vec![];
    let res = execute(&mut r, "move", &mut keep(&mut log), &quiet);
    drop(reader);
    if res.is_ok() {
        // anderes Laufwerk: kopiert, protokolliert, Original bleibt
        assert!(r.note.contains("nicht gelöscht") && log.len() == 1 && a.exists());
    } else {
        // gleiches Laufwerk: gar nichts passiert
        assert!(a.exists() && log.is_empty() && !l.p("mv/Old (2021)/Old (2021).mkv").exists());
    }
}

#[test]
fn without_history_nothing_is_moved() {
    let l = Lib::new();
    let a = l.file("src/Old 2021.mkv", &[1; 5000]);
    let mut r = matched(&l, std::slice::from_ref(&a), source(vec![film("1", "Old", Some(2021))], vec![]), &HashMap::new()).remove(0);
    assert!(execute(&mut r, "move", &mut |_| Err(std::io::Error::other("Platte voll")), &quiet).is_err());
    assert!(a.exists() && !l.p("mv/Old (2021)/Old (2021).mkv").exists());
}

#[cfg(windows)]
#[test]
fn locked_subtitle_does_not_strand_the_film() {
    let l = Lib::new();
    let a = l.file("src/Old 2021.mkv", &[1; 5000]);
    let sub = l.file("src/Old 2021.de.srt", b"1");
    let mut r = matched(&l, &[a], source(vec![film("1", "Old", Some(2021))], vec![]), &HashMap::new()).remove(0);
    let lock = locked(&sub, 0);
    let mut log = vec![];
    execute(&mut r, "move", &mut keep(&mut log), &quiet).unwrap();
    drop(lock);
    assert!(r.status == Status::Done && r.note.contains("Untertitel nicht übertragen") && log.len() == 1);
    assert!(l.p("mv/Old (2021)/Old (2021).mkv").exists() && sub.exists());
}

#[test]
fn same_drive_move_is_a_rename() {
    let l = Lib::new();
    let a = l.file("src/Old 2021.mkv", &[1; 5000]);
    let mut r = matched(&l, std::slice::from_ref(&a), source(vec![film("1", "Old", Some(2021))], vec![]), &HashMap::new()).remove(0);
    let phases = Mutex::new(BTreeSet::new());
    let mut log = vec![];
    execute(&mut r, "move", &mut keep(&mut log), &|p, _, _| {
        phases.lock().unwrap().insert(p.to_string());
        true
    })
    .unwrap();
    assert_eq!(phases.into_inner().unwrap(), BTreeSet::from(["verify".to_string()]));
    undo(&log[0], &l.cfg(), &mut |_| Ok(()), &quiet).unwrap();
    assert!(a.exists() && l.p("mv").exists());
}

#[test]
fn sidecars_belong_to_exactly_one_film() {
    let l = Lib::new();
    let rocky = l.file("src/Rocky.mkv", b"x");
    l.file("src/Rocky.II.1979.mkv", b"x");
    for n in ["Rocky.de.srt", "ROCKY.en.forced.srt", "Rocky.II.1979.de.srt", "Rocky.III.srt.txt", "Rocky.Balboa.srt"] {
        l.file(&format!("src/{n}"), b"x");
    }
    let got: Vec<String> = sidecars(&rocky).into_iter().map(|x| x.1).collect();
    assert_eq!(got, [".en.forced.srt", ".de.srt"]);
}

// ---------------------------------------------------------------- H4: gelernte Zuordnungen vergiften nichts

fn alias(folder: &str, ids: &[(&str, &str)]) -> Alias {
    Alias { folder: folder.into(), ids: ids.iter().map(|i| (i.0.into(), i.1.into())).collect() }
}

#[test]
fn unproven_alias_is_never_safe() {
    let l = Lib::new();
    fs::create_dir(l.p("tv/Silo (2023)")).unwrap();
    let f = l.file("src/Ganz Anders S01E07.mkv", b"x");
    let known = HashMap::from([("ganz anders".to_string(), alias("Silo (2023)", &[]))]);
    let r = &matched(&l, std::slice::from_ref(&f), source(vec![tv("1", "Silo", 2023)], eps()), &known)[0];
    assert_eq!((r.status, r.checked), (Status::Check, false));
    let known = HashMap::from([("ganz anders".to_string(), alias("Silo (2023)", &[("tvmaze", "1")]))]);
    let r = &matched(&l, &[f], source(vec![tv("1", "Silo", 2023)], eps()), &known)[0];
    assert_eq!((r.status, r.folder.as_str()), (Status::Safe, "Silo (2023)"));
}

#[test]
fn generic_and_folder_titles_are_never_learned() {
    let l = Lib::new();
    fs::create_dir(l.p("tv/Silo (2023)")).unwrap();
    for (rel, ok) in [("src/Mein Ordner/S01E01.mkv", false), ("src/Video S01E01.mkv", false), ("src/Das Silo S01E01.mkv", true)] {
        let f = l.file(rel, b"x");
        let h = Hit { aka: vec!["Das Silo".into(), "Video".into(), "Mein Ordner".into()], ..tv("1", "Silo", 2023) };
        let mut r = matched(&l, &[f], source(vec![h], eps()), &HashMap::new()).remove(0);
        assert_eq!(r.folder, "Silo (2023)", "{rel}");
        r.approved = true;
        assert_eq!(learned(&r).is_some(), ok, "{rel}");
        r.approved = false;
        r.confirmed = true; // nur umbenannt
        assert!(learned(&r).is_none());
    }
    assert_eq!(serde_json::from_str::<Aliases>(r#"{"a":"Alt","b":{"folder":"Neu","ids":[["x","1"]]}}"#).unwrap()["a"], alias("Alt", &[]));
}

// ---------------------------------------------------------------- H5, M5, M14, M15, M17: Erkennung

fn p(n: &str) -> Media {
    parse(Path::new(n))
}

#[test]
fn title_words_are_not_mistaken_for_tags() {
    for (f, t, y) in [
        ("x/Madame Web (2024).mkv", "Madame Web", 2024),
        ("x/Charlotte's Web 2006 1080p.mkv", "Charlotte's Web", 2006),
        ("x/The.Good.German.2006.German.DL.1080p.BluRay.x264.mkv", "The Good German", 2006),
        ("x/Multi (2019).mkv", "Multi", 2019),
        ("x/Dune Teil 2 (2024).mkv", "Dune Teil 2", 2024),
        ("x/Star Wars Episode 4 1977.mkv", "Star Wars Episode 4", 1977),
    ] {
        let m = p(f);
        assert_eq!((m.kind, m.title.as_str(), m.year), (Kind::Movie, t, Some(y)), "{f}");
    }
    assert_eq!(p("x/Web of Lies S01E02.mkv").title, "Web of Lies");
    assert_eq!(p("x/Der.Film.German.DL.1080p.WEB.x264.mkv").title, "Der Film");
    assert_eq!(p("x/German.1080p.mkv").title, "German");
}

#[test]
fn episode_codes() {
    for (f, s, e) in [
        ("A.S01E01E02E03.mkv", 1, vec![1, 2, 3]),
        ("A S01E01-03 - Pilot.mkv", 1, vec![1, 2, 3]),
        ("A S01E01-E03.mkv", 1, vec![1, 2, 3]),
        ("A S01E05-720p.mkv", 1, vec![5]),
        ("A S01E05-10bit.mkv", 1, vec![5]),
        ("A.S2024E05.mkv", 2024, vec![5]),
        ("A.1x02.mkv", 1, vec![2]),
    ] {
        let m = p(&format!("x/{f}"));
        assert_eq!((m.kind, m.season, m.episodes), (Kind::Tv, Some(s), e), "{f}");
    }
    assert_eq!(p("x/Film.2019.DD5.1x264.mkv").kind, Kind::Movie);
    let d = p("x/Show.2024.05.12.mkv");
    assert!(d.kind == Kind::Unknown && d.title == "Show" && d.hint.contains("Datum"));
}

#[test]
fn other_scripts_stay_distinct() {
    assert_ne!(norm("進撃の巨人"), norm("鬼滅の刃"));
    assert_eq!((norm("Амели").as_str(), norm("Bodø & Łódź").as_str()), ("амели", "bodo and lodz"));
    let l = Lib::new();
    fs::create_dir(l.p("tv/鬼滅の刃")).unwrap();
    let f = l.file("src/進撃の巨人 S01E01.mkv", b"x");
    let r = &matched(&l, &[f], source(vec![], vec![]), &HashMap::new())[0];
    assert_eq!(r.folder, "進撃の巨人");
    assert!(short(&"ö".repeat(200), 201).len() <= 201);
}

#[test]
fn finds_existing_episodes_in_any_layout() {
    let l = Lib::new();
    l.file("tv/Silo/S01/Silo 1x02.mkv", b"y");
    l.file("tv/Silo/Silo - S01E03.mkv", b"y");
    l.file("tv/Silo/2. Staffel/Silo - S02E01.mkv", b"y");
    let files: Vec<PathBuf> = ["Silo.S01E02.mkv", "Silo.S01E03.mkv", "Silo.S01E04.mkv"].iter().map(|n| l.file(&format!("src/{n}"), b"xx")).collect();
    let r = matched(&l, &files, source(vec![tv("1", "Silo", 2023)], eps()), &HashMap::new());
    assert_eq!(r.iter().map(|r| r.status).collect::<Vec<_>>(), [Status::Conflict, Status::Conflict, Status::Safe]);
    assert!(s(r[2].dst.as_ref().unwrap()).contains(r"Silo\S01\Silo"));
}

#[test]
fn double_episode_overlap_and_removal() {
    let l = Lib::new();
    let files: Vec<PathBuf> = ["Silo.S01E01E02.mkv", "Silo.S01E02.mkv"].iter().map(|n| l.file(&format!("src/{n}"), b"xx")).collect();
    let mut r = matched(&l, &files, source(vec![tv("1", "Silo", 2023)], eps()), &HashMap::new());
    assert_eq!((r[0].status, r[1].status, r[1].checked), (Status::Safe, Status::Conflict, false));
    r.remove(0);
    mark_duplicates(r.iter_mut());
    assert_eq!((r[0].status, r[0].checked), (Status::Safe, true));
}

#[test]
fn source_without_year_keeps_year_and_asks() {
    let l = Lib::new();
    let f = l.file("src/Old 2021.mkv", b"x");
    let r = &matched(&l, &[f], source(vec![film("1", "Old", None)], vec![]), &HashMap::new())[0];
    assert_eq!(r.status, Status::Check);
    assert!(r.dst.as_ref().unwrap().ends_with("Old (2021)/Old (2021).mkv"));
}

// ---------------------------------------------------------------- Schnittstelle

#[derive(Default)]
struct Ui {
    free: HashMap<String, u64>,
    asked: Arc<Mutex<Vec<PathBuf>>>,
}

impl Dialogs for Ui {
    fn free(&self, p: &Path) -> Option<u64> {
        self.asked.lock().unwrap().push(p.into());
        self.free.get("alle").copied().or(Some(1 << 50))
    }
}

fn api(cfg: &Settings, src: Arc<Fake>, ui: Ui) -> Arc<Api> {
    cfg.save().unwrap();
    Api::new(Box::new(ui), vec![src as Arc<dyn Source>] as Sources)
}

fn idle(a: &Arc<Api>) -> Value {
    let t = Instant::now();
    while a.busy() && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(30));
    a.call("state", &[json!(0), json!(0)])
}

fn picks(st: &Value) -> Value {
    st["rows"].as_array().unwrap().iter().filter(|r| r["checked"] == true).map(|r| json!({ "id": r["id"], "dst": r["dst"] })).collect()
}

#[test]
fn transfer_takes_exactly_what_the_dialog_showed() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![film("1", "Old", Some(2021)), film("2", "Neu", Some(2022))], vec![]), Ui::default());
    a.add(vec![l.file("src/Old 2021.mkv", b"x")], false);
    let shown = picks(&idle(&a));
    a.add(vec![l.file("src/Neu 2022.mkv", b"y")], false); // kommt waehrend des Dialogs dazu
    assert_eq!(picks(&idle(&a)).as_array().unwrap().len(), 2);
    assert_eq!(a.call("transfer", &[shown.clone(), json!("move")]), json!(""));
    let st = idle(&a);
    assert_eq!(st["info"], "1 übertragen und geprüft");
    assert!(l.p("src/Neu 2022.mkv").exists() && !l.p("src/Old 2021.mkv").exists());
    assert!(a.call("transfer", &[shown, json!("move")]).as_str().unwrap().contains("geändert"));
    let mut wrong = picks(&st);
    wrong[0]["dst"] = json!("C:\\woanders.mkv");
    assert!(a.call("transfer", &[wrong, json!("copy")]).as_str().unwrap().contains("Ziel"));
    assert!(!a.call("transfer", &[picks(&st), json!("format")]).as_str().unwrap().is_empty());
}

#[test]
fn search_only_proposes() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![film("1", "Etwas ganz anderes", Some(1990))], vec![]), Ui::default());
    a.add(vec![l.file("src/Dune 2021.mkv", b"x")], false);
    idle(&a);
    a.call("search", &[json!([1]), json!("Dune 2021"), json!("movie")]);
    let r = idle(&a)["rows"][0].clone();
    assert_eq!((r["status"].as_str(), r["checked"].as_bool()), (Some("pruefen"), Some(false)), "{r}");
    a.call("pick", &[json!([1]), json!(0)]);
    let r = idle(&a)["rows"][0].clone();
    assert_eq!((r["status"].as_str(), r["checked"].as_bool()), (Some("sicher"), Some(true)));
    a.call("match", &[json!(false)]); // neuer Abgleich verwirft Entschiedenes nicht
    assert_eq!(idle(&a)["rows"][0]["title"], "Etwas ganz anderes");
    a.call("match", &[json!(true)]);
    assert_ne!(idle(&a)["rows"][0]["status"], "sicher");
}

#[test]
fn big_folders_do_not_block_the_window() {
    let _g = fresh();
    let l = Lib::new();
    for n in 0..1500 {
        l.file(&format!("src/Silo.S{:02}E{:02}.mkv", n / 90 + 1, n % 90 + 1), b"x");
    }
    let a = api(&l.cfg(), source(vec![tv("1", "Silo", 2023)], eps()), Ui::default());
    a.add(vec![l.p("src")], false);
    let mut worst = Duration::ZERO;
    for _ in 0..40 {
        let t = Instant::now();
        a.busy();
        a.call("check", &[json!([]), json!(true)]);
        worst = worst.max(t.elapsed());
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(worst < Duration::from_millis(400), "{worst:?}");
    a.call("stop", &[]);
    idle(&a);
}

#[test]
fn state_sends_only_changes() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![film("1", "Old", Some(2021)), film("2", "Neu", Some(2022))], vec![]), Ui::default());
    a.add(vec![l.file("src/Old 2021.mkv", b"x"), l.file("src/Neu 2022.mkv", b"y")], false);
    let st = idle(&a);
    assert_eq!(st["rows"].as_array().unwrap().len(), 2);
    let rv = st["rv"].clone();
    a.call("check", &[json!([1]), json!(false)]);
    let d = a.call("state", &[json!(0), rv.clone()]);
    assert_eq!((d["rows"].as_array().unwrap().len(), d["rows"][0]["id"].as_u64(), d["ids"].is_null()), (1, Some(1), true));
    a.call("remove", &[json!([1])]);
    assert_eq!(a.call("state", &[json!(0), rv])["ids"], json!([2]));
}

#[test]
fn adding_while_busy_is_queued() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![film("1", "Old", Some(2021)), film("2", "Neu", Some(2022))], vec![]), Ui::default());
    a.add(vec![l.file("src/Old 2021.mkv", b"x")], false);
    a.add(vec![l.file("src/Neu 2022.mkv", b"y")], false);
    for _ in 0..100 {
        if idle(&a)["rows"].as_array().unwrap().len() == 2 {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("zweite Datei ging verloren");
}

#[test]
fn changed_library_moves_the_targets() {
    let _g = fresh();
    let l = Lib::new();
    fs::create_dir(l.p("mv2")).unwrap();
    let a = api(&l.cfg(), source(vec![film("1", "Old", Some(2021))], vec![]), Ui::default());
    a.add(vec![l.file("src/Old 2021.mkv", b"x")], false);
    idle(&a);
    assert_eq!(a.call("set", &[json!("movies"), json!(s(&l.p("mv2")))]), json!(""));
    for _ in 0..100 {
        if idle(&a)["rows"][0]["dst"].as_str().unwrap().contains("mv2") {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("Ziel zeigt noch auf den alten Ordner");
}

#[test]
fn films_work_without_series_and_space_is_checked_per_drive() {
    let _g = fresh();
    let l = Lib::new();
    let cfg = Settings { tv: String::new(), ..l.cfg() };
    let ui = Ui { free: HashMap::from([("alle".to_string(), copy::RESERVE + 3)]), ..Default::default() };
    let asked = ui.asked.clone();
    let a = api(&cfg, source(vec![film("1", "Old", Some(2021)), film("2", "Neu", Some(2022)), tv("3", "Silo", 2023)], eps()), ui);
    a.add(vec![l.file("src/Old 2021.mkv", b"xx"), l.file("src/Neu 2022.mkv", b"yy"), l.file("src/Silo.S01E01.mkv", b"z")], false);
    let st = idle(&a);
    let silo = st["rows"].as_array().unwrap().iter().find(|r| r["file"] == "Silo.S01E01.mkv").unwrap().clone();
    assert!(silo["note"].as_str().unwrap().contains("Serienordner noch nicht"));
    assert!(a.call("transfer", &[picks(&st), json!("copy")]).as_str().unwrap().contains("Zu wenig Platz"));
    assert!(asked.lock().unwrap().iter().all(|p| !p.as_os_str().is_empty()));
    let one = json!([picks(&st)[0]]);
    assert_eq!(a.call("transfer", &[one, json!("copy")]), json!(""));
    idle(&a);
}

#[test]
fn episode_can_be_set_by_hand() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![tv("1", "Silo", 2023)], eps()), Ui::default());
    a.add(vec![l.file("src/Silo Folge 3.mkv", b"x"), l.file("src/Silo Folge 9.mkv", b"y")], false);
    idle(&a);
    a.call("set_episode", &[json!([1, 2]), json!(1), json!(4)]);
    let st = idle(&a);
    let names: Vec<&str> = st["rows"].as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Silo (2023) - S01E04 - Titel", "Silo (2023) - S01E05 - Titel"]);
}

#[test]
fn watched_folders_do_not_leak_unknown_names() {
    let _g = fresh();
    let l = Lib::new();
    let src = source(vec![], vec![]);
    let a = api(&l.cfg(), src.clone(), Ui::default());
    l.file("src/Geburt Lena privat.mp4", b"x");
    a.add(vec![l.p("src")], true);
    let st = idle(&a);
    assert_eq!((st["rows"].as_array().unwrap().len(), src.3.load(Ordering::SeqCst)), (1, 0));
    assert!(st["rows"][0]["note"].as_str().unwrap().contains("nicht online gesucht"));
}

#[test]
fn undo_runs_as_a_job_and_forgets_the_alias() {
    let _g = fresh();
    let l = Lib::new();
    fs::create_dir(l.p("tv/Silo (2023)")).unwrap();
    let h = Hit { aka: vec!["Das Silo".into()], ..tv("1", "Silo", 2023) };
    let a = api(&l.cfg(), source(vec![h], eps()), Ui::default());
    a.add(vec![l.file("src/Das Silo S01E01.mkv", b"x")], false);
    idle(&a);
    a.call("pick", &[json!([1]), json!(0)]);
    let st = idle(&a);
    a.call("transfer", &[picks(&st), json!("move")]);
    idle(&a);
    assert_eq!(a.call("aliases", &[])[0]["key"], "das silo");
    assert_eq!(a.call("undo", &[json!(0)]), json!(""));
    assert_eq!(idle(&a)["info"], "Wiederhergestellt: Das Silo S01E01.mkv");
    assert!(l.p("src/Das Silo S01E01.mkv").exists());
    assert_eq!(a.call("aliases", &[]), json!([]));
    assert_eq!(a.call("history", &[])[1]["undone"], true);
}

#[test]
fn checksum_lists_cannot_point_elsewhere() {
    let _g = fresh();
    let l = Lib::new();
    let a = api(&l.cfg(), source(vec![], vec![]), Ui::default());
    let secret = l.file("geheim.txt", b"x");
    let list = l.file("src/a.sfv", format!("..\\geheim.txt 00000000\n{} 00000000\n\\\\angreifer\\x\\y 00000000\ngut.txt 00000000\n", secret.display()).as_bytes());
    a.sums_add(vec![list]);
    std::thread::sleep(Duration::from_millis(300));
    let got = a.call("sums", &[]);
    let r: Vec<&str> = got.as_array().unwrap().iter().map(|x| x["result"].as_str().unwrap()).collect();
    assert_eq!(r.iter().filter(|x| **x == "fremd").count(), 3, "{got}");
}

#[test]
fn damaged_settings_are_kept_aside() {
    let _g = fresh();
    fs::write(HOME.path().join("settings.json"), "{kaputt").unwrap();
    assert!(Settings::load().tv.is_empty());
    assert!(HOME.path().join("settings.json.bad").exists() || HOME.path().join("settings.json.bad").with_extension("bad").exists());
}

// ---------------------------------------------------------------- Ersetzen

fn replaceable(l: &Lib, old_name: &str, old: &[u8], new: &[u8]) -> Row {
    l.file(&format!("mv/Old (2021)/{old_name}"), old);
    let a = l.file("src/Old 2021.mp4", new);
    matched(l, &[a], source(vec![film("1", "Old", Some(2021))], vec![]), &HashMap::new()).remove(0)
}

fn allow(l: &Lib, r: &mut Row) {
    r.replace = r.old.clone();
    let no = HashMap::new();
    resolve(r, &l.cfg(), &Library::new(&l.p("tv"), &no), &Library::new(&l.p("mv"), &no));
}

fn names_in(p: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(p).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn replacing_needs_an_explicit_choice() {
    let l = Lib::new();
    let mut r = replaceable(&l, "Old (2021).mkv", b"alt alt", b"neu");
    assert_eq!((r.status, r.checked, r.old.is_some()), (Status::Conflict, false, true));
    assert!(execute(&mut { let mut x = r.clone(); x.dst = Some(l.p("mv/Old (2021)/Old (2021).mkv")); x }, "copy", &mut |_| Ok(()), &quiet).is_err());
    allow(&l, &mut r);
    assert_eq!((r.status, r.checked), (Status::Replace, true));
    assert!(r.dst.as_ref().unwrap().ends_with("Old (2021)/Old (2021).mp4"));
}

#[test]
fn replace_other_extension_and_delete_old() {
    for action in ["copy", "move"] {
        let l = Lib::new();
        let mut r = replaceable(&l, "Old (2021).mkv", b"alt alt", b"neu");
        allow(&l, &mut r);
        let mut log = vec![];
        execute(&mut r, action, &mut keep(&mut log), &quiet).unwrap();
        assert_eq!(names_in(&l.p("mv/Old (2021)")), ["Old (2021).mp4"], "{action}");
        assert_eq!(fs::read(l.p("mv/Old (2021)/Old (2021).mp4")).unwrap(), b"neu");
        assert_eq!(l.p("src/Old 2021.mp4").exists(), action == "copy");
        let e = undo(&log[0], &l.cfg(), &mut |_| Ok(()), &quiet).unwrap_err();
        assert!(e.to_string().contains("gelöscht") && l.p("mv/Old (2021)/Old (2021).mp4").exists());
    }
}

#[test]
fn replace_same_name_keep_old_and_undo() {
    for action in ["copy", "move"] {
        let l = Lib::new();
        let mut r = replaceable(&l, "Old (2021).mp4", b"alt alt", b"neu");
        allow(&l, &mut r);
        r.keep_old = true;
        let mut log = vec![];
        execute(&mut r, action, &mut keep(&mut log), &quiet).unwrap();
        assert_eq!(names_in(&l.p("mv/Old (2021)")), ["Old (2021).mp4", "Old (2021).mp4.spool-alt"]);
        let alt = copy::leftovers(&[l.p("mv")], copy::KEPT);
        assert_eq!(copy::original_name(&alt[0]), Some(l.p("mv/Old (2021)/Old (2021).mp4")));
        undo(&log[0], &l.cfg(), &mut |_| Ok(()), &quiet).unwrap();
        assert_eq!(names_in(&l.p("mv/Old (2021)")), ["Old (2021).mp4"]);
        assert_eq!(fs::read(l.p("mv/Old (2021)/Old (2021).mp4")).unwrap(), b"alt alt");
        assert_eq!(fs::read(l.p("src/Old 2021.mp4")).unwrap(), b"neu");
    }
}

#[test]
fn failed_replacement_leaves_everything_as_before() {
    for action in ["copy", "move"] {
        let l = Lib::new();
        let mut r = replaceable(&l, "Old (2021).mp4", b"alt alt", b"neu");
        allow(&l, &mut r);
        // Verlauf nicht schreibbar
        assert!(execute(&mut r.clone(), action, &mut |_| Err(std::io::Error::other("voll")), &quiet).is_err());
        // Abbruch waehrend des Kopierens
        if action == "copy" {
            assert!(execute(&mut r.clone(), action, &mut |_| Ok(()), &|p, _, _| p != "copy").is_err());
        }
        assert_eq!(fs::read(l.p("mv/Old (2021)/Old (2021).mp4")).unwrap(), b"alt alt");
        assert_eq!(fs::read(l.p("src/Old 2021.mp4")).unwrap(), b"neu");
        assert!(copy::leftovers(&[l.p("mv")], copy::KEPT).is_empty());
    }
}

#[cfg(windows)]
#[test]
fn old_file_in_use_is_not_replaced() {
    let l = Lib::new();
    let mut r = replaceable(&l, "Old (2021).mp4", b"alt alt", b"neu");
    allow(&l, &mut r);
    let lock = locked(&l.p("mv/Old (2021)/Old (2021).mp4"), 1); // wird gerade abgespielt
    assert!(execute(&mut r.clone(), "move", &mut |_| Ok(()), &quiet).is_err());
    drop(lock);
    assert_eq!(fs::read(l.p("mv/Old (2021)/Old (2021).mp4")).unwrap(), b"alt alt");
    assert!(l.p("src/Old 2021.mp4").exists());
}

#[test]
fn changed_old_file_withdraws_the_permission() {
    let l = Lib::new();
    let mut r = replaceable(&l, "Old (2021).mp4", b"alt alt", b"neu");
    allow(&l, &mut r);
    l.file("mv/Old (2021)/Old (2021).mp4", b"inzwischen bessere Fassung");
    assert!(execute(&mut r.clone(), "copy", &mut |_| Ok(()), &quiet).unwrap_err().to_string().contains("geändert"));
    let no = HashMap::new();
    resolve(&mut r, &l.cfg(), &Library::new(&l.p("tv"), &no), &Library::new(&l.p("mv"), &no));
    assert_eq!((r.status, r.replace.is_none()), (Status::Conflict, true));
}

#[test]
fn some_files_must_not_be_replaced() {
    let l = Lib::new();
    // Doppelfolge enthaelt noch eine andere Folge
    l.file("tv/Silo/Season 01/Silo - S01E01-E02.mkv", b"y");
    let a = l.file("src/Silo.S01E02.mkv", b"xx");
    let r = &matched(&l, &[a], source(vec![tv("1", "Silo", 2023)], eps()), &HashMap::new())[0];
    assert!(r.status == Status::Conflict && r.old.is_none() && r.note.contains("weitere Folgen"), "{}", r.note);
    // mehrere fremde Videos im Filmordner
    l.file("mv/Old (2021)/Teil A.mkv", b"a");
    l.file("mv/Old (2021)/Teil B.mkv", b"bb");
    let b = l.file("src/Old 2021.mp4", b"neu");
    let r = &matched(&l, &[b], source(vec![film("1", "Old", Some(2021))], vec![]), &HashMap::new())[0];
    assert!(r.status == Status::Conflict && r.old.is_none() && r.note.contains("mehrere Videos"), "{}", r.note);
    // die Datei selbst
    let c = l.file("mv/Neu (2022)/Neu (2022).mkv", b"neu");
    let r = &matched(&l, &[c], source(vec![film("1", "Neu", Some(2022))], vec![]), &HashMap::new())[0];
    assert!(r.old.is_none());
}

#[test]
fn dialog_must_have_shown_what_is_replaced() {
    let _g = fresh();
    let l = Lib::new();
    l.file("mv/Old (2021)/Old (2021).mkv", b"alt alt");
    let a = api(&l.cfg(), source(vec![film("1", "Old", Some(2021))], vec![]), Ui::default());
    a.add(vec![l.file("src/Old 2021.mp4", b"neu")], false);
    let r = idle(&a)["rows"][0].clone();
    assert_eq!((r["status"].as_str(), r["old"]["name"].as_str(), r["checked"].as_bool()), (Some("konflikt"), Some("Old (2021).mkv"), Some(false)));
    a.call("replace", &[json!([1]), json!(true)]);
    let r = idle(&a)["rows"][0].clone();
    assert_eq!((r["status"].as_str(), r["replace"].as_bool(), r["checked"].as_bool()), (Some("ersetzen"), Some(true), Some(true)));
    let without = json!([{ "id": 1, "dst": r["dst"] }]);
    assert!(a.call("transfer", &[without, json!("copy"), json!(false)]).as_str().unwrap().contains("ersetzt"));
    a.call("replace", &[json!([1]), json!(false)]);
    assert_eq!(idle(&a)["rows"][0]["status"], "konflikt");
    a.call("replace", &[json!([1]), json!(true)]);
    let r = idle(&a)["rows"][0].clone();
    let shown = json!([{ "id": 1, "dst": r["dst"], "old": r["old"]["path"] }]);
    assert_eq!(a.call("transfer", &[shown, json!("copy"), json!(true)]), json!(""));
    assert!(idle(&a)["rows"][0]["note"].as_str().unwrap().contains("alte Fassung aufgehoben"));
    let alt = a.call("partials", &[json!("alt")]);
    assert!(!a.call("alt_restore", &[alt[0]["path"].clone()]).as_str().unwrap().contains("andere Datei"));
    assert_eq!(names_in(&l.p("mv/Old (2021)")), ["Old (2021).mkv", "Old (2021).mp4"]);
}
