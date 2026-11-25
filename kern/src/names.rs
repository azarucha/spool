//! Dateinamen lesen und vorhandene Bibliotheksordner finden.
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

pub const VIDEO: &[&str] = &["mkv", "mp4", "m4v", "avi", "ts", "mov", "wmv", "mpg"];
pub const AUDIO: &[&str] = &["mp3", "flac", "m4a", "ogg", "opus", "wav", "aiff", "ape", "wv"];
pub const SUBS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "vtt"];
const GENERIC: &[&str] = &[
    "", "megasync", "onedrive", "dropbox", "google drive", "nextcloud", "icloud drive", "downloads", "download", "desktop", "dokumente", "documents",
    "series", "serien", "movies", "filme", "videos", "video", "tv shows", "shows", "neuer ordner", "new folder", "temp", "tmp", "media", "medien", "staffel", "season",
];

macro_rules! re {
    ($name:ident, $src:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($src).expect("Regex"));
    };
}
re!(EPISODE, r"(?i)(?<![a-z0-9])s(\d{1,2}|(?:19|20)\d\d)[ ._-]*e(\d{1,3})((?:-?e\d{1,3})*)(?:-(\d{2,3})(?![\da-z]))?(?!\d)");
re!(EPISODE_X, r"(?i)(?<![a-z0-9])(\d{1,2})x(?!26[45](?!\d))(\d{2,3})(?!\d)");
re!(FOLGE, r"(?i)(?<![a-z])(?:folge|episode|teil)[ ._-]*(\d{1,3})(?!\d)");
re!(STAFFEL, r"(?i)(?<![a-z])(?:staffel|season)[ ._-]*(\d{1,2})(?!\d)|^s(\d{1,2})$|^(\d{1,2})\. ?staffel$");
re!(LANG_PREFIX, r"^(?:4K-)?[A-Z]{2}\s+-\s+");
re!(LEAD, r"^(?:\[[^\]]*\]\s*|0\d[. ]\s*(?=[A-Za-z]))");
re!(COUNTRY, r"\s*\((?:[A-Z]{2,3})\)");
re!(
    STRICT,
    r"(?i)(?<![a-z0-9])(?:2160p|1440p|1080[pi]|720p|576p|480p|x26[45]|h[ .]?26[45]|hevc|xvid|blu-?ray|bdrip|brrip|web-?dl|web-?rip|hdtv|dvdrip|10bit)(?![a-z0-9])"
);
re!(
    WEAK,
    r"(?i)[ ._\-(\[]+(?:web|german|english|french|multi|dl|proper|dubbed|remastered|repack|internal|4k|uhd|remux|avc|av1|uncut|extended)[ ._\-)\]]*$"
);
re!(DATE, r"(?<!\d)(?:19|20)\d{2}[ ._-](?:0[1-9]|1[0-2])[ ._-](?:0[1-9]|[12]\d|3[01])(?!\d)");
re!(SIDECAR, r"(?i)^(?:\.[a-z]{2,3}(?:-[a-z]{2,4})?)?(?:\.(?:forced|sdh|cc|hi|default|full|\d{1,2}))*\.[a-z]{2,4}$");
re!(YEAR, r"(?<!\d)((?:19|20)\d{2})(?!\d)");
re!(SPACES, r"\s+");
re!(COLON, r"\s*:\s*");
re!(FOLDER, r"^(.*?)\s*\(((?:19|20)\d{2})\)(?:\s*\{[^}]*\})*$");
re!(OPEN_TAIL, r"[\[(][^\])]*$");
re!(SEASON_TAIL, r"(?i)[ ._-]s\d{1,2}(?=[ ._-]|$).*");
re!(RESERVED, r"(?i)^(?:con|prn|aux|nul|com\d|lpt\d)$");

fn sub(re: &Regex, text: &str, with: &str) -> String {
    re.replace_all(text, with).into_owned()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Kind {
    Tv,
    Movie,
    Music,
    Unknown,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Tv => "tv",
            Kind::Movie => "movie",
            Kind::Music => "music",
            Kind::Unknown => "unknown",
        }
    }
    pub fn named(s: &str) -> Option<Kind> {
        match s {
            "tv" => Some(Kind::Tv),
            "movie" => Some(Kind::Movie),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub kind: Kind,
    pub title: String,
    pub year: Option<i32>,
    pub season: Option<u32>,
    pub episodes: Vec<u32>,
    pub ep_title: String,
    /// Titel stammt nur aus Ordnernamen
    pub from_parent: bool,
    /// Warum nicht erkannt (z. B. Datumsfolge)
    pub hint: &'static str,
}

impl Media {
    pub fn of(kind: Kind) -> Media {
        Media { kind, ..Media::unknown(String::new(), None, vec![]) }
    }
    fn unknown(title: String, year: Option<i32>, episodes: Vec<u32>) -> Media {
        Media { kind: Kind::Unknown, title, year, season: None, episodes, ep_title: String::new(), from_parent: false, hint: "" }
    }
}

/// Vergleichsform: ohne Akzente, Satzzeichen und Gross-/Kleinschreibung. Andere Schriften bleiben erhalten.
pub fn norm(text: &str) -> String {
    let base = text.replace('&', " and ").replace(['\'', '’'], "");
    let mut out = String::new();
    for c in base.nfkd().filter(|c| !is_combining_mark(*c)).flat_map(char::to_lowercase) {
        let plain = match c {
            'ß' => "ss",
            'ø' => "o",
            'ł' => "l",
            'æ' => "ae",
            'œ' => "oe",
            'đ' | 'ð' => "d",
            'þ' => "th",
            'ı' => "i",
            _ => "",
        };
        if !plain.is_empty() {
            out.push_str(plain);
        } else if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') && !out.is_empty() {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

/// Auf hoechstens `max` UTF-8-Bytes kuerzen, ohne Zeichen zu zerschneiden.
pub fn short(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end_matches([' ', '.']).to_string()
}

pub fn is_generic(title: &str) -> bool {
    GENERIC.contains(&norm(title).as_str())
}

/// Technische Zusaetze abschneiden. Mehrdeutige Woerter ("Web", "German") nur direkt vor eindeutigen.
fn cut_tags(text: &str) -> String {
    let Ok(Some(m)) = STRICT.find(text) else { return text.to_string() };
    let mut head = text[..m.start()].to_string();
    while let Ok(Some(w)) = WEAK.find(&head) {
        if head[..w.start()].trim_matches([' ', '-', '.', '_', '(', '[']).is_empty() {
            break;
        }
        head.truncate(w.start());
    }
    head
}

/// Fuer Windows/SMB gueltiger Datei- oder Ordnername.
pub fn safe_name(text: &str) -> String {
    let t = sub(&COLON, text, " - ");
    let t: String = t.chars().filter(|c| !"<>\"/\\|?*".contains(*c) && *c >= ' ').collect();
    let t = sub(&SPACES, &t, " ").trim_matches([' ', '.']).to_string();
    if RESERVED.is_match(&t).unwrap_or(false) {
        t + "_"
    } else {
        t
    }
}

fn valid_year(y: i32) -> bool {
    use chrono::Datelike;
    (1900..=chrono::Local::now().year() + 1).contains(&y)
}

fn spaced(text: &str) -> String {
    let t = if text.trim().contains(' ') { text.to_string() } else { text.replace(['.', '_'], " ") };
    t.replace("_ ", " - ").replace('_', " ")
}

/// Titel und Jahr aus einem rohen Namensteil holen.
pub fn clean_title(raw: &str) -> (String, Option<i32>) {
    let text = sub(&LANG_PREFIX, spaced(raw).trim(), "");
    let text = sub(&LEAD, &text, "");
    let mut text = sub(&COUNTRY, &text, "");
    let years: Vec<(usize, i32)> = YEAR
        .captures_iter(&text)
        .flatten()
        .filter_map(|c| Some((c.get(1)?.start(), c.get(1)?.as_str().parse().ok()?)))
        .filter(|(_, y)| valid_year(*y))
        .collect();
    let paren = years.iter().find(|(s, _)| *s > 0 && text.as_bytes()[s - 1] == b'(');
    let pick = paren.or_else(|| years.iter().rev().find(|(s, _)| *s > 0)).copied();
    let mut year = None;
    if let Some((at, y)) = pick {
        if !text[..at].trim_matches([' ', '(', '[', '-', '.']).is_empty() {
            year = Some(y);
            text.truncate(at);
        }
    }
    let text = sub(&SPACES, &if year.is_some() { text } else { cut_tags(&text) }, " ");
    (text.trim_matches([' ', '-', '(', '[', '.', '_', '·', ',']).to_string(), year)
}

fn full(re: &Regex, text: &str) -> Option<u32> {
    let c = re.captures(text).ok()??;
    let m = c.get(0)?;
    let n = (1..c.len()).find_map(|i| c.get(i));
    (m.start() == 0 && m.end() == text.len()).then(|| n?.as_str().parse().ok())?
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn stem_of(p: &Path) -> String {
    p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn ext_of(p: &Path) -> String {
    p.extension().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_video(p: &Path) -> bool {
    VIDEO.contains(&ext_of(p).as_str())
}

fn title_from_parents(path: &Path) -> (String, Option<i32>) {
    for parent in path.ancestors().skip(1).take(2) {
        let name = name_of(parent);
        if is_generic(&name) {
            break; // Sammelordner, kein Serientitel
        }
        if full(&STAFFEL, name.trim()).is_some() {
            continue;
        }
        return clean_title(&sub(&EPISODE, &sub(&SEASON_TAIL, &name, ""), ""));
    }
    (String::new(), None)
}

struct Code {
    start: usize,
    end: usize,
    season: u32,
    episodes: Vec<u32>,
}

/// Staffel und Folgen aus einem Namen: S01E02, S01E01E02E03, S01E01-03, 1x02.
fn code(stem: &str) -> Option<Code> {
    let num = |c: &fancy_regex::Captures, i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    let mut hits: Vec<_> = EPISODE.captures_iter(stem).flatten().collect();
    let long = !hits.is_empty();
    if !long {
        hits = EPISODE_X.captures_iter(stem).flatten().collect();
    }
    let (first, last) = (hits.first()?, hits.last()?);
    let (season, a) = (num(first, 1)?, num(first, 2)?);
    let mut episodes = vec![a];
    if long {
        let more = first.get(3).map(|m| m.as_str()).unwrap_or("");
        let list: Vec<u32> = more.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
        let to = num(first, 4).or_else(|| if more.contains('-') { list.last().copied() } else { None });
        match to {
            Some(b) if b > a && b <= a + 9 => episodes = (a..=b).collect(),
            _ => episodes.extend(list.into_iter().filter(|n| *n > a && *n <= a + 9)),
        }
    }
    Some(Code { start: first.get(0)?.start(), end: last.get(0)?.end(), season, episodes })
}

pub fn parse(path: &Path) -> Media {
    let stem = stem_of(path);
    let num = |c: &fancy_regex::Captures, i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    let titled = |text: &str| {
        let (title, year) = clean_title(text);
        if title.is_empty() {
            let (t, y) = title_from_parents(path);
            (t, y, true)
        } else {
            (title, year, false)
        }
    };
    if let Some(c) = code(&stem) {
        let (title, year, from_parent) = titled(&stem[..c.start]);
        if title.is_empty() {
            return Media::unknown(title, None, vec![]);
        }
        let raw = &stem[c.end..];
        let mut ep_title = String::new();
        if raw.trim().contains(' ') || raw.trim_start().starts_with('-') {
            ep_title = sub(&OPEN_TAIL, &cut_tags(&spaced(raw)), "").trim_matches([' ', '-', '.', '_', '[', '(']).to_string();
        }
        return Media { kind: Kind::Tv, season: Some(c.season), ep_title, from_parent, ..Media::unknown(title, year, c.episodes) };
    }
    if let Ok(Some(d)) = DATE.find(&stem) {
        let (title, year, from_parent) = titled(&stem[..d.start()]);
        return Media { from_parent, hint: "Datumsfolge: bitte Staffel und Folge eintragen", ..Media::unknown(title, year, vec![]) };
    }
    let movie = |title: String, year, from_parent| Media { kind: Kind::Movie, from_parent, ..Media::unknown(title, year, vec![]) };
    let (title, year) = clean_title(&stem);
    if let Ok(Some(folge)) = FOLGE.captures(&stem) {
        let at = folge.get(0).unwrap().start();
        let ep = num(&folge, 1).unwrap_or(0);
        let own = STAFFEL.captures(&stem).ok().flatten();
        let parent = path.parent().map(name_of).unwrap_or_default();
        let season = match &own {
            Some(c) => num(c, 1),
            None => STAFFEL.captures(&parent).ok().flatten().and_then(|c| (1..c.len()).find_map(|i| num(&c, i))),
        };
        if season.is_none() && year.is_some() && !title.is_empty() {
            return movie(title, year, false); // "Dune Teil 2 (2024)"
        }
        let begin = own.as_ref().map(|c| c.get(0).unwrap().start()).filter(|s| *s < at).unwrap_or(at);
        let (title, year, from_parent) = titled(&stem[..begin]);
        if season.is_some() && !title.is_empty() {
            return Media { kind: Kind::Tv, season, from_parent, ..Media::unknown(title, year, vec![ep]) };
        }
        return Media { hint: "Staffel fehlt: bitte eintragen", from_parent, ..Media::unknown(title, year, vec![ep]) };
    }
    if !title.is_empty() && year.is_some() {
        return movie(title, year, false);
    }
    let (pt, py) = clean_title(&path.parent().map(name_of).unwrap_or_default());
    if !pt.is_empty() && py.is_some() {
        let (a, b) = (norm(&title), norm(&pt));
        if title.is_empty() || b.contains(&a) || a.contains(&b) {
            return movie(pt, py, true);
        }
    }
    Media::unknown(title, None, vec![])
}

// ---------------------------------------------------------------- Bibliothek

/// "Silo (2023)" -> ("Silo", 2023). Plex-Zusaetze wie {tmdb-1} werden ignoriert.
pub fn split_folder(name: &str) -> (String, Option<i32>) {
    match FOLDER.captures(name) {
        Ok(Some(c)) => (c[1].to_string(), c[2].parse().ok()),
        _ => (name.to_string(), None),
    }
}

/// Gelernte Zuordnung: Titel aus Dateinamen -> Serienordner, abgesichert ueber die Kennungen der Datenquellen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "AliasForm")]
pub struct Alias {
    pub folder: String,
    pub ids: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AliasForm {
    Old(String),
    New { folder: String, ids: Vec<(String, String)> },
}

impl From<AliasForm> for Alias {
    fn from(f: AliasForm) -> Alias {
        match f {
            AliasForm::Old(folder) => Alias { folder, ids: vec![] },
            AliasForm::New { folder, ids } => Alias { folder, ids },
        }
    }
}

pub type Aliases = HashMap<String, Alias>;

/// Ohne Einzelabfrage je Eintrag; auf Netzlaufwerken entscheidend. Nur Verknuepfungen werden aufgeloest.
pub fn is_dir(e: &fs::DirEntry) -> bool {
    e.file_type().is_ok_and(|t| t.is_dir() || (t.is_symlink() && e.path().is_dir()))
}

pub struct Library {
    pub root: PathBuf,
    aliases: Aliases,
    pub folders: HashMap<String, Vec<(String, Option<i32>)>>,
    shows: Mutex<HashMap<String, Arc<ShowIndex>>>,
}

/// Was in einem Serienordner schon liegt, egal in welchem Unterordner und in welcher Schreibweise.
#[derive(Default)]
pub struct ShowIndex {
    pub seasons: Vec<(u32, String)>,
    pub episodes: HashMap<(u32, u32), PathBuf>,
}

impl Library {
    pub fn new(root: &Path, aliases: &Aliases) -> Library {
        let mut lib = Library { root: root.into(), aliases: aliases.clone(), folders: HashMap::new(), shows: Default::default() };
        if root.as_os_str().is_empty() {
            return lib;
        }
        let mut names: Vec<String> = fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(is_dir)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with(['.', '$']))
            .collect();
        names.sort();
        for n in names {
            let (title, year) = split_folder(&n);
            lib.folders.entry(norm(&title)).or_default().push((n, year));
        }
        lib
    }

    fn has(&self, name: &str) -> bool {
        self.folders.values().flatten().any(|(n, _)| n == name)
    }

    pub fn find(&self, title: &str, year: Option<i32>, need_year: bool) -> Option<String> {
        let key = norm(title);
        if key.is_empty() {
            return None;
        }
        let rows = self.folders.get(&key).map(Vec::as_slice).unwrap_or_default();
        if need_year {
            return rows.iter().find(|(_, y)| *y == year).map(|(n, _)| n.clone());
        }
        if let Some((n, _)) = rows.iter().find(|(_, y)| year.is_some() && *y == year) {
            return Some(n.clone());
        }
        let fit: Vec<_> = rows.iter().filter(|(_, y)| year.is_none() || y.is_none() || *y == year).collect();
        if fit.len() == 1 {
            return Some(fit[0].0.clone());
        }
        None
    }

    /// Gelernte Zuordnung, sofern der Ordner noch existiert und es keinen Ordner mit dem Titel selbst gibt.
    pub fn alias(&self, title: &str) -> Option<&Alias> {
        let key = norm(title);
        self.aliases.get(&key).filter(|a| !self.folders.contains_key(&key) && self.has(&a.folder))
    }

    /// Inhalt eines Serienordners, je Lauf nur einmal gelesen.
    pub fn show(&self, folder: &str) -> Arc<ShowIndex> {
        let mut all = self.shows.lock().unwrap_or_else(|e| e.into_inner());
        all.entry(folder.to_string())
            .or_insert_with(|| {
                let mut ix = ShowIndex::default();
                index_show(&self.root.join(folder), None, &mut ix, 0);
                Arc::new(ix)
            })
            .clone()
    }
}

fn index_show(dir: &Path, season: Option<u32>, ix: &mut ShowIndex, depth: u32) {
    let mut entries: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let (p, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        if is_dir(&e) {
            let special = matches!(norm(&name).as_str(), "specials" | "extras" | "special");
            let s = full(&STAFFEL, name.trim()).or(special.then_some(0));
            if let Some(n) = s {
                ix.seasons.push((n, name));
            }
            if depth < 2 {
                index_show(&p, s.or(season), ix, depth + 1);
            }
        } else if is_video(&p) {
            if let Some(c) = code(&stem_of(&p)) {
                for n in c.episodes {
                    ix.episodes.entry((c.season, n)).or_insert_with(|| p.clone());
                }
            }
        }
    }
}

/// Vorhandenen Staffelordner wiederverwenden, sonst "Season 01".
pub fn season_dir_name(ix: &ShowIndex, season: u32) -> String {
    ix.seasons.iter().find(|s| s.0 == season).map(|s| s.1.clone()).unwrap_or_else(|| format!("Season {season:02}"))
}

#[derive(Default)]
pub struct Found {
    pub files: Vec<PathBuf>,
    pub archives: usize,
    pub small: usize,
}

/// Videos (und auf Wunsch Musik) unter den angegebenen Dateien und Ordnern. Direkt genannte Dateien zaehlen immer.
pub fn scan_report(roots: &[PathBuf], min_size: u64, music: bool) -> Found {
    struct Walk {
        min: u64,
        music: bool,
        files: BTreeSet<PathBuf>,
        archives: usize,
        small: usize,
    }
    impl Walk {
        fn dir(&mut self, dir: &Path, depth: u32) {
            for e in fs::read_dir(dir).into_iter().flatten().flatten() {
                let (p, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if !name.starts_with(['.', '$']) && depth < 12 {
                        self.dir(&p, depth + 1);
                    }
                } else if self.music && AUDIO.contains(&ext_of(&p).as_str()) {
                    self.files.insert(p);
                } else if is_video(&p) && !is_sample(&p) {
                    if e.metadata().is_ok_and(|m| m.len() >= self.min) {
                        self.files.insert(p);
                    } else {
                        self.small += 1;
                    }
                } else if crate::archive::ARCHIVES.contains(&ext_of(&p).as_str()) {
                    self.archives += 1;
                }
            }
        }
    }
    let mut w = Walk { min: min_size, music, files: BTreeSet::new(), archives: 0, small: 0 };
    for r in roots {
        if r.is_file() {
            if is_video(r) || (music && AUDIO.contains(&ext_of(r).as_str())) {
                w.files.insert(r.clone());
            }
        } else {
            w.dir(r, 0);
        }
    }
    Found { files: w.files.into_iter().collect(), archives: w.archives, small: w.small }
}

pub fn scan(roots: &[PathBuf], min_size: u64, music: bool) -> Vec<PathBuf> {
    scan_report(roots, min_size, music).files
}

/// "sample" als eigenes Wort, nicht als Teil eines Titels.
fn is_sample(p: &Path) -> bool {
    norm(&stem_of(p)).split(' ').any(|w| w == "sample")
}

/// Untertitel neben dem Video: (Datei, Rest nach dem Stamm), z. B. ".de.srt". Nur mit Sprach- und Artkennung,
/// damit "Rocky.mkv" nicht "Rocky.II.de.srt" mitnimmt; Gross-/Kleinschreibung egal.
pub fn sidecars(video: &Path) -> Vec<(PathBuf, String)> {
    let stem = stem_of(video);
    let lower = stem.to_lowercase();
    let all: Vec<PathBuf> = fs::read_dir(video.parent().unwrap_or(Path::new("."))).into_iter().flatten().flatten().map(|e| e.path()).collect();
    let rivals: Vec<String> = all.iter().filter(|p| is_video(p)).map(|p| stem_of(p).to_lowercase()).filter(|s| s.len() > lower.len() && s.starts_with(&lower)).collect();
    let mut out: Vec<(PathBuf, String)> = all
        .into_iter()
        .filter(|p| SUBS.contains(&ext_of(p).as_str()))
        .filter_map(|p| {
            let name = name_of(&p);
            let low = name.to_lowercase();
            if !low.starts_with(&lower) || low.len() != name.len() || rivals.iter().any(|r| low.starts_with(r.as_str())) {
                return None;
            }
            let rest = name.get(stem.len()..)?.to_string();
            SIDECAR.is_match(&rest).unwrap_or(false).then_some((p, rest))
        })
        .collect();
    out.sort();
    out
}

/// Zeichen eines noch laufenden Downloads neben der Datei.
pub fn download_marker(p: &Path) -> bool {
    let name = name_of(p);
    ["part", "crdownload", "!qb", "!ut", "aria2", "opdownload", "download", "tmp"].iter().any(|x| p.with_file_name(format!("{name}.{x}")).exists())
}
