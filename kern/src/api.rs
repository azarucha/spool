//! Bruecke zwischen Oberflaeche und Kern. Kennt weder Fenster noch HTML.
//!
//! Regel: Unter der Zustandssperre wird nie auf Dateien oder das Netz zugegriffen.
use crate::archive;
use crate::copy;
use crate::engine::*;
use crate::library;
use crate::names::*;
use crate::sources::{self, Sources};
use crate::subs::{self, SubHit};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

pub trait Dialogs: Send + Sync {
    fn files(&self) -> Vec<PathBuf> {
        vec![]
    }
    fn folder(&self) -> Option<PathBuf> {
        None
    }
    fn open(&self, _target: &str) {}
    /// Freier Platz am Pfad in Bytes.
    fn free(&self, _path: &Path) -> Option<u64> {
        None
    }
    /// Kennung des Laufwerks, zu dem ein Pfad gehoert.
    fn volume(&self, path: &Path) -> String {
        path.ancestors().last().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default()
    }
}

/// Woher Dateien kommen: vom Nutzer gezeigt, per Klick aus den Quellordnern, oder von der Ueberwachung.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    User,
    Folders,
    Watch,
}

#[derive(Default, Clone)]
struct Meter {
    i: usize,
    n: usize,
    text: String,
    phase: String,
    bytes: u64,
    total: u64,
    speed: f64,
}

#[derive(Default)]
struct State {
    v: u64,
    rv: u64,
    rows: BTreeMap<u64, Row>,
    stamps: HashMap<u64, u64>,
    removed: u64,
    next: u64,
    job: &'static str,
    meter: Meter,
    info: String,
    plex: (bool, u64),
    cfg: Settings,
    shows: Vec<Cand>,
    sums: Vec<Value>,
    subs: Vec<SubItem>,
    sub_key: String,
    queue: Vec<(Vec<PathBuf>, Origin)>,
}

#[derive(Clone)]
struct SubItem {
    path: PathBuf,
    hits: Vec<SubHit>,
    note: String,
}

impl State {
    fn mark(&mut self, id: u64) {
        self.v += 1;
        self.rv += 1;
        self.stamps.insert(id, self.rv);
    }
    fn mark_all(&mut self) {
        let ids: Vec<u64> = self.rows.keys().copied().collect();
        ids.into_iter().for_each(|i| self.mark(i));
    }
    /// Doppelte Ziele neu bestimmen und geaenderte Zeilen vormerken.
    fn duplicates(&mut self) {
        let before: Vec<(u64, Status, bool)> = self.rows.iter().map(|(i, r)| (*i, r.status, r.checked)).collect();
        mark_duplicates(self.rows.values_mut());
        for (i, s, c) in before {
            if self.rows.get(&i).is_some_and(|r| r.status != s || r.checked != c) {
                self.mark(i);
            }
        }
    }
}

pub struct Api {
    st: Mutex<State>,
    busy: AtomicBool,
    stop: Arc<AtomicBool>,
    checking: AtomicBool,
    sources: Sources,
    aliases: Mutex<Aliases>,
    dialogs: Box<dyn Dialogs>,
}

fn file_name(p: &Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

fn text_of(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn ids_of(v: Option<&Value>) -> Vec<u64> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_u64).collect(),
        Some(x) => x.as_u64().into_iter().collect(),
        None => vec![],
    }
}

fn example(key: &str) -> Values {
    let mut v: Values = HashMap::from([("n", "Silo".into()), ("y", "2023".into()), ("folder", "Silo (2023)".into())]);
    if key == "format_tv" {
        for (k, x) in [("s", "1"), ("s00", "01"), ("e", "3"), ("e00", "03"), ("s00e00", "S01E03"), ("sxe", "1x03"), ("t", "Machines")] {
            v.insert(k, x.into());
        }
        v.insert("plex_format", "{folder}/Season 01/{folder} - {s00e00} - {t}".into());
    } else {
        v.extend([("n", "Old".into()), ("y", "2021".into()), ("folder", "Old (2021)".into()), ("plex_format", "{folder}/{folder}".into())]);
    }
    v
}

/// Von Hand Entschiedenes, das ein neuer Abgleich nicht verwerfen darf.
fn decided(r: &Row) -> bool {
    r.approved || r.confirmed || r.manual.is_some() || r.by_hand.is_some()
}

const SUB_ID: &str = "opensubtitles";
const LINKS: &[(&str, &str)] = &[
    ("tmdb", "https://www.themoviedb.org"),
    ("tvdb", "https://thetvdb.com"),
    ("tvmaze", "https://www.tvmaze.com"),
    ("wikidata", "https://www.wikidata.org"),
    ("wikipedia", "https://en.wikipedia.org"),
    ("anilist", "https://anilist.co"),
    ("omdb", "https://www.omdbapi.com"),
    (SUB_ID, "https://www.opensubtitles.com"),
    ("kofi", "https://ko-fi.com/azarucha"),
    ("code", "https://github.com/azarucha/spool"),
];

impl Api {
    pub fn new(dialogs: Box<dyn Dialogs>, sources: Sources) -> Arc<Api> {
        let cfg = Settings::load();
        let keys = sources::load_keys();
        let sub_key = keys.get(SUB_ID).cloned().unwrap_or_default();
        for (id, key) in keys {
            if let Some(s) = sources.iter().find(|s| s.id() == id) {
                s.set_key(&key);
            }
        }
        sources::WIKI_POSTERS.store(cfg.wiki_posters, Ordering::SeqCst);
        let st = State { v: 1, rv: 1, next: 1, cfg, sub_key, ..Default::default() };
        let api = Arc::new(Api {
            st: Mutex::new(st),
            busy: AtomicBool::new(false),
            stop: Default::default(),
            checking: AtomicBool::new(false),
            sources,
            aliases: Mutex::new(load_aliases()),
            dialogs,
        });
        std::thread::spawn(sources::prune_cache);
        api.check_plex();
        let a = Arc::downgrade(&api);
        std::thread::spawn(move || {
            let mut n = 0u32;
            loop {
                std::thread::sleep(Duration::from_secs(20));
                let Some(a) = a.upgrade() else { break };
                n += 1;
                if n.is_multiple_of(3) {
                    a.check_plex();
                }
                a.tick();
            }
        });
        api
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn say(&self, text: impl Into<String>) {
        let mut s = self.lock();
        s.info = text.into();
        s.v += 1;
    }

    fn step(&self, i: usize, n: usize, text: &str) {
        let mut s = self.lock();
        (s.meter.i, s.meter.n, s.meter.text) = (i, n, text.into());
        s.v += 1;
    }

    fn running(&self) -> bool {
        !self.stop.load(Ordering::SeqCst)
    }

    fn start(self: &Arc<Self>, name: &'static str, work: impl FnOnce(&Arc<Api>) + Send + 'static) -> bool {
        if self.busy.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.stop.store(false, Ordering::SeqCst);
        {
            let mut s = self.lock();
            (s.job, s.info, s.meter) = (name, String::new(), Meter::default());
            s.v += 1;
        }
        let a = self.clone();
        std::thread::spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&a))).is_err() {
                a.lock().info = "Unerwarteter Fehler, bitte erneut versuchen".into();
            }
            let waiting = {
                let mut s = a.lock();
                s.job = "";
                s.v += 1;
                std::mem::take(&mut s.queue)
            };
            a.busy.store(false, Ordering::SeqCst);
            for (paths, from) in waiting {
                a.add_from(paths, from);
            }
        });
        true
    }

    fn cfg(&self) -> Settings {
        self.lock().cfg.clone()
    }

    fn change(&self, f: impl FnOnce(&mut Settings)) {
        let cfg = {
            let mut s = self.lock();
            f(&mut s.cfg);
            s.v += 1;
            s.cfg.clone()
        };
        sources::WIKI_POSTERS.store(cfg.wiki_posters, Ordering::SeqCst);
        if let Err(e) = cfg.save() {
            self.say(format!("Einstellungen nicht gespeichert: {e}"));
        }
    }

    fn aliases(&self) -> Aliases {
        self.aliases.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn edit_aliases(&self, f: impl FnOnce(&mut Aliases)) {
        let mut a = self.aliases.lock().unwrap_or_else(|e| e.into_inner());
        let old = a.clone();
        f(&mut a);
        if *a != old {
            save_aliases(&a);
        }
    }

    fn finder(&self) -> Finder {
        let cfg = self.cfg();
        let on = self.sources.iter().filter(|s| s.ready() && cfg.active.iter().any(|a| a == s.id())).cloned().collect();
        let mut f = Finder::new(on, &cfg.lang, &self.aliases());
        f.cancel = self.stop.clone();
        f
    }

    fn libraries(&self, cfg: &Settings) -> (Library, Library) {
        (Library::new(Path::new(&cfg.tv), &self.aliases()), Library::new(Path::new(&cfg.movies), &Aliases::new()))
    }

    /// Eingerichtete, aber gerade nicht erreichbare Ordner. Nicht eingerichtete melden die Zeilen selbst.
    fn reachable(&self) -> bool {
        let cfg = self.cfg();
        let bad: Vec<_> = [&cfg.tv, &cfg.movies, &cfg.music].into_iter().filter(|p| !p.is_empty() && !Path::new(p).is_dir()).cloned().collect();
        if !bad.is_empty() {
            self.say(format!("Plex-Ordner nicht erreichbar: {}", bad.join(", ")));
        }
        bad.is_empty()
    }

    fn check_plex(self: &Arc<Self>) {
        if self.checking.swap(true, Ordering::SeqCst) {
            return; // haengt noch an einem traegen Netzlaufwerk
        }
        let a = self.clone();
        std::thread::spawn(move || {
            let cfg = a.cfg();
            let roots: Vec<&String> = [&cfg.tv, &cfg.movies, &cfg.music].into_iter().filter(|p| !p.is_empty()).collect();
            let ok = !roots.is_empty() && roots.iter().all(|p| Path::new(p).is_dir());
            let free = roots.first().filter(|_| ok).and_then(|p| a.dialogs.free(Path::new(p))).unwrap_or(0);
            a.checking.store(false, Ordering::SeqCst);
            let mut s = a.lock();
            if s.plex != (ok, free) {
                s.plex = (ok, free);
                s.v += 1;
            }
        });
    }

    /// Alle 20 Sekunden: fertig gewordene Dateien neu pruefen, ueberwachte Ordner lesen.
    fn tick(self: &Arc<Self>) {
        if self.busy() {
            return;
        }
        let cfg = self.cfg();
        let waiting: Vec<(u64, Row)> = self.lock().rows.iter().filter(|(_, r)| r.status == Status::Busy).map(|(i, r)| (*i, r.clone())).collect();
        let ready: Vec<(u64, u64)> = waiting
            .into_iter()
            .filter(|(_, r)| occupied(r, &cfg).is_none())
            .map(|(i, r)| (i, fs::metadata(&r.src).map(|m| m.len()).unwrap_or(r.size)))
            .collect();
        if !ready.is_empty() {
            for (i, size) in &ready {
                if let Some(r) = self.lock().rows.get_mut(i) {
                    r.size = *size;
                }
            }
            let ids: Vec<u64> = ready.iter().map(|r| r.0).collect();
            self.rebuild(&ids, |_, _| {});
        } else if cfg.watch {
            self.add(cfg.folders.iter().map(PathBuf::from).collect(), true);
        }
    }

    // ------------------------------------------------ Zustand
    fn state(&self, v: u64, rv: u64) -> Value {
        let s = self.lock();
        if v == s.v {
            return Value::Null;
        }
        let m = &s.meter;
        let mut out = json!({
            "v": s.v, "rv": s.rv, "busy": s.job, "info": s.info, "version": env!("CARGO_PKG_VERSION"),
            "progress": { "i": m.i, "n": m.n, "text": m.text, "phase": m.phase, "bytes": m.bytes, "total": m.total, "speed": m.speed },
            "plex": { "ok": s.plex.0, "free": s.plex.1 },
            "cfg": s.cfg, "subKey": !s.sub_key.is_empty(),
            "sources": self.sources.iter().map(|x| json!({
                "id": x.id(), "name": x.name(), "ready": x.ready(), "needsKey": x.needs_key(), "hasKey": !x.key().is_empty(),
                "active": x.ready() && s.cfg.active.iter().any(|a| a == x.id()),
                "movies": x.kinds().contains(&Kind::Movie), "episodes": x.has_episodes(),
            })).collect::<Vec<_>>(),
            "preview": { "format_tv": preview(&s.cfg, "format_tv"), "format_movie": preview(&s.cfg, "format_movie") },
        });
        if rv != s.rv {
            // nur Geaendertes; nach Entfernungen zusaetzlich die Liste der vorhandenen Kennungen
            out["rows"] = s.rows.iter().filter(|(i, _)| s.stamps.get(i).copied().unwrap_or(0) > rv).map(|(i, r)| row_json(*i, r, &s.cfg.lang)).collect();
            if s.removed > rv {
                out["ids"] = s.rows.keys().copied().collect();
            }
        }
        out
    }

    // ------------------------------------------------ Dateien
    /// `watched`: von der Ordnerueberwachung, also ohne Musik und ohne Online-Suche nach Unerkanntem.
    pub fn add(self: &Arc<Self>, paths: Vec<PathBuf>, watched: bool) {
        self.add_from(paths, if watched { Origin::Watch } else { Origin::User });
    }

    fn add_from(self: &Arc<Self>, paths: Vec<PathBuf>, from: Origin) {
        if paths.is_empty() {
            return;
        }
        let job = paths.clone();
        let started = self.start("scan", move |a| a.add_now(job, from));
        if !started && from != Origin::Watch {
            let mut s = self.lock();
            s.queue.push((paths, from));
            s.info = "Wird gleich danach hinzugefügt.".into();
            s.v += 1;
        }
    }

    fn add_now(self: &Arc<Self>, paths: Vec<PathBuf>, from: Origin) {
        let (arcs, mut paths): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| archive::is_archive(p));
        let mut notes = vec![];
        for (i, arc) in arcs.iter().enumerate() {
            self.lock().job = "extract";
            self.step(i, arcs.len(), &file_name(arc));
            match archive::extract(arc, arc.parent().and_then(|p| self.dialogs.free(p)), &|| self.running()) {
                Ok(dir) => paths.push(dir),
                Err(e) => notes.push(e),
            }
            if !self.running() {
                break;
            }
        }
        self.lock().job = "scan";
        self.step(0, 0, "Lese Ordner");
        let direct = !arcs.is_empty() || paths.iter().any(|p| p.is_file());
        let min = if direct { 0 } else { self.cfg().min_size_mb * 1024 * 1024 };
        let found = scan_report(&paths, min, from == Origin::User);
        let have: BTreeSet<PathBuf> = self.lock().rows.values().map(|r| r.src.clone()).collect();
        let fresh: Vec<PathBuf> = found.files.into_iter().filter(|p| !have.contains(p)).collect();
        let (mut ids, total) = (vec![], fresh.len());
        for (n, part) in fresh.chunks(50).enumerate() {
            if !self.running() {
                break;
            }
            self.step(n * 50, total, "Lese Dateien");
            let rows: Vec<Row> = part.iter().filter_map(|p| Row::new(p).ok()).collect(); // liest Musik-Tags, ohne Sperre
            let mut s = self.lock();
            for mut r in rows {
                r.auto = from == Origin::Watch;
                let id = s.next;
                s.next += 1;
                s.rows.insert(id, r);
                s.mark(id);
                ids.push(id);
            }
        }
        if from != Origin::Watch {
            if found.archives > 0 {
                notes.push(format!("{} Archive im Ordner nicht entpackt, dafür einzeln hinzufügen", found.archives));
            }
            if found.small > 0 {
                notes.push(format!("{} kleine Videos übersprungen (unter {} MB)", found.small, self.cfg().min_size_mb));
            }
            if ids.is_empty() && notes.is_empty() {
                notes.push("Nichts Neues gefunden.".into());
            }
        }
        if !ids.is_empty() && self.running() {
            self.match_now(ids, false);
        }
        if !notes.is_empty() {
            let mut s = self.lock();
            if !s.info.is_empty() {
                notes.push(s.info.clone());
            }
            s.info = notes.join("; ");
        }
    }

    // ------------------------------------------------ Abgleich
    fn match_ids(self: &Arc<Self>, ids: Vec<u64>, fresh: bool) {
        if !ids.is_empty() {
            self.start("match", move |a| a.match_now(ids, fresh));
        }
    }

    fn match_now(self: &Arc<Self>, ids: Vec<u64>, fresh: bool) {
        self.lock().job = "match";
        if !self.reachable() {
            return;
        }
        let (finder, cfg) = (self.finder(), self.cfg());
        let mut at: HashMap<PathBuf, u64> = HashMap::new();
        let mut rows: Vec<Row> = {
            let s = self.lock();
            ids.iter().filter_map(|i| s.rows.get(i).filter(|r| r.status != Status::Done).map(|r| (*i, r.clone()))).collect::<Vec<_>>()
        }
        .into_iter()
        .map(|(i, r)| {
            at.insert(r.src.clone(), i);
            r
        })
        .collect();
        let put = |r: Row| {
            let Some(i) = at.get(&r.src).copied() else { return };
            let mut s = self.lock();
            if let Some(slot) = s.rows.get_mut(&i) {
                *slot = r;
                s.mark(i);
            }
        };
        sources::FRESH.store(fresh, Ordering::SeqCst);
        match_rows(&mut rows, &cfg, &finder, &|i, n, t| self.step(i, n, t), &|r| put(r.clone()));
        sources::FRESH.store(false, Ordering::SeqCst);
        let mut s = self.lock();
        s.duplicates();
        s.info = if self.running() { finder.seen_errors() } else { "Abgebrochen".into() };
    }

    /// Zeilen aendern und Ziele neu berechnen, ohne neue Online-Suche.
    fn rebuild(&self, ids: &[u64], edit: impl Fn(usize, &mut Row)) {
        if !self.reachable() {
            return;
        }
        let cfg = self.cfg();
        let (tv, mv) = self.libraries(&cfg);
        let mut rows: Vec<(u64, Row)> = {
            let s = self.lock();
            ids.iter().filter_map(|i| s.rows.get(i).filter(|r| r.status != Status::Done).map(|r| (*i, r.clone()))).collect()
        };
        for (n, (_, r)) in rows.iter_mut().enumerate() {
            edit(n, r);
            resolve(r, &cfg, &tv, &mv);
            if (r.approved || r.confirmed) && matches!(r.status, Status::Safe | Status::Local) {
                r.checked = true;
            }
        }
        let mut s = self.lock();
        for (i, r) in rows {
            // inzwischen Uebertragenes nicht mit einem alten Stand ueberschreiben
            if let Some(slot) = s.rows.get_mut(&i).filter(|x| x.status != Status::Done) {
                *slot = r;
                s.mark(i);
            }
        }
        s.duplicates();
    }

    /// Kandidaten neu setzen. `by_search`: nur Vorschlag, gilt erst nach Bestaetigung.
    fn set_cands(self: &Arc<Self>, name: &'static str, ids: Vec<u64>, by_search: bool, get: impl FnOnce(&Finder) -> (Vec<Cand>, usize) + Send + 'static) {
        self.start(name, move |a| {
            let f = a.finder();
            let (mut cands, pick) = get(&f);
            let Some(c) = cands.get_mut(pick) else {
                a.lock().info = format!("Nichts gefunden. {}", f.errors());
                return;
            };
            if c.kind() == Kind::Tv {
                f.load_episodes(c, &a.seasons(&ids));
            }
            let cands = Arc::new(cands);
            a.rebuild(&ids, |_, r| {
                (r.cands, r.pick, r.manual, r.source_error) = (cands.clone(), Some(pick), None, String::new());
                (r.approved, r.confirmed, r.searched) = (!by_search, false, by_search);
                r.auto = false;
            });
        });
    }

    fn seasons(&self, ids: &[u64]) -> BTreeSet<u32> {
        let s = self.lock();
        let mut out: BTreeSet<u32> = ids.iter().filter_map(|i| s.rows.get(i)?.episode().0).collect();
        if out.is_empty() {
            out.insert(1);
        }
        out
    }

    /// Staffel und Folge von Hand; mehrere Dateien werden der Reihe nach durchgezaehlt.
    fn set_episode(self: &Arc<Self>, ids: Vec<u64>, season: u32, first: u32) {
        let Some(row) = ids.first().and_then(|i| self.lock().rows.get(i).cloned()) else { return };
        self.start("episode", move |a| {
            let f = a.finder();
            let (mut cands, pick) = match (row.pick, row.cand().is_some_and(|c| c.kind() == Kind::Tv)) {
                (Some(p), true) => (row.cands.to_vec(), Some(p)),
                _ => {
                    let found = f.find(Kind::Tv, &row.media.title, row.media.year);
                    let p = (!found.is_empty() && found[0].score >= 0.6).then_some(0);
                    (found, p)
                }
            };
            if let Some(p) = pick {
                cands[p].episodes.clear();
                f.load_episodes(&mut cands[p], &BTreeSet::from([season]));
            }
            let cands = Arc::new(cands);
            a.rebuild(&ids, |n, r| {
                if pick.is_some() {
                    (r.cands, r.pick) = (cands.clone(), pick);
                }
                let per = r.media.episodes.len().max(1) as u32;
                r.by_hand = Some((season, first + n as u32 * per));
            });
        });
    }

    fn rename(&self, id: u64, name: &str) {
        let clean = short(&safe_name(name), 200);
        let cfg = self.cfg();
        let rel = {
            let s = self.lock();
            let Some(r) = s.rows.get(&id).filter(|r| !clean.is_empty() && r.status.checkable()) else { return };
            r.dst.as_ref().and_then(|d| d.parent()?.strip_prefix(cfg.root(r.kind())).ok().map(|p| p.join(&clean)))
        };
        if let Some(rel) = rel {
            self.rebuild(&[id], |_, r| (r.manual, r.confirmed) = (Some(rel.clone()), true));
        }
    }

    fn open_rows(&self, waiting: bool) -> Vec<u64> {
        let s = self.lock();
        let fits = |r: &Row| r.status != Status::Done && (r.status == Status::None && r.note.is_empty()) == waiting;
        s.rows.iter().filter(|(_, r)| fits(r)).map(|(i, _)| *i).collect()
    }

    /// Nach geaenderten Ordnern, Vorlagen oder Sprache: alles Offene neu ausrichten.
    fn realign(self: &Arc<Self>) {
        let a = self.clone();
        std::thread::spawn(move || {
            a.check_plex();
            a.rebuild(&a.open_rows(false), |_, _| {});
            a.match_ids(a.open_rows(true), false);
        });
    }

    // ------------------------------------------------ Uebertragen
    /// Uebertraegt genau die im Dialog gezeigten Zeilen, mit genau dem gezeigten Ziel.
    fn transfer(self: &Arc<Self>, list: &Value, action: &str) -> String {
        if !matches!(action, "copy" | "move") {
            return "Unbekannte Aktion".into();
        }
        let cfg = self.cfg();
        let mut todo: Vec<(u64, u64, PathBuf, Kind)> = vec![];
        {
            let s = self.lock();
            for x in list.as_array().into_iter().flatten() {
                let row = x["id"].as_u64().and_then(|i| Some((i, s.rows.get(&i)?)));
                let Some((i, r)) = row.filter(|(_, r)| r.checked && r.status.checkable()) else {
                    return "Die Auswahl hat sich geändert, bitte noch einmal prüfen.".into();
                };
                let Some(dst) = r.dst.clone().filter(|d| x["dst"].as_str() == Some(&text_of(d))) else {
                    return "Ein Ziel hat sich geändert, bitte noch einmal prüfen.".into();
                };
                todo.push((i, r.size, dst, r.kind()));
            }
        }
        if todo.is_empty() || !self.reachable() {
            return String::new();
        }
        // Platz je Laufwerk, nicht je Ordner
        let mut need: HashMap<String, (u64, PathBuf)> = HashMap::new();
        for (_, size, _, kind) in &todo {
            let root = cfg.root(*kind);
            need.entry(self.dialogs.volume(root)).or_insert((0, root.into())).0 += size;
        }
        let mut pools: HashMap<u64, (u64, PathBuf)> = HashMap::new(); // gleiche Freigabe unter zwei Namen
        for (n, root) in need.into_values() {
            if let Some(free) = self.dialogs.free(&root) {
                pools.entry(free).or_insert((0, root)).0 += n;
            }
        }
        for (free, (n, root)) in pools {
            if n + copy::RESERVE > free {
                return format!("Zu wenig Platz auf {}: {:.1} GB nötig.", root.display(), n as f64 / 1e9);
            }
        }
        let action = action.to_string();
        let started = self.start("transfer", move |a| {
            let (mut ok, mut bad, total) = (0, 0, todo.len());
            let mut why = String::new();
            for (n, (id, size, dst, _)) in todo.into_iter().enumerate() {
                if !a.running() {
                    why = ", Rest abgebrochen".into();
                    break;
                }
                {
                    let mut s = a.lock();
                    s.meter = Meter { i: n, n: total, text: file_name(&dst), phase: "copy".into(), total: size, ..Default::default() };
                    s.v += 1;
                }
                let Some(mut row) = a.lock().rows.get(&id).cloned() else { continue };
                if let Some(reason) = occupied(&row, &a.cfg()) {
                    (row.status, row.note, row.checked) = (Status::Busy, reason, false);
                    bad += 1;
                    a.put(id, row);
                    continue;
                }
                let result = execute(&mut row, &action, &mut |e| add_history(&e), &a.meter());
                match result {
                    Ok(()) => {
                        ok += 1;
                        if let Some((key, alias)) = learned(&row) {
                            a.edit_aliases(|known| {
                                known.insert(key, alias);
                            });
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        why = ", abgebrochen (angefangene Datei wird beim nächsten Mal fortgesetzt)".into();
                        row.note = "abgebrochen".into();
                        a.put(id, row);
                        break;
                    }
                    Err(e) => {
                        bad += 1;
                        let text = if copy::in_use(&e) { "Datei oder Teilkopie wird gerade benutzt".into() } else { e.to_string() };
                        (row.status, row.note, row.checked) = (Status::Error, text, false);
                        if copy::disk_full(&e) {
                            why = ", Ziel ist voll".into();
                            a.put(id, row);
                            break;
                        }
                    }
                }
                a.put(id, row);
            }
            let tail = if bad > 0 { format!(", {bad} nicht übertragen") } else { String::new() };
            a.lock().info = format!("{ok} übertragen und geprüft{tail}{why}");
            a.check_plex();
        });
        if started {
            String::new()
        } else {
            "Es läuft gerade noch etwas anderes.".into()
        }
    }

    fn put(&self, id: u64, row: Row) {
        let mut s = self.lock();
        if let Some(slot) = s.rows.get_mut(&id) {
            *slot = row;
            s.mark(id);
        }
    }

    /// Fortschrittsanzeige; gibt false zurueck, sobald abgebrochen wurde.
    fn meter(&self) -> impl Fn(&str, u64, u64) -> bool + Sync + '_ {
        let clock = Mutex::new((Instant::now(), Instant::now(), 0u64, String::new()));
        move |phase: &str, pos: u64, all: u64| {
            let mut c = clock.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if c.3 != phase {
                *c = (now, now, pos, phase.into());
            } else if now - c.1 < Duration::from_millis(200) && pos < all {
                return self.running();
            }
            c.1 = now;
            let secs = (now - c.0).as_secs_f64();
            let speed = if secs > 0.3 { pos.saturating_sub(c.2) as f64 / secs } else { 0.0 };
            let mut s = self.lock();
            s.v += 1;
            (s.meter.phase, s.meter.bytes, s.meter.total, s.meter.speed) = (phase.into(), pos, all, speed);
            drop(s);
            self.running()
        }
    }

    fn undo(self: &Arc<Self>, i: usize) -> String {
        let rows = read_history();
        let Some(e) = rows.get(i).filter(|e| e.aktion != "undo").cloned() else {
            return "Dieser Eintrag lässt sich nicht rückgängig machen.".into();
        };
        let started = self.start("undo", move |a| {
            a.step(0, 1, &file_name(Path::new(&e.ziel)));
            let res = undo(&e, &a.cfg(), &mut |x| add_history(&x), &a.meter());
            if res.is_ok() && !e.alias.is_empty() {
                a.edit_aliases(|k| {
                    k.remove(&e.alias);
                });
            }
            a.lock().info = res.unwrap_or_else(|x| x.to_string());
        });
        if started {
            String::new()
        } else {
            "Es läuft gerade noch etwas anderes.".into()
        }
    }

    // ------------------------------------------------ Episodenlisten
    fn episodes_search(&self, text: &str) -> Value {
        let f = self.finder();
        let (title, year) = clean_title(text.trim());
        let mut found = f.find(Kind::Tv, if title.is_empty() { text.trim() } else { &title }, year);
        found.truncate(12);
        let lang = &f.lang;
        let shows: Vec<Value> =
            found.iter().map(|c| json!({ "title": c.title(lang), "year": c.year(), "poster": c.poster(), "sources": c.sources() })).collect();
        self.lock().shows = found;
        json!({ "shows": shows, "note": f.errors() })
    }

    fn episodes_load(&self, i: usize) -> Value {
        let Some(mut c) = self.lock().shows.get(i).cloned() else { return Value::Null };
        let f = self.finder();
        f.load_episodes(&mut c, &(1..=15).collect());
        let keys: BTreeSet<(u32, u32)> = c.episodes.iter().flat_map(|e| e.1.keys().copied()).collect();
        let name = |id: &str| self.sources.iter().find(|s| s.id() == id).map(|s| s.name().to_string()).unwrap_or_default();
        let rows: Vec<Value> = keys
            .iter()
            .map(|k| json!({ "s": k.0, "e": k.1, "t": c.episodes.iter().map(|e| e.1.get(k).cloned().unwrap_or_default()).collect::<Vec<_>>() }))
            .collect();
        json!({ "cols": c.episodes.iter().map(|e| name(&e.0)).collect::<Vec<_>>(), "rows": rows, "title": c.label(&f.lang), "overview": c.overview() })
    }

    // ------------------------------------------------ Pruefsummen
    pub fn sums_add(self: &Arc<Self>, paths: Vec<PathBuf>) {
        let mut todo: Vec<(PathBuf, String)> = vec![];
        for p in paths {
            let ext = ext_of(&p);
            if ext == "sfv" || ext == "sha256" {
                let text = fs::read(&p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(';')) {
                    let cut = if ext == "sfv" { line.rsplit_once(' ') } else { line.split_once(' ').map(|(a, b)| (b, a)) };
                    let Some((name, want)) = cut else { continue };
                    let name = name.trim().trim_start_matches('*');
                    // nur Dateien neben der Liste; fremde Listen duerfen nirgendwo sonst hinzeigen
                    if Path::new(name).components().all(|c| matches!(c, Component::Normal(_))) {
                        todo.push((p.with_file_name("").join(name), want.to_lowercase()));
                    } else {
                        self.lock().sums.push(json!({ "file": name, "crc": "", "sha": "", "result": "fremd" }));
                    }
                }
            } else if p.is_file() {
                todo.push((p, String::new()));
            }
        }
        let base = {
            let mut s = self.lock();
            let base = s.sums.len();
            s.sums.extend(todo.iter().map(|(p, _)| json!({ "file": file_name(p), "crc": "", "sha": "", "result": "rechnet" })));
            base
        };
        let a = self.clone();
        std::thread::spawn(move || {
            for (i, (p, want)) in todo.into_iter().enumerate() {
                let out = match sums_of(&p) {
                    Ok((crc, sha)) => {
                        let result = match want.as_str() {
                            "" => "berechnet",
                            w if w == crc || w == sha => "stimmt",
                            _ => "abweichend",
                        };
                        json!({ "file": file_name(&p), "crc": crc, "sha": sha, "result": result })
                    }
                    Err(_) => json!({ "file": file_name(&p), "crc": "", "sha": "", "result": "fehlt" }),
                };
                if let Some(slot) = a.lock().sums.get_mut(base + i).filter(|s| s["file"] == out["file"]) {
                    *slot = out;
                }
            }
        });
    }

    // ------------------------------------------------ Untertitel
    pub fn subs_add(&self, paths: Vec<PathBuf>) {
        let ok: Vec<PathBuf> = paths.into_iter().filter(|p| is_video(p) && p.is_file()).collect();
        let mut s = self.lock();
        for path in ok {
            if !s.subs.iter().any(|x| x.path == path) {
                s.subs.push(SubItem { path, hits: vec![], note: String::new() });
            }
        }
    }

    fn subs_list(&self) -> Value {
        let items = self.lock().subs.clone();
        let have = |p: &Path| sidecars(p).into_iter().map(|x| x.1.trim_start_matches('.').to_string()).collect::<Vec<_>>();
        items
            .iter()
            .map(|x| {
                json!({ "file": file_name(&x.path), "path": text_of(&x.path), "have": have(&x.path), "note": x.note,
                        "hits": x.hits.iter().map(subs::hit_json).collect::<Vec<_>>() })
            })
            .collect()
    }

    fn subs_do(&self, i: usize, work: impl FnOnce(&Path, &str) -> Result<(Vec<SubHit>, String), String>) {
        let (path, key) = {
            let s = self.lock();
            let Some(x) = s.subs.get(i) else { return };
            (x.path.clone(), s.sub_key.clone())
        };
        let (hits, note) = work(&path, &key).unwrap_or_else(|e| (vec![], e));
        if let Some(x) = self.lock().subs.iter_mut().find(|x| x.path == path) {
            (x.hits, x.note) = (hits, note);
        }
    }

    // ------------------------------------------------ Aufrufe der Oberflaeche
    pub fn call(self: &Arc<Self>, method: &str, args: &[Value]) -> Value {
        let text = |i: usize| args.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        let int = |i: usize| args.get(i).and_then(Value::as_u64).unwrap_or(0);
        let flag = |i: usize| args.get(i).and_then(Value::as_bool).unwrap_or(false);
        let ids = || ids_of(args.first());
        let idle = !self.busy();
        match method {
            "state" => return self.state(int(0), int(1)),
            "stop" => {
                self.stop.store(true, Ordering::SeqCst);
                self.say("Wird abgebrochen …");
            }
            "add_files" => self.add(self.dialogs.files(), false),
            "add_folder" => self.add(self.dialogs.folder().into_iter().collect(), false),
            "add_sources" => {
                let dirs: Vec<PathBuf> = self.cfg().folders.iter().map(PathBuf::from).filter(|p| p.is_dir()).collect();
                if dirs.is_empty() {
                    return json!("keine");
                }
                self.add_from(dirs, Origin::Folders);
            }
            "reveal" => {
                let dir = self.lock().rows.get(&int(0)).and_then(|r| r.src.parent().map(text_of));
                if let Some(d) = dir {
                    self.dialogs.open(&d);
                }
            }
            "link" => {
                if let Some(l) = LINKS.iter().find(|l| l.0 == text(0)) {
                    self.dialogs.open(l.1);
                }
            }
            "key_page" if text(0) == SUB_ID => self.dialogs.open(subs::KEY_URL),
            "key_page" => {
                if let Some(s) = self.sources.iter().find(|s| s.id() == text(0)) {
                    self.dialogs.open(s.key_url());
                }
            }
            "check" => {
                let mut s = self.lock();
                for i in ids() {
                    if let Some(r) = s.rows.get_mut(&i).filter(|r| r.dst.is_some() && r.status.checkable()) {
                        r.checked = flag(1);
                        s.mark(i);
                    }
                }
            }
            "episodes_search" => return self.episodes_search(&text(0)),
            "episodes_load" => return self.episodes_load(int(0) as usize),
            "sums" => return json!(self.lock().sums),
            "sums_clear" => self.lock().sums.clear(),
            "sums_add" => self.sums_add(self.dialogs.files()),
            "subs" => return self.subs_list(),
            "subs_clear" => self.lock().subs.clear(),
            "subs_add" => self.subs_add(self.dialogs.files()),
            "subs_recent" => {
                let mut list: Vec<PathBuf> = read_history().iter().rev().filter(|e| e.aktion != "undo").map(|e| PathBuf::from(&e.ziel)).collect();
                list.truncate(30);
                self.subs_add(list);
            }
            "subs_search" => {
                let lang = text(1);
                self.subs_do(int(0) as usize, |p, key| {
                    let hits = subs::search(subs::BASE, key, p, &lang)?;
                    let note = if hits.is_empty() { "Nichts gefunden".into() } else { String::new() };
                    Ok((hits, note))
                });
                return self.subs_list();
            }
            "subs_load" => {
                let (id, lang) = (int(1), text(2));
                self.subs_do(int(0) as usize, |p, key| {
                    let f = subs::download(subs::BASE, key, p, id, &lang)?;
                    let _ = add_history(&subs::entry(&f));
                    Ok((vec![], format!("Gespeichert: {}", file_name(&f))))
                });
                return self.subs_list();
            }
            "history" => {
                let rows = read_history();
                let undone: BTreeSet<(&str, &str)> = rows.iter().filter(|e| e.aktion == "undo").map(|e| (e.ziel.as_str(), e.sha256.as_str())).collect();
                return rows
                    .iter()
                    .enumerate()
                    .rev()
                    .take(500)
                    .map(|(i, e)| {
                        json!({ "i": i, "zeit": e.zeit, "aktion": e.aktion, "quelle": e.quelle, "ziel": e.ziel, "bytes": e.bytes,
                                "name": file_name(Path::new(&e.ziel)), "undone": undone.contains(&(e.ziel.as_str(), e.sha256.as_str())) })
                    })
                    .collect();
            }
            "aliases" => {
                let a: BTreeMap<_, _> = self.aliases().into_iter().collect();
                return a.into_iter().map(|(k, v)| json!({ "key": k, "folder": v.folder, "sure": !v.ids.is_empty() })).collect();
            }
            "alias_delete" => self.edit_aliases(|a| {
                a.remove(&text(0));
            }),
            "partials" => {
                let cfg = self.cfg();
                let list = copy::leftovers(&[&cfg.tv, &cfg.movies, &cfg.music].map(PathBuf::from));
                return list.iter().map(|p| json!({ "path": text_of(p), "bytes": fs::metadata(p).map(|m| m.len()).unwrap_or(0) })).collect();
            }
            "partial_delete" if idle => {
                let (cfg, p) = (self.cfg(), PathBuf::from(text(0)));
                if copy::leftovers(&[&cfg.tv, &cfg.movies, &cfg.music].map(PathBuf::from)).contains(&p) {
                    return json!(fs::remove_file(&p).err().map(|e| e.to_string()).unwrap_or_default());
                }
            }
            "cache_clear" => sources::clear_cache(),
            "find_plex" => {
                let path = |p: &Option<PathBuf>| p.as_deref().map(text_of).unwrap_or_default();
                return library::search_drives(Duration::from_secs(5))
                    .iter()
                    .map(|f| json!({ "tv": path(&f.tv), "movies": path(&f.movies), "music": path(&f.music) }))
                    .collect();
            }
            "choose_root" => {
                let Some(p) = self.dialogs.folder() else { return Value::Null };
                let f = library::inside(&p);
                let mut none = true;
                for (key, dir) in [("tv", f.tv), ("movies", f.movies), ("music", f.music)] {
                    if let Some(d) = dir {
                        none = false;
                        let m = self.set(key, &json!(text_of(&d)));
                        if !m.is_empty() {
                            return json!(m);
                        }
                    }
                }
                if none {
                    return json!("Darin habe ich keine Ordner für Serien, Filme oder Musik erkannt. Bitte einzeln wählen.");
                }
            }
            "set" => return json!(self.set(&text(0), args.get(1).unwrap_or(&Value::Null))),
            "choose_dir" => {
                if let Some(p) = self.dialogs.folder() {
                    return json!(self.set(&text(0), &json!(text_of(&p))));
                }
            }
            "add_watch" => {
                if let Some(p) = self.dialogs.folder().map(|p| text_of(&p)) {
                    self.change(|c| {
                        if !c.folders.contains(&p) {
                            c.folders.push(p);
                        }
                    });
                }
            }
            "remove_watch" => self.change(|c| c.folders.retain(|f| *f != text(0))),
            "save_key" => return json!(self.save_key(&text(0), &text(1))),
            "undo" => return json!(self.undo(int(0) as usize)),
            "transfer" => return json!(self.transfer(args.first().unwrap_or(&Value::Null), &text(1))),
            // Ab hier nur, wenn nichts laeuft
            _ if !idle => return json!("Es läuft gerade noch etwas anderes."),
            "clear" | "remove" => {
                let gone = ids();
                let mut s = self.lock();
                s.rows.retain(|i, _| method == "remove" && !gone.contains(i));
                s.info.clear();
                s.v += 1;
                s.rv += 1;
                s.removed = s.rv;
                s.duplicates();
            }
            "match" => {
                let all: Vec<u64> = self.lock().rows.iter().filter(|(_, r)| r.status != Status::Done && (flag(0) || !decided(r))).map(|(i, _)| *i).collect();
                self.match_ids(all, true);
            }
            "toggle_source" => {
                let (id, on) = (text(0), flag(1));
                if self.sources.iter().any(|s| s.id() == id && s.ready()) {
                    self.change(|c| {
                        c.active.retain(|x| *x != id);
                        if on {
                            c.active.push(id);
                        }
                    });
                    let all: Vec<u64> = self.lock().rows.iter().filter(|(_, r)| r.status != Status::Done && !decided(r)).map(|(i, _)| *i).collect();
                    self.match_ids(all, false);
                }
            }
            "approve" => {
                let list: Vec<u64> = {
                    let s = self.lock();
                    let open = |i: &u64| s.rows.get(i).is_some_and(|r| matches!(r.status, Status::Check | Status::Local | Status::Busy));
                    ids().into_iter().filter(open).collect()
                };
                self.rebuild(&list, |_, r| {
                    if r.status == Status::Busy {
                        r.rested = true; // nur die Wartezeit, nicht den Titel
                    } else {
                        (r.approved, r.confirmed, r.searched) = (true, true, false);
                    }
                });
            }
            "pick" => {
                let (list, index) = (ids(), int(1) as usize);
                let Some(cands) = list.first().and_then(|i| self.lock().rows.get(i).map(|r| r.cands.clone())) else { return Value::Null };
                let same: Vec<u64> = {
                    let s = self.lock();
                    list.into_iter().filter(|i| s.rows.get(i).is_some_and(|r| Arc::ptr_eq(&r.cands, &cands))).collect()
                };
                self.set_cands("pick", same, false, move |_| (cands.to_vec(), index));
            }
            "search" => {
                let list = ids();
                let (title, year) = clean_title(text(1).trim());
                let what = if title.is_empty() { text(1).trim().to_string() } else { title };
                if let Some(kind) = Kind::named(&text(2)).filter(|_| !what.is_empty() && !list.is_empty()) {
                    self.set_cands("search", list, true, move |f| {
                        let mut found = f.find(kind, &what, year);
                        found.truncate(8);
                        (found, 0)
                    });
                }
            }
            "set_episode" => self.set_episode(ids(), int(1) as u32, int(2) as u32),
            "rename" => self.rename(int(0), &text(1)),
            _ => {}
        }
        Value::Null
    }

    fn set(self: &Arc<Self>, key: &str, value: &Value) -> String {
        let text = value.as_str().unwrap_or("").trim().to_string();
        if self.busy() && matches!(key, "tv" | "movies" | "music" | "format_tv" | "format_movie") {
            return "Bitte warten, bis der laufende Vorgang fertig ist.".into();
        }
        match key {
            "format_tv" | "format_movie" => {
                if let Err(e) = render(&text, &example(key)) {
                    return e;
                }
                self.change(|c| *(if key == "format_tv" { &mut c.format_tv } else { &mut c.format_movie }) = text);
                self.realign();
            }
            "tv" | "movies" | "music" => {
                let text = match library::check(&text) {
                    Ok(t) => t,
                    Err(_) if text.is_empty() => text, // abwaehlen ist erlaubt
                    Err(e) => return e,
                };
                self.change(|c| {
                    *(match key {
                        "tv" => &mut c.tv,
                        "music" => &mut c.music,
                        _ => &mut c.movies,
                    }) = text
                });
                self.realign();
            }
            "lang" if matches!(text.as_str(), "de" | "en" | "orig") => {
                self.change(|c| c.lang = text);
                self.lock().mark_all();
                self.realign();
            }
            "watch" => self.change(|c| c.watch = value.as_bool().unwrap_or(false)),
            "wiki_posters" => self.change(|c| c.wiki_posters = value.as_bool().unwrap_or(false)),
            "min_size_mb" | "min_age_minutes" => {
                let Some(n) = value.as_u64().or_else(|| text.parse().ok()).filter(|n| *n <= 100_000) else { return "Bitte eine Zahl eingeben".into() };
                self.change(|c| *(if key == "min_size_mb" { &mut c.min_size_mb } else { &mut c.min_age_minutes }) = n);
            }
            _ => return "Ungültige Einstellung".into(),
        }
        String::new()
    }

    fn store_keys(&self) -> Result<(), String> {
        let mut keys: HashMap<String, String> = self.sources.iter().map(|s| (s.id().to_string(), s.key())).collect();
        keys.insert(SUB_ID.into(), self.lock().sub_key.clone());
        let res = sources::save_keys(&keys);
        self.lock().v += 1;
        res
    }

    fn save_key(&self, id: &str, key: &str) -> String {
        // Wechselnder Titel umgeht den Zwischenspeicher, damit der Schluessel wirklich geprueft wird
        let n = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() % 1000).unwrap_or(0);
        if id == SUB_ID {
            let key = key.trim();
            if !key.is_empty() {
                if let Err(e) = subs::search(subs::BASE, key, Path::new(&format!("Matrix {n} (1999).mkv")), "de") {
                    return format!("{e}. Nicht gespeichert.");
                }
            }
            self.lock().sub_key = key.into();
            return self.store_keys().err().unwrap_or_default();
        }
        let Some(s) = self.sources.iter().find(|s| s.id() == id && s.needs_key()) else { return "Unbekannte Quelle".into() };
        let old = s.key();
        s.set_key(key);
        if !s.key().is_empty() {
            if let Err(e) = s.search(Kind::Movie, &format!("Matrix {n}"), "de") {
                s.set_key(&old);
                return format!("{e}. Nicht gespeichert.");
            }
        }
        if let Err(e) = self.store_keys() {
            return e;
        }
        self.change(|c| {
            if !c.active.iter().any(|a| a == id) {
                c.active.push(id.into());
            }
        });
        String::new()
    }
}

fn preview(cfg: &Settings, key: &str) -> String {
    let fmt = if key == "format_tv" { &cfg.format_tv } else { &cfg.format_movie };
    match render(fmt, &example(key)) {
        Ok(p) => format!("{}.mkv", p.display()),
        Err(e) => e,
    }
}

fn sums_of(p: &Path) -> std::io::Result<(String, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let (mut f, mut sha, mut crc) = (fs::File::open(p)?, Sha256::new(), crc32fast::Hasher::new());
    let mut buf = vec![0u8; copy::CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok((format!("{:08x}", crc.finalize()), copy::hex(&sha.finalize())));
        }
        sha.update(&buf[..n]);
        crc.update(&buf[..n]);
    }
}

fn row_json(id: u64, r: &Row, lang: &str) -> Value {
    let c = r.cand();
    let m = &r.media;
    let (season, eps) = r.episode();
    let album = |t: &crate::music::Track| if t.album.is_empty() { t.artist.clone() } else { format!("{} – {}", t.artist, t.album) };
    json!({
        "id": id, "file": file_name(&r.src), "size": r.size, "status": r.status.as_str(), "note": r.note, "checked": r.checked,
        "checkable": r.dst.is_some() && r.status.checkable(),
        "name": r.dst.as_ref().map(|d| stem_of(d)).unwrap_or_default(),
        "dst": r.dst.as_ref().map(|d| text_of(d)).unwrap_or_default(),
        "folder": r.folder, "season": season, "episode": eps.first(),
        "title": c.map(|c| c.title(lang).to_string()).or_else(|| r.track.as_ref().map(album)).unwrap_or_else(|| if m.title.is_empty() { stem_of(&r.src) } else { m.title.clone() }),
        "year": c.map_or(m.year.or(r.track.as_ref().and_then(|t| t.year)), |c| c.year()), "kind": r.kind().as_str(), "query": m.title,
        "poster": c.map_or("", |c| c.poster()), "overview": c.map_or("", |c| c.overview()),
        "sources": c.map(|c| c.sources()).unwrap_or_default(), "pick": r.pick,
        "waiting": r.status == Status::None && r.note.is_empty(),
        "cands": r.cands.iter().map(|x| json!({ "label": x.label(lang), "score": (x.score * 100.0).round() })).collect::<Vec<_>>(),
    })
}
