//! Abgleich: Dateien + Datenquellen -> Zielnamen, Ausfuehren, Verlauf.
use crate::copy::{self, Progress};
use crate::music::{self, Track};
use crate::names::*;
use crate::sources::{Episodes, Hit, Source, Sources, DATA, ORDER};
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Safe,
    Check,
    Local,
    None,
    Exists,
    Conflict,
    /// vorhandene Datei ausdruecklich ersetzen
    Replace,
    Busy,
    Done,
    Error,
}
use Status::*;

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Safe => "sicher",
            Check => "pruefen",
            Local => "lokal",
            Status::None => "kein",
            Exists => "vorhanden",
            Conflict => "konflikt",
            Replace => "ersetzen",
            Busy => "laeuft",
            Done => "fertig",
            Error => "fehler",
        }
    }
    pub fn checkable(self) -> bool {
        matches!(self, Safe | Check | Local | Error | Replace)
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Settings {
    pub tv: String,
    pub movies: String,
    pub music: String,
    pub lang: String,
    pub format_tv: String,
    pub format_movie: String,
    pub active: Vec<String>,
    pub folders: Vec<String>,
    pub watch: bool,
    pub wiki_posters: bool,
    pub min_age_minutes: u64,
    pub min_size_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            tv: String::new(),
            movies: String::new(),
            music: String::new(),
            lang: "en".into(),
            format_tv: "{plex}".into(),
            format_movie: "{plex}".into(),
            active: ORDER.iter().map(|s| s.to_string()).collect(),
            folders: vec![],
            watch: false,
            wiki_posters: false,
            min_age_minutes: 10,
            min_size_mb: 40,
        }
    }
}

impl Settings {
    pub fn root(&self, kind: Kind) -> &Path {
        Path::new(match kind {
            Kind::Tv => &self.tv,
            Kind::Music => &self.music,
            _ => &self.movies,
        })
    }
    pub fn file() -> PathBuf {
        DATA.join("settings.json")
    }
    pub fn load() -> Settings {
        read_json(&Self::file()).unwrap_or_default()
    }
    pub fn save(&self) -> io::Result<()> {
        fs::create_dir_all(&*DATA)?;
        write_json(&Self::file(), self)
    }
}

/// Beschaedigte Dateien werden beiseitegelegt statt still ueberschrieben.
pub fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Option<T> {
    let bytes = fs::read(p).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(_) => {
            let _ = fs::rename(p, p.with_extension("json.bad"));
            Option::None
        }
    }
}

pub fn write_json<T: Serialize>(p: &Path, v: &T) -> io::Result<()> {
    fs::create_dir_all(&*DATA)?;
    let tmp = p.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    fs::rename(tmp, p)
}

// ---------------------------------------------------------------- Kandidaten

/// Derselbe Film/dieselbe Serie, zusammengefasst ueber mehrere Quellen.
#[derive(Clone, Debug)]
pub struct Cand {
    pub hits: Vec<Hit>,
    pub score: f64,
    pub episodes: Vec<(String, Episodes)>,
    /// nur ueber eine gelernte, nicht bestaetigte Zuordnung gefunden
    pub via_alias: bool,
}

fn unique<T: PartialEq>(it: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out = vec![];
    for x in it {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

impl Cand {
    pub fn kind(&self) -> Kind {
        self.hits[0].kind
    }
    pub fn year(&self) -> Option<i32> {
        self.hits.iter().find_map(|h| h.year)
    }
    pub fn sources(&self) -> Vec<&str> {
        unique(self.hits.iter().map(|h| h.source.as_str()))
    }
    pub fn poster(&self) -> &str {
        self.hits.iter().map(|h| h.poster.as_str()).find(|p| !p.is_empty()).unwrap_or("")
    }
    pub fn overview(&self) -> &str {
        self.hits.iter().map(|h| h.overview.as_str()).max_by_key(|t| t.len()).unwrap_or("")
    }
    pub fn ids(&self) -> Vec<(String, String)> {
        unique(self.hits.iter().flat_map(|h| h.ids.iter().cloned().chain([(h.source.clone(), h.id.clone())])))
    }
    fn knows(&self, a: &Alias) -> bool {
        self.ids().iter().any(|i| a.ids.contains(i))
    }
    pub fn variants(&self) -> Vec<&str> {
        unique(self.hits.iter().flat_map(|h| h.variants()))
    }
    pub fn title(&self, lang: &str) -> &str {
        let tr = self.hits.iter().flat_map(|h| &h.titles).find(|t| t.0 == lang && !t.1.is_empty());
        tr.map(|t| t.1.as_str()).unwrap_or(&self.hits[0].title)
    }
    pub fn label(&self, lang: &str) -> String {
        format!("{} ({})", self.title(lang), self.year().map_or("?".into(), |y| y.to_string()))
    }
}

fn same(a: &Hit, b: &Hit) -> bool {
    if a.kind != b.kind || a.source == b.source {
        return false;
    }
    let common: Vec<bool> = a.ids.iter().filter_map(|x| b.ids.iter().find(|y| y.0 == x.0).map(|y| y.1 == x.1)).collect();
    if !common.is_empty() {
        return common.contains(&true);
    }
    if let (Some(x), Some(y)) = (a.year, b.year) {
        if (x - y).abs() > 1 {
            return false;
        }
    }
    let names: Vec<String> = b.variants().iter().map(|v| norm(v)).collect();
    a.variants().iter().map(|v| norm(v)).any(|v| !v.is_empty() && names.contains(&v))
}

pub fn merge(hits: Vec<Hit>) -> Vec<Cand> {
    let mut out: Vec<Cand> = vec![];
    for h in hits {
        match out.iter_mut().find(|c| c.hits.iter().any(|x| same(&h, x))) {
            Some(c) => c.hits.push(h),
            Option::None => out.push(Cand { hits: vec![h], score: 0.0, episodes: vec![], via_alias: false }),
        }
    }
    out
}

/// Aehnlichkeit 0..1 nach Ratcliff/Obershelp: laengste gemeinsame Stuecke, rekursiv.
pub fn ratio(a: &str, b: &str) -> f64 {
    fn blocks(a: &[char], b: &[char]) -> usize {
        let (mut best, mut ai, mut bi) = (0, 0, 0);
        let mut prev = vec![0usize; b.len() + 1];
        for (i, ca) in a.iter().enumerate() {
            let mut cur = vec![0usize; b.len() + 1];
            for j in 0..b.len() {
                if *ca == b[j] {
                    cur[j + 1] = prev[j] + 1;
                    if cur[j + 1] > best {
                        (best, ai, bi) = (cur[j + 1], i + 1 - cur[j + 1], j + 1 - cur[j + 1]);
                    }
                }
            }
            prev = cur;
        }
        if best == 0 {
            return 0;
        }
        best + blocks(&a[..ai], &b[..bi]) + blocks(&a[ai + best..], &b[bi + best..])
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    2.0 * blocks(&a, &b) as f64 / (a.len() + b.len()) as f64
}

pub fn score(title: &str, year: Option<i32>, c: &Cand) -> f64 {
    let want = norm(title);
    let names: Vec<String> = c.variants().iter().map(|v| norm(v)).filter(|v| !v.is_empty()).collect();
    let mut s = if names.contains(&want) { 1.0 } else { names.iter().map(|n| ratio(&want, n)).fold(0.0, f64::max) * 0.95 };
    if let Some(y) = year {
        if let Some(diff) = c.hits.iter().filter_map(|h| h.year).map(|h| (h - y).abs()).min() {
            s -= match diff {
                0 => 0.0,
                1 => 0.02,
                _ => 0.4,
            };
        }
    }
    s
}

static COUNTRY_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+(?:UK|US|AU|USA)$").unwrap());

fn queries(media: &Media, aliases: &Aliases) -> Vec<String> {
    let mut out = vec![media.title.clone(), COUNTRY_END.replace(&media.title, "").into_owned()];
    if let Some(a) = aliases.get(&norm(&media.title)) {
        out.push(split_folder(&a.folder).0);
    }
    unique(out.into_iter().filter(|q| !q.is_empty()))
}

/// Fragt alle aktiven Quellen gleichzeitig; Ausfaelle einzelner Quellen stoppen nichts.
pub struct Finder {
    pub sources: Sources,
    pub lang: String,
    pub aliases: Aliases,
    pub cancel: Arc<AtomicBool>,
    errors: Mutex<BTreeMap<String, String>>, // aktuelle Suche
    seen: Arc<Mutex<BTreeMap<String, String>>>, // ganzer Lauf
}

fn lines(m: &Mutex<BTreeMap<String, String>>) -> String {
    m.lock().unwrap().values().cloned().collect::<Vec<_>>().join("; ")
}

impl Finder {
    pub fn new(sources: Sources, lang: &str, aliases: &Aliases) -> Finder {
        Finder { sources, lang: lang.into(), aliases: aliases.clone(), cancel: Default::default(), errors: Default::default(), seen: Default::default() }
    }
    /// Fuer parallele Suchen: eigene aktuelle Fehler, gemeinsame Gesamtliste.
    fn fork(&self) -> Finder {
        Finder { seen: self.seen.clone(), cancel: self.cancel.clone(), ..Finder::new(self.sources.clone(), &self.lang, &self.aliases) }
    }
    pub fn errors(&self) -> String {
        lines(&self.errors)
    }
    pub fn seen_errors(&self) -> String {
        lines(&self.seen)
    }

    fn guard<T>(&self, src: &Arc<dyn Source>, fun: impl FnOnce() -> Result<T, String>) -> Option<T> {
        match fun() {
            Ok(v) => Some(v),
            Err(e) => {
                self.errors.lock().unwrap().insert(src.id().into(), e.clone());
                self.seen.lock().unwrap().insert(src.id().into(), e);
                Option::None
            }
        }
    }

    pub fn find(&self, kind: Kind, title: &str, year: Option<i32>) -> Vec<Cand> {
        self.find_in(kind, title, year, &|_| false)
    }

    /// `known`: gibt es fuer diesen Treffer schon einen Plex-Ordner?
    fn find_in(&self, kind: Kind, title: &str, year: Option<i32>, known: &dyn Fn(&Cand) -> bool) -> Vec<Cand> {
        let usable: Vec<_> = self.sources.iter().filter(|s| s.kinds().contains(&kind) && s.ready()).collect();
        let mut hits: Vec<Hit> = vec![];
        let mut cands: Vec<Cand> = vec![];
        if title.is_empty() {
            return cands;
        }
        for step in 0..3u8 {
            let stage: Vec<_> = usable.iter().filter(|s| s.stage(kind) == step).collect();
            let clear = cands.iter().any(|c| c.score >= 0.97 && (step == 2 || known(c)));
            if stage.is_empty() || clear {
                continue;
            }
            let got: Vec<Vec<Hit>> = std::thread::scope(|sc| {
                let jobs: Vec<_> = stage
                    .iter()
                    .map(|s| sc.spawn(move || self.guard(s, || s.search(kind, title, &self.lang)).unwrap_or_default()))
                    .collect();
                jobs.into_iter().map(|j| j.join().unwrap_or_default()).collect()
            });
            hits.extend(got.into_iter().flatten());
            cands = merge(hits.clone());
            for c in &mut cands {
                c.score = score(title, year, c);
            }
        }
        let rank = |h: &Hit| self.sources.iter().position(|s| s.id() == h.source).unwrap_or(99);
        for c in &mut cands {
            c.hits.sort_by_key(rank);
        }
        cands.sort_by(|a, b| {
            let r = |c: &Cand| (c.score * 1000.0).round() as i64;
            r(b).cmp(&r(a)).then(b.sources().len().cmp(&a.sources().len()))
        });
        cands
    }

    pub fn search_media(&self, media: &Media, kind: Kind, lib: &Library) -> Vec<Cand> {
        let mut best: Vec<Cand> = vec![];
        for (i, q) in queries(media, &self.aliases).iter().enumerate() {
            let mut found = self.find_in(kind, q, media.year, &|c| {
                library_folder(c, media, lib).is_some() || (kind == Kind::Movie && c.sources().contains(&"tmdb"))
            });
            if i > 0 {
                // Alternativsuchen nur, wenn die Originalsuche nichts Klares brachte
                let alias = self.aliases.get(&norm(&media.title)).filter(|a| norm(&split_folder(&a.folder).0) == norm(q));
                for c in &mut found {
                    c.score -= 0.01;
                    c.via_alias = alias.is_some_and(|a| !c.knows(a));
                }
            }
            best.extend(found);
            best.sort_by(|a, b| b.score.total_cmp(&a.score));
            if best.first().is_some_and(|c| c.score >= 0.97) {
                break;
            }
        }
        best.truncate(8);
        best
    }

    pub fn load_episodes(&self, c: &mut Cand, seasons: &BTreeSet<u32>) {
        for hit in c.hits.clone() {
            let Some(src) = self.sources.iter().find(|s| s.id() == hit.source) else { continue };
            if !src.has_episodes() || c.episodes.iter().any(|e| e.0 == hit.source) {
                continue;
            }
            if let Some(e) = self.guard(src, || src.episodes(&hit, seasons, &self.lang)).filter(|e| !e.is_empty()) {
                c.episodes.push((hit.source.clone(), e));
            }
        }
    }
}

// ---------------------------------------------------------------- Zeilen

/// Eine schon vorhandene Datei, so wie sie beim Entscheiden aussah.
#[derive(Clone, Debug, PartialEq)]
pub struct Old {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl Old {
    pub fn of(p: &Path) -> Option<Old> {
        let m = fs::metadata(p).ok()?;
        Some(Old { path: p.into(), len: m.len(), modified: m.modified().ok() })
    }
}

#[derive(Clone)]
pub struct Row {
    pub src: PathBuf,
    pub size: u64,
    pub media: Media,
    pub cands: Arc<Vec<Cand>>,
    pub pick: Option<usize>,
    pub status: Status,
    pub note: String,
    pub dst: Option<PathBuf>,
    pub checked: bool,
    pub manual: Option<PathBuf>,
    /// Titel von Hand gewaehlt
    pub approved: bool,
    /// auch Warnungen ausdruecklich bestaetigt
    pub confirmed: bool,
    /// Ruhezeit ausdruecklich uebersprungen; bestaetigt sonst nichts
    pub rested: bool,
    /// Treffer stammt aus einer Handsuche und ist noch nicht bestaetigt
    pub searched: bool,
    /// ohne Aufforderung hinzugefuegt (Ordnerueberwachung): Unerkanntes nicht online suchen
    pub auto: bool,
    /// Staffel und erste Folge von Hand
    pub by_hand: Option<(u32, u32)>,
    /// vorhandene Datei, die ersetzt werden koennte
    pub old: Option<Old>,
    /// vom Nutzer ausdruecklich zum Ersetzen freigegeben
    pub replace: Option<Old>,
    /// alte Fassung nach dem Ersetzen aufheben
    pub keep_old: bool,
    dup: Option<(Status, bool, String)>,
    pub folder: String,
    pub source_error: String,
    pub track: Option<Track>,
}

impl Row {
    pub fn new(src: &Path) -> io::Result<Row> {
        Ok(Row {
            size: fs::metadata(src)?.len(),
            media: if music::is_audio(src) { Media::of(Kind::Music) } else { parse(src) },
            track: if music::is_audio(src) { music::read(src) } else { Option::None },
            src: src.into(),
            cands: Default::default(),
            pick: Option::None,
            status: Status::None,
            note: String::new(),
            dst: Option::None,
            checked: false,
            manual: Option::None,
            approved: false,
            confirmed: false,
            rested: false,
            searched: false,
            auto: false,
            by_hand: Option::None,
            old: Option::None,
            replace: Option::None,
            keep_old: false,
            dup: Option::None,
            folder: String::new(),
            source_error: String::new(),
        })
    }
    pub fn cand(&self) -> Option<&Cand> {
        self.cands.get(self.pick?)
    }
    /// Staffel und Folgen, von Hand Eingetragenes zuerst.
    pub fn episode(&self) -> (Option<u32>, Vec<u32>) {
        let n = self.media.episodes.len().max(1) as u32;
        match self.by_hand {
            Some((s, e)) => (Some(s), (e..e + n).collect()),
            Option::None => (self.media.season, self.media.episodes.clone()),
        }
    }
    pub fn kind(&self) -> Kind {
        match self.cand() {
            Some(c) => c.kind(),
            Option::None if self.media.kind != Kind::Unknown => self.media.kind,
            Option::None if self.media.episodes.is_empty() && self.media.hint.is_empty() => Kind::Movie,
            Option::None => Kind::Tv,
        }
    }
}

/// Besten Kandidaten waehlen und sagen, ob er sicher ist (leerer Grund = sicher).
pub fn choose(media: &Media, cands: &[Cand], lib: &Library) -> (Option<usize>, String) {
    let Some(first) = cands.first().filter(|c| c.score >= 0.6) else {
        return (Option::None, "keine Quelle kennt diesen Titel".into());
    };
    if first.via_alias {
        return (Some(0), "gelernte Zuordnung, bitte einmal bestätigen".into());
    }
    if first.score < 0.97 {
        return (Some(0), format!("nur ähnlich ({:.0} %)", first.score * 100.0));
    }
    let top: Vec<usize> = (0..cands.len()).filter(|i| cands[*i].score >= first.score - 0.03).collect();
    if top.len() > 1 && media.year.is_none() {
        let years: Vec<Option<i32>> = cands
            .iter()
            .flat_map(|c| c.variants())
            .filter_map(|v| lib.folders.get(&norm(v)))
            .flatten()
            .map(|f| f.1)
            .filter(Option::is_some)
            .collect();
        let hit: Vec<_> = top.iter().filter(|i| years.contains(&cands[**i].year())).collect();
        if hit.len() == 1 {
            return (Some(*hit[0]), String::new());
        }
        return (Some(0), format!("{} gleichnamige Titel", top.len()));
    }
    if top.len() > 1 && cands[top[1]].score >= first.score && cands[top[1]].year() == first.year() {
        return (Some(0), "mehrere Titel mit gleichem Namen und Jahr".into());
    }
    (Some(0), String::new())
}

/// Vorhandenen Ordner unter irgendeinem bekannten Titel (deutsch, englisch, original) finden.
/// Der zweite Wert sagt, ob man ihm ohne Rueckfrage trauen darf.
fn library_folder(c: &Cand, media: &Media, lib: &Library) -> Option<(String, bool)> {
    let known = c.hits.iter().flat_map(|h| std::iter::once(&h.title).chain(h.titles.iter().map(|t| &t.1)));
    let names = unique(std::iter::once(&media.title).chain(known));
    let direct = unique([c.year(), media.year])
        .into_iter()
        .flat_map(|y| names.iter().map(move |n| (y, n)))
        .find_map(|(y, n)| lib.find(n, y, c.kind() == Kind::Movie));
    if let Some(f) = direct {
        return Some((f, true));
    }
    let a = lib.alias(&media.title).filter(|_| c.kind() == Kind::Tv)?;
    Some((a.folder.clone(), c.knows(a)))
}

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{(\w+)\}").unwrap());

pub type Values = HashMap<&'static str, String>;

/// Vorlage fuellen. Werte koennen keine Ordnergrenzen einschleusen, ".." ist ausgeschlossen.
pub fn render(fmt: &str, values: &Values) -> Result<PathBuf, String> {
    let plain = fmt.replace("{plex}", &values["plex_format"]).replace('\\', "/");
    let mut out = PathBuf::new();
    for part in plain.split('/') {
        let mut text = String::new();
        let mut at = 0;
        for c in TOKEN.captures_iter(part).flatten() {
            let m = c.get(0).unwrap();
            let v = values.get(&c[1]).filter(|_| &c[1] != "plex_format");
            text += &part[at..m.start()];
            text += v.ok_or_else(|| format!("Unbekannter Platzhalter {{{}}}", &c[1]))?;
            at = m.end();
        }
        text += &part[at..];
        let text = text.replace("()", "");
        let name = safe_name(text.trim().trim_end_matches([' ', '-']));
        let name = short(&name, 200);
        if !name.is_empty() && name != ".." {
            out.push(name);
        }
    }
    if out.as_os_str().is_empty() {
        return Err("Die Vorlage ergibt keinen Namen".into());
    }
    Ok(out)
}

fn with_year(title: &str, year: Option<i32>) -> String {
    safe_name(title) + &year.map(|y| format!(" ({y})")).unwrap_or_default()
}

fn len_of(p: &Path) -> u64 {
    fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Ziel und Status einer Zeile berechnen.
pub fn resolve(row: &mut Row, cfg: &Settings, tv: &Library, movies: &Library) {
    if row.media.kind == Kind::Music {
        return music::resolve(row, cfg);
    }
    let mut media = row.media.clone();
    if let Some((s, e)) = row.by_hand {
        let n = media.episodes.len().max(1) as u32;
        (media.season, media.episodes) = (Some(s), (e..e + n).collect());
        if media.kind == Kind::Unknown && !media.title.is_empty() {
            media.kind = Kind::Tv;
        }
    }
    let cand = row.cand().cloned();
    let kind = cand.as_ref().map_or(media.kind, |c| c.kind());
    let lib = if kind == Kind::Tv { tv } else { movies };
    (row.dst, row.checked, row.note, row.dup, row.old) = (Option::None, false, String::new(), Option::None, Option::None);
    let mut reason = String::new();
    let mut missing = false;
    let because = |reason: &mut String, text: String| {
        if reason.is_empty() {
            *reason = text;
        }
    };
    let mut season = media.season;
    let eps = &media.episodes;
    let (title, year, folder);
    let mut ep_title = media.ep_title.clone();
    match &cand {
        Option::None => {
            if media.kind == Kind::Unknown {
                row.status = Status::None;
                row.note = match (row.source_error.as_str(), media.hint, row.auto) {
                    (e, ..) if !e.is_empty() => e.into(),
                    (_, h, _) if !h.is_empty() => h.into(),
                    (.., true) => "nicht erkannt, deshalb nicht online gesucht".into(),
                    _ => "Titel nicht erkannt".into(),
                };
                return;
            }
            row.status = Local;
            reason = if row.source_error.is_empty() { "keine Quelle kennt diesen Titel".into() } else { row.source_error.clone() };
            (title, year) = (media.title.clone(), media.year);
            folder = match (lib.find(&title, year, kind == Kind::Movie), lib.alias(&title).filter(|_| kind == Kind::Tv)) {
                (Some(f), _) => f,
                (_, Some(a)) => a.folder.clone(),
                _ => with_year(&title, year),
            };
        }
        Some(c) => {
            row.status = Safe;
            (title, year) = (c.title(&cfg.lang).to_string(), c.year().or(media.year));
            let found = library_folder(c, &media, lib);
            let fresh = found.is_none();
            let trusted = found.as_ref().is_none_or(|f| f.1);
            folder = found.map(|f| f.0).unwrap_or_else(|| with_year(&title, year));
            if !row.approved {
                reason = if row.searched { "Suchtreffer, bitte bestätigen".into() } else { choose(&media, &row.cands, lib).1 };
                if !trusted {
                    because(&mut reason, "gelernte Zuordnung, bitte einmal bestätigen".into());
                }
                if kind == Kind::Movie && c.year().is_none() {
                    because(&mut reason, "Quelle kennt kein Jahr".into());
                }
                // Faellt eine Quelle aus, koennte der Ordner unter anderem Titel schon existieren
                if fresh && !row.source_error.is_empty() {
                    because(&mut reason, format!("{}; Ordner evtl. unter anderem Titel vorhanden", row.source_error));
                }
                if kind == Kind::Movie && media.year.is_none() {
                    because(&mut reason, "kein Jahr im Dateinamen".into());
                }
            }
            if kind == Kind::Tv {
                if season.is_none() && !eps.is_empty() {
                    let known: BTreeSet<u32> = c.episodes.iter().flat_map(|e| e.1.keys()).map(|k| k.0).filter(|s| *s > 0).collect();
                    if known.len() == 1 {
                        season = known.first().copied();
                        because(&mut reason, "Staffel ergänzt".into());
                    }
                }
                if let (Some(sn), Some(first)) = (season, eps.first()) {
                    let names = eps.iter().filter_map(|n| {
                        c.episodes.iter().filter_map(|e| e.1.get(&(sn, *n))).find(|t| !placeholder(t)).cloned()
                    });
                    let joined = unique(names).join(" & ");
                    if !joined.is_empty() {
                        ep_title = joined;
                    }
                    if c.episodes.is_empty() {
                        if media.year.is_none() {
                            because(&mut reason, "Folge nicht online bestätigt".into());
                        }
                    } else if !c.episodes.iter().any(|e| e.1.contains_key(&(sn, *first))) {
                        missing = true;
                        because(&mut reason, format!("S{sn:02}E{first:02} gibt es laut Quelle nicht"));
                    }
                }
            }
            if !row.confirmed && (missing || (!reason.is_empty() && !row.approved)) {
                row.status = Check;
            }
        }
    }
    row.note = reason;
    row.folder = folder.clone();
    if lib.root.as_os_str().is_empty() {
        let what = if kind == Kind::Tv { "Serien" } else { "Film" };
        (row.status, row.note) = (Status::None, format!("{what}ordner noch nicht gewählt (Einstellungen)"));
        return;
    }

    let mut values: Values = HashMap::from([
        ("n", safe_name(&title)),
        ("y", year.map(|y| y.to_string()).unwrap_or_default()),
        ("folder", folder.clone()),
    ]);
    let mut code = (0, 0);
    if kind == Kind::Tv {
        let (Some(sn), Some(first), Some(last)) = (season, eps.first(), eps.last()) else {
            (row.status, row.note) = (Status::None, if media.hint.is_empty() { "Staffel oder Folge fehlt: bitte eintragen".into() } else { media.hint.into() });
            return;
        };
        code = (sn, *first);
        let tail = if last > first { format!("-E{last:02}") } else { String::new() };
        let sdir = season_dir_name(&lib.show(&folder), sn);
        values.extend([
            ("s", sn.to_string()),
            ("s00", format!("{sn:02}")),
            ("e", first.to_string()),
            ("e00", format!("{first:02}")),
            ("s00e00", format!("S{sn:02}E{first:02}{tail}")),
            ("sxe", format!("{sn}x{first:02}")),
            ("t", safe_name(&ep_title)),
            ("season_dir", sdir),
            ("plex_format", "{folder}/{season_dir}/{folder} - {s00e00} - {t}".into()),
        ]);
    } else {
        values.insert("plex_format", "{folder}/{folder}".into());
    }
    let fmt = if kind == Kind::Tv { &cfg.format_tv } else { &cfg.format_movie };
    let rel = match row.manual.clone().map_or_else(|| render(fmt, &values), Ok) {
        Ok(p) => p,
        Err(e) => {
            (row.status, row.note) = (Error, e);
            return;
        }
    };
    let mut name = rel.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}", ext_of(&row.src)));
    let dst = lib.root.join(rel.with_file_name(name));

    let parent = dst.parent().unwrap_or(&lib.root).to_path_buf();
    let mut present = Option::None;
    let mut hard = ""; // warum sich die vorhandene Datei nicht einfach ersetzen laesst
    if kind == Kind::Tv {
        let have = lib.show(&folder);
        present = eps.iter().find_map(|n| have.episodes.get(&(code.0, *n)).cloned());
        if present.as_ref().is_some_and(|p| have.episodes.iter().any(|(k, f)| f == p && !(k.0 == code.0 && eps.contains(&k.1)))) {
            hard = "sie enthält weitere Folgen";
        }
    } else if parent != lib.root {
        let vids: Vec<PathBuf> = fs::read_dir(&parent).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| is_video(p)).collect();
        let named = vids.iter().find(|v| stem_of(v).to_lowercase() == stem_of(&dst).to_lowercase());
        if named.is_none() && vids.len() > 1 {
            hard = "im Ordner liegen mehrere Videos";
        }
        present = named.or(vids.iter().find(|v| len_of(v) == row.size)).or(vids.first()).cloned();
    }
    if dst.exists() {
        if kind != Kind::Tv {
            hard = "";
        }
        present = Some(dst.clone());
    }
    if let Some(p) = present {
        if replacing(row, &p, dst, hard, cfg) {
            return;
        }
        if len_of(&p) == row.size {
            (row.status, row.note) = (Exists, "liegt schon in Plex, gleiche Größe".into());
        } else {
            let n = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let why = if hard.is_empty() { String::new() } else { format!(" (lässt sich nicht ersetzen, {hard})") };
            (row.status, row.note) = (Conflict, format!("andere Datei vorhanden: {n}{why}"));
        }
        row.dst = Some(p);
        return;
    }
    row.dst = Some(dst);
    if let Some(why) = occupied(row, cfg) {
        (row.status, row.note) = (Busy, why);
        return;
    }
    row.checked = row.status == Safe;
}

/// Merkt sich die ersetzbare Datei und stellt die Zeile auf "Ersetzen", wenn der Nutzer genau diese freigegeben hat.
pub fn replacing(row: &mut Row, present: &Path, dst: PathBuf, hard: &str, cfg: &Settings) -> bool {
    let itself = copy::same_path(&row.src, present);
    row.old = if hard.is_empty() && !itself { Old::of(present) } else { Option::None };
    if row.replace.is_none() || row.replace != row.old {
        row.replace = Option::None;
        return false;
    }
    let name = present.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let doubt = if row.note.is_empty() { String::new() } else { format!("; Achtung: {}", row.note) };
    row.dst = Some(dst);
    if let Some(why) = occupied(row, cfg) {
        (row.status, row.note) = (Busy, why);
        return true;
    }
    (row.status, row.checked, row.note) = (Replace, true, format!("ersetzt {name}{doubt}"));
    true
}

/// Platzhalter, die Datenquellen vor der Ausstrahlung eintragen.
fn placeholder(title: &str) -> bool {
    static P: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:|tba|tbd|tbc|(?:episode|folge|chapter|teil) ?#?\d+)$").unwrap());
    P.is_match(title.trim()).unwrap_or(false)
}

/// Grund, warum eine Datei noch nicht angefasst werden darf.
pub fn occupied(row: &Row, cfg: &Settings) -> Option<String> {
    match copy::hold(&row.src) {
        Err(e) if copy::in_use(&e) => return Some("wird gerade geschrieben, z. B. von einem Download".into()),
        Err(e) => return Some(format!("nicht lesbar: {e}")),
        Ok(_) => {}
    }
    if download_marker(&row.src) {
        return Some("daneben liegt eine unfertige Download-Datei".into());
    }
    let age = fs::metadata(&row.src).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
    if !row.rested && age.is_some_and(|a| a.as_secs() < cfg.min_age_minutes * 60) {
        return Some("gerade erst geändert, vielleicht noch nicht fertig".into());
    }
    Option::None
}

/// Gleiche Ziele in der Liste erkennen. Merkt sich den vorherigen Stand, damit Entfernen einer Zeile die andere freigibt.
pub fn mark_duplicates<'a>(rows: impl Iterator<Item = &'a mut Row>) {
    let mut seen: HashMap<String, String> = HashMap::new();
    for r in rows {
        if let Some((s, c, n)) = r.dup.take() {
            (r.status, r.checked, r.note) = (s, c, n);
        }
        let Some(dst) = r.dst.clone().filter(|_| matches!(r.status, Safe | Check | Local | Replace)) else { continue };
        let mut keys = vec![dst.with_extension("").to_string_lossy().to_lowercase()];
        if r.kind() == Kind::Tv {
            let (season, eps) = r.episode();
            keys.extend(eps.iter().map(|e| format!("{}|{}|{e}", norm(&r.folder), season.unwrap_or(0))));
        }
        let name = r.src.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if let Some(other) = keys.iter().find_map(|k| seen.get(k)) {
            r.dup = Some((r.status, r.checked, std::mem::take(&mut r.note)));
            (r.status, r.checked, r.note) = (Conflict, false, format!("gleiches Ziel wie {other}"));
        } else {
            seen.extend(keys.into_iter().map(|k| (k, name.clone())));
        }
    }
}

/// Gruppiert gleiche Titel, fragt die Quellen und setzt Ziel + Status. `ready` meldet jede fertige Zeile sofort.
pub fn match_rows(rows: &mut [Row], cfg: &Settings, f: &Finder, progress: &dyn Fn(usize, usize, &str), ready: &dyn Fn(&Row)) {
    let (tv, movies) = (Library::new(Path::new(&cfg.tv), &f.aliases), Library::new(Path::new(&cfg.movies), &Aliases::new()));
    type Key = (Kind, String, Option<i32>);
    let mut groups: Vec<(Key, Vec<usize>)> = vec![];
    for (i, r) in rows.iter_mut().enumerate() {
        if r.media.kind == Kind::Music {
            music::resolve(r, cfg);
            ready(r);
            continue;
        }
        if r.auto && r.media.kind == Kind::Unknown {
            resolve(r, cfg, &tv, &movies); // ueberwachte Ordner: Unerkanntes nicht nach draussen geben
            ready(r);
            continue;
        }
        let kind = match r.media.kind {
            Kind::Unknown if r.media.episodes.is_empty() && r.media.hint.is_empty() && r.by_hand.is_none() => Kind::Movie,
            Kind::Unknown => Kind::Tv,
            k => k,
        };
        let key = (kind, norm(&r.media.title), r.media.year);
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(i),
            Option::None => groups.push((key, vec![i])),
        }
    }
    let n = groups.len();
    let jobs: Vec<(Kind, Media, BTreeSet<u32>)> = groups
        .iter()
        .map(|((kind, ..), ids)| {
            let mut seasons: BTreeSet<u32> = ids.iter().filter_map(|i| rows[*i].media.season).collect();
            if seasons.is_empty() {
                seasons.insert(1);
            }
            (*kind, rows[ids[0]].media.clone(), seasons)
        })
        .collect();
    let mut wanted: Vec<(Kind, String)> = vec![];
    for (kind, media, _) in &jobs {
        wanted.extend(queries(media, &f.aliases).into_iter().map(|q| (*kind, q)));
    }
    progress(0, n, "Frage Datenquellen");
    for s in f.sources.iter().filter(|s| s.ready()) {
        s.prefetch(&wanted);
    }
    // Drei Titel gleichzeitig; die Wartezeiten je Dienst regelt die Drosselung
    let next = AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|sc| {
        for _ in 0..3.min(n) {
            let (tx, next, jobs, f, tv, movies) = (tx.clone(), &next, &jobs, f.fork(), &tv, &movies);
            sc.spawn(move || loop {
                let g = next.fetch_add(1, Ordering::SeqCst);
                if f.cancel.load(Ordering::SeqCst) {
                    break;
                }
                let Some((kind, media, seasons)) = jobs.get(g) else { break };
                f.errors.lock().unwrap().clear();
                let lib = if *kind == Kind::Tv { tv } else { movies };
                let mut cands = if media.title.is_empty() { vec![] } else { f.search_media(media, *kind, lib) };
                let pick = choose(media, &cands, lib).0;
                if let (Kind::Tv, Some(p)) = (kind, pick) {
                    f.load_episodes(&mut cands[p], seasons);
                }
                if tx.send((g, Arc::new(cands), pick, f.errors())).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for (done, (g, cands, pick, errors)) in rx.iter().enumerate() {
            for i in &groups[g].1 {
                let r = &mut rows[*i];
                (r.cands, r.pick, r.approved, r.confirmed, r.searched, r.manual) = (cands.clone(), pick, false, false, false, Option::None);
                r.source_error = errors.clone();
                resolve(r, cfg, &tv, &movies);
                ready(r);
            }
            progress(done + 1, n, &jobs[g].1.title);
        }
    });
    mark_duplicates(rows.iter_mut());
    progress(n, n, "");
}

// ---------------------------------------------------------------- Ausfuehren + Verlauf

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Entry {
    pub zeit: String,
    pub aktion: String,
    pub quelle: String,
    pub ziel: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub quellen: Vec<String>,
    /// dabei gelernte Zuordnung
    #[serde(default)]
    pub alias: String,
    /// beiseitegelegte alte Fassung und ihr frueherer Pfad
    #[serde(default)]
    pub alt: String,
    #[serde(default)]
    pub alt_pfad: String,
}

pub fn stamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub fn add_history(e: &Entry) -> io::Result<()> {
    use io::Write;
    fs::create_dir_all(&*DATA)?;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(DATA.join("verlauf.jsonl"))?;
    f.write_all(format!("{}\n", serde_json::to_string(e)?).as_bytes())?;
    f.sync_all()
}

pub fn read_history() -> Vec<Entry> {
    let text = fs::read_to_string(DATA.join("verlauf.jsonl")).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

pub type Log<'a> = &'a mut dyn FnMut(Entry) -> io::Result<()>;

/// Kopieren oder Verschieben, immer mit Pruefsumme. Reihenfolge: alles kopieren, im Verlauf festhalten, erst dann loeschen.
pub fn execute(row: &mut Row, action: &str, log: Log, progress: Progress) -> io::Result<()> {
    let dst = row.dst.clone().ok_or_else(|| io::Error::other("kein Ziel"))?;
    let stem = stem_of(&dst);
    let quellen: Vec<String> = row.cand().map_or(vec!["dateiname".into()], |c| c.sources().iter().map(|s| s.to_string()).collect());
    let alias = learned(row).map(|a| a.0).unwrap_or_default();
    let entry = |src: &Path, dst: &Path, bytes: u64, sha256: String, alias: &str| Entry {
        zeit: stamp(),
        aktion: action.into(),
        quelle: src.to_string_lossy().into(),
        ziel: dst.to_string_lossy().into(),
        bytes,
        sha256,
        quellen: quellen.clone(),
        alias: alias.into(),
        alt: String::new(),
        alt_pfad: String::new(),
    };
    let mut notes: Vec<String> = vec![];
    let mut sources: Vec<(PathBuf, fs::File)> = vec![]; // offene Griffe: bis zum Loeschen schreibt niemand
    let mut jobs = vec![(row.src.clone(), dst.clone())];
    jobs.extend(sidecars(&row.src).into_iter().map(|(p, rest)| (p, dst.with_file_name(format!("{stem}{rest}")))));
    let mut complete = true;
    let mut aside: Option<PathBuf> = Option::None;
    if let Some(o) = &row.replace {
        if Old::of(&o.path).as_ref() != Some(o) {
            return Err(io::Error::other("Die vorhandene Datei hat sich seit deiner Entscheidung geändert"));
        }
    }
    let replace = row.replace.as_ref().map(|o| o.path.clone());
    for (n, (src, dst)) in jobs.into_iter().enumerate() {
        let main = n == 0;
        if !main && dst.exists() {
            notes.push("Untertitel war schon da".into());
            complete = false;
            continue;
        }
        let one = (|| {
            let held = copy::hold(&src)?;
            let len = held.metadata()?.len();
            let old = replace.as_deref().filter(|_| main);
            let noted = |sha: String, kept: Option<&PathBuf>| {
                let mut e = entry(&src, &dst, len, sha, if main { &alias } else { "" });
                if let (Some(k), Some(o)) = (kept, old) {
                    (e.alt, e.alt_pfad) = (k.to_string_lossy().into(), o.to_string_lossy().into());
                }
                e
            };
            if action == "move" {
                // Gleiches Laufwerk: umbenennen statt doppelt Platz zu brauchen
                dst.parent().map_or(Ok(()), fs::create_dir_all)?;
                let placed = match old {
                    Some(o) => copy::swap(&src, &dst, o).map(Some),
                    Option::None => copy::rename_new(&src, &dst).map(|_| Option::None),
                };
                match placed {
                    Ok(kept) => {
                        let sha = copy::sha256_held(&held, progress);
                        if let Err(e) = sha.and_then(|s| log(noted(s, kept.as_ref()))) {
                            copy::rename_new(&dst, &src)?; // ohne Verlauf kein Verschieben
                            if let (Some(k), Some(o)) = (kept, old) {
                                copy::rename_new(&k, o)?;
                            }
                            return Err(e);
                        }
                        aside = kept.or(aside.take());
                        return Ok(());
                    }
                    Err(e) if copy::other_volume(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            let (sha, kept) = match old {
                Some(o) => copy::copy_replacing(&held, &dst, o, progress).map(|r| (r.0, Some(r.1)))?,
                Option::None => (copy::copy(&held, &dst, progress)?, Option::None),
            };
            if let Err(e) = log(noted(sha, kept.as_ref())) {
                if let (Some(k), Some(o)) = (kept, old) {
                    fs::remove_file(&dst)?; // ohne Verlauf bleibt die alte Fassung
                    copy::rename_new(&k, o)?;
                }
                return Err(e);
            }
            aside = kept.or(aside.take());
            sources.push((src.clone(), held));
            Ok(())
        })();
        match one {
            Ok(()) => {}
            Err(e) if main => return Err(e),
            Err(e) => {
                complete = false;
                notes.push(format!("Untertitel nicht übertragen: {e}"));
            }
        }
    }
    if action == "move" && !sources.is_empty() {
        if complete {
            for (src, held) in sources {
                if fs::remove_file(&src).is_err() {
                    notes.push("Original konnte nicht gelöscht werden".into());
                }
                drop(held);
            }
        } else {
            notes.push("Originale deshalb behalten".into());
        }
    }
    if let Some(k) = aside {
        if row.keep_old {
            notes.push("alte Fassung aufgehoben".into());
        } else if fs::remove_file(&k).is_err() {
            notes.push(format!("alte Fassung konnte nicht gelöscht werden: {}", k.display()));
        } else {
            notes.push("alte Fassung gelöscht".into());
        }
    }
    notes.insert(0, "übertragen und geprüft".into());
    (row.status, row.note, row.checked) = (Done, notes.join("; "), false);
    Ok(())
}

pub fn alias_file() -> PathBuf {
    DATA.join("aliases.json")
}

pub fn load_aliases() -> Aliases {
    read_json(&alias_file()).unwrap_or_default()
}

pub fn save_aliases(a: &Aliases) {
    let sorted: BTreeMap<_, _> = a.iter().collect();
    let _ = write_json(&alias_file(), &sorted);
}

/// Ausdruecklich gewaehlte Serie merken, wenn der Titel im Dateinamen anders lautet als der Ordner.
/// Nie bei Titeln aus Ordnernamen oder Allerweltsnamen.
pub fn learned(row: &Row) -> Option<(String, Alias)> {
    let key = norm(&row.media.title);
    let own = norm(&split_folder(&row.folder).0);
    let c = row.cand()?;
    let ok = row.approved && c.kind() == Kind::Tv && !row.media.from_parent && !is_generic(&row.media.title) && key.len() > 2;
    (ok && !row.folder.is_empty() && key != own).then(|| (key, Alias { folder: row.folder.clone(), ids: c.ids() }))
}

/// Macht einen Eintrag rueckgaengig, nur wenn die Zieldatei noch genau die uebertragene ist.
pub fn undo(e: &Entry, cfg: &Settings, log: Log, progress: Progress) -> io::Result<String> {
    let fail = |t: &str| Err(io::Error::other(t));
    let (src, dst) = (Path::new(&e.quelle), Path::new(&e.ziel));
    if !dst.exists() {
        return fail("Die Zieldatei gibt es nicht mehr");
    }
    if !e.alt.is_empty() && !Path::new(&e.alt).exists() {
        return fail("Die ersetzte alte Fassung ist gelöscht, deshalb geht Rückgängig hier nicht");
    }
    let held = copy::hold(dst)?;
    if copy::sha256_held(&held, progress)? != e.sha256 {
        return fail("Die Zieldatei wurde seitdem verändert");
    }
    if !e.quelle.is_empty() {
        if src.exists() {
            if copy::sha256(src, progress)? != e.sha256 {
                return fail("Am Quellort liegt eine andere Datei");
            }
        } else {
            copy::copy(&held, src, progress)?;
        }
    }
    fs::remove_file(dst)?;
    drop(held);
    if !e.alt.is_empty() {
        copy::rename_new(Path::new(&e.alt), Path::new(&e.alt_pfad))?;
    }
    log(Entry { zeit: stamp(), aktion: "undo".into(), ..e.clone() })?;
    for dir in dst.ancestors().skip(1).take(2) {
        // nur leere Ordner, nie die Bibliothek selbst
        if [Kind::Tv, Kind::Movie, Kind::Music].iter().any(|k| dir == cfg.root(*k)) || fs::remove_dir(dir).is_err() {
            break;
        }
    }
    let name = if e.quelle.is_empty() { dst } else { src };
    Ok(format!("{}: {}", if e.quelle.is_empty() { "Entfernt" } else { "Wiederhergestellt" }, name.file_name().unwrap_or_default().to_string_lossy()))
}
