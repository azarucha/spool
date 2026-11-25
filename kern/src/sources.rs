//! Online-Datenquellen. Ohne Schluessel: TVmaze, Wikidata, AniList. Mit eigenem Schluessel: TMDB, TheTVDB, OMDb.
//! An die Dienste geht der aus Datei- oder Ordnernamen erkannte Titel, dazu Staffel und Jahr; nie der ganze Pfad.
use crate::names::Kind;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub static DATA: LazyLock<PathBuf> = LazyLock::new(|| match std::env::var_os("SPOOL_HOME") {
    Some(p) => p.into(),
    None => PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Spool"),
});
pub const UA: &str = concat!("Spool/", env!("CARGO_PKG_VERSION"), " (desktop media organizer)");

pub type Episodes = BTreeMap<(u32, u32), String>;
type Pairs = Vec<(String, String)>;

#[derive(Clone, Debug)]
pub struct Hit {
    pub source: String,
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub year: Option<i32>,
    pub titles: Pairs, // de/en/orig
    pub aka: Vec<String>,
    pub ids: Pairs, // imdb/tmdb/tvdb
    pub poster: String,
    pub overview: String,
}

impl Hit {
    pub fn new(source: &str, id: impl Into<String>, kind: Kind, title: impl Into<String>, year: Option<i32>) -> Hit {
        Hit {
            source: source.into(),
            id: id.into(),
            kind,
            title: title.into(),
            year,
            titles: vec![],
            aka: vec![],
            ids: vec![],
            poster: String::new(),
            overview: String::new(),
        }
    }
    pub fn variants(&self) -> Vec<&str> {
        let mut out: Vec<&str> = vec![];
        let all = std::iter::once(&self.title).chain(self.titles.iter().map(|t| &t.1)).chain(&self.aka);
        for t in all {
            if !t.is_empty() && !out.contains(&t.as_str()) {
                out.push(t);
            }
        }
        out
    }
}

// ---------------------------------------------------------------- HTTP

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    let tls = native_tls::TlsConnector::new().expect("TLS");
    ureq::AgentBuilder::new().tls_connector(Arc::new(tls)).timeout(Duration::from_secs(15)).user_agent(UA).build()
});

/// Zwischenspeicher uebergehen (ausdruecklicher neuer Abgleich).
pub static FRESH: AtomicBool = AtomicBool::new(false);
/// Filmplakate aus Wikipedia laden (Einstellung).
pub static WIKI_POSTERS: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Host {
    next: Option<Instant>,
    paused: Option<Instant>,
    slow: u32, // waechst bei Drosselung, damit wir hoeflich bleiben
    recent: Vec<Instant>,
    fails: u32,
    slowed: Option<Instant>,
}
static HOSTS: LazyLock<Mutex<HashMap<String, Host>>> = LazyLock::new(Default::default);

fn throttle(host: &str, gap: Duration) -> Result<(), String> {
    let wait = {
        let mut hosts = HOSTS.lock().unwrap();
        let h = hosts.entry(host.into()).or_default();
        let now = Instant::now();
        if h.paused.is_some_and(|t| t > now) {
            return Err(format!("{host}: macht kurz Pause, gleich neu abgleichen"));
        }
        if h.slowed.is_some_and(|t| now - t > Duration::from_secs(300)) {
            (h.slow, h.slowed) = (h.slow / 2, Some(now)); // klingt wieder ab
        }
        let mut slot = h.next.filter(|t| *t > now).unwrap_or(now);
        if matches!(host, "www.wikidata.org" | "en.wikipedia.org") {
            // Wikimedia erlaubt ohne Kontaktangabe rund 10 Anfragen je Minute; wir bleiben bei 8
            let minute = Duration::from_secs(60);
            h.recent.retain(|t| *t + minute > now);
            if h.recent.len() >= 8 {
                slot = slot.max(h.recent[h.recent.len() - 8] + minute);
            }
            if slot > now + Duration::from_secs(20) {
                return Err(format!("{host}: Anfragelimit erreicht, in einer Minute neu abgleichen"));
            }
            h.recent.push(slot);
        }
        h.next = Some(slot + gap * h.slow.max(1));
        slot - now
    };
    std::thread::sleep(wait);
    Ok(())
}

pub struct Req<'a> {
    pub url: String,
    pub secret: Option<(&'a str, String)>,
    pub bearer: Option<String>,
    pub header: Option<(&'a str, String)>,
    pub ua: Option<&'a str>,
    pub body: Option<Value>,
    pub form: Option<(&'a str, String)>,
    pub gap_ms: u64,
    pub ttl: u64,
    pub tries: u32,
}

impl Req<'_> {
    pub fn get(url: String) -> Self {
        Req { url, secret: None, bearer: None, header: None, ua: None, body: None, form: None, gap_ms: 100, ttl: 7 * 86400, tries: 3 }
    }
}

/// JSON holen. Geheime Werte gelangen weder in den Zwischenspeicher noch in Fehlermeldungen. 404 ergibt Null.
pub fn fetch(r: Req) -> Result<Value, String> {
    let mut h = Sha256::new();
    h.update(&r.url);
    h.update(r.body.as_ref().map(Value::to_string).unwrap_or_default());
    h.update(r.form.as_ref().map(|f| f.1.as_str()).unwrap_or_default());
    let cached = DATA.join("cache").join(format!("{}.json", &crate::copy::hex(&h.finalize())[..40]));
    if r.ttl > 0 && !FRESH.load(Ordering::SeqCst) {
        let fresh = fs::metadata(&cached).and_then(|m| m.modified()).ok();
        if fresh.is_some_and(|t| SystemTime::now().duration_since(t).is_ok_and(|d| d.as_secs() < r.ttl)) {
            if let Some(v) = fs::read(&cached).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
                return Ok(v);
            }
        }
    }
    let host = r.url.split('/').nth(2).unwrap_or_default().to_string();
    let mut attempt = 0;
    let value = loop {
        attempt += 1;
        throttle(&host, Duration::from_millis(r.gap_ms))?;
        let mut q = if r.body.is_some() || r.form.is_some() { AGENT.post(&r.url) } else { AGENT.get(&r.url) };
        q = q.set("Accept", "application/json");
        if let Some((k, v)) = &r.secret {
            q = q.query(k, v);
        }
        if let Some(t) = &r.bearer {
            q = q.set("Authorization", &format!("Bearer {t}"));
        }
        if let Some((k, v)) = &r.header {
            q = q.set(k, v);
        }
        if let Some(ua) = r.ua {
            q = q.set("User-Agent", ua);
        }
        let got = match (&r.body, &r.form) {
            (Some(b), _) => q.send_json(b),
            (None, Some((k, v))) => q.send_form(&[(*k, v.as_str())]),
            _ => q.call(),
        };
        let last = attempt >= r.tries;
        match got {
            Ok(resp) => match resp.into_json::<Value>() {
                // Manche Dienste melden Ueberlast mit Status 200; das darf nie in den Zwischenspeicher
                Ok(v) if truthy(&v["error"]) || truthy(&v["errors"]) => {
                    if last {
                        return Err(format!("{host}: meldet einen Fehler, später erneut versuchen"));
                    }
                }
                Ok(v) => break v,
                Err(_) if last => return Err(format!("{host}: Antwort unlesbar")),
                Err(_) => {}
            },
            Err(ureq::Error::Status(401 | 403, resp)) => {
                let text = resp.into_string().unwrap_or_default().to_lowercase();
                let why = if text.contains("limit") { "Tageslimit erreicht" } else { "Schlüssel fehlt oder ist ungültig" };
                return Err(format!("{host}: {why}"));
            }
            Err(ureq::Error::Status(404, _)) => return Ok(Value::Null),
            Err(ureq::Error::Status(code, resp)) => {
                if code == 429 {
                    let mut hosts = HOSTS.lock().unwrap();
                    let h = hosts.entry(host.clone()).or_default();
                    (h.slow, h.slowed) = ((h.slow.max(1) * 2).min(8), Some(Instant::now()));
                }
                if code == 429 && attempt >= 2 {
                    HOSTS.lock().unwrap().entry(host.clone()).or_default().paused = Some(Instant::now() + Duration::from_secs(60));
                    return Err(format!("{host}: gedrosselt, kurze Pause"));
                }
                if last || !matches!(code, 429 | 500 | 502 | 503 | 504) {
                    return Err(format!("{host}: Fehler {code}"));
                }
                let after: u64 = resp.header("Retry-After").and_then(|s| s.parse().ok()).unwrap_or(0);
                std::thread::sleep(Duration::from_secs(after.min(20)));
            }
            Err(_) if last => {
                let mut hosts = HOSTS.lock().unwrap();
                let h = hosts.entry(host.clone()).or_default();
                h.fails += 1;
                if h.fails >= 2 {
                    h.paused = Some(Instant::now() + Duration::from_secs(60)); // nicht fuer jeden Titel erneut warten
                }
                return Err(format!("{host}: nicht erreichbar"));
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(attempt as u64));
    };
    if let Some(h) = HOSTS.lock().unwrap().get_mut(&host) {
        h.fails = 0;
    }
    if r.ttl > 0 && !hollow(&value) {
        let tmp = cached.with_extension(format!("{:?}.tmp", std::thread::current().id()).replace(['(', ')'], ""));
        let _ = fs::create_dir_all(DATA.join("cache"));
        if fs::write(&tmp, value.to_string()).is_ok() && fs::rename(&tmp, &cached).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
    Ok(value)
}

/// Leere Ergebnisse werden nicht gemerkt, sonst bleibt eine neue Serie tagelang unbekannt.
fn hollow(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.values().any(|x| x.as_array().is_some_and(|a| a.is_empty())) && !o.values().any(|x| x.as_array().is_some_and(|a| !a.is_empty())),
        _ => false,
    }
}

fn cache_files() -> impl Iterator<Item = (PathBuf, Duration)> {
    let all = fs::read_dir(DATA.join("cache")).into_iter().flatten().flatten();
    all.filter_map(|e| Some((e.path(), SystemTime::now().duration_since(e.metadata().ok()?.modified().ok()?).unwrap_or_default())))
}

pub fn prune_cache() {
    for (p, _) in cache_files().filter(|f| f.1 > Duration::from_secs(30 * 86400)) {
        let _ = fs::remove_file(p);
    }
}

pub fn clear_cache() {
    for (p, _) in cache_files() {
        let _ = fs::remove_file(p);
    }
}

/// Kleine Datei laden (z. B. Untertitel), mit Groessengrenze.
pub fn download(url: &str, ua: &str, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = AGENT.get(url).set("User-Agent", ua).call().map_err(|_| "Download fehlgeschlagen".to_string())?;
    let mut out = vec![];
    resp.into_reader().take(max + 1).read_to_end(&mut out).map_err(|e| e.to_string())?;
    if out.len() as u64 > max {
        return Err("Datei ist unerwartet groß".into());
    }
    Ok(out)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

fn enc(s: &str) -> String {
    let keep = |b: u8| b.is_ascii_alphanumeric() || b"-_.~".contains(&b);
    s.bytes().map(|b| if keep(b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

pub fn query(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&")
}

/// Schreibweisen, unter denen ein Titel aus einem Dateinamen bei Wikidata stehen kann.
fn spellings(title: &str) -> Vec<String> {
    let mut out = vec![title.to_string()];
    if title.contains(" - ") {
        out.push(title.replacen(" - ", ": ", 1));
    }
    if !title.chars().any(char::is_uppercase) {
        let big = title.split(' ').map(|w| w.chars().take(1).flat_map(char::to_uppercase).chain(w.chars().skip(1)).collect::<String>());
        out.push(big.collect::<Vec<_>>().join(" "));
    }
    out
}

fn year_of(v: &Value) -> Option<i32> {
    let s = match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim_start_matches('+').to_string(),
        _ => return None,
    };
    s.get(..4)?.parse().ok().filter(|y| *y > 1800)
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn pairs(list: &[(&str, String)]) -> Pairs {
    list.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn strip_html(text: &str) -> String {
    let (mut out, mut tag) = (String::new(), false);
    for c in text.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    for (a, b) in [("&quot;", "\""), ("&#39;", "'"), ("&lt;", "<"), ("&gt;", ">"), ("&nbsp;", " "), ("&amp;", "&")] {
        out = out.replace(a, b);
    }
    out.trim().to_string()
}

fn num(v: &Value) -> Option<u32> {
    v.as_u64().map(|n| n as u32)
}

// ---------------------------------------------------------------- Quellen

pub trait Source: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn kinds(&self) -> &[Kind] {
        &[Kind::Tv, Kind::Movie]
    }
    fn needs_key(&self) -> bool {
        false
    }
    fn key_url(&self) -> &str {
        ""
    }
    fn key(&self) -> String {
        String::new()
    }
    fn set_key(&self, _: &str) {}
    fn has_episodes(&self) -> bool {
        true
    }
    /// Nur fragen, wenn die anderen nichts Klares liefern.
    fn fallback(&self) -> bool {
        false
    }
    /// Viele Titel auf einmal vorbereiten, um Einzelanfragen zu sparen.
    fn prefetch(&self, _titles: &[(Kind, String)]) {}
    /// 0 sofort, 1 nur ohne klaren Treffer mit vorhandenem Plex-Ordner, 2 nur ohne klaren Treffer.
    fn stage(&self, _kind: Kind) -> u8 {
        if self.fallback() {
            2
        } else {
            0
        }
    }
    fn ready(&self) -> bool {
        !self.needs_key() || !self.key().is_empty()
    }
    fn search(&self, kind: Kind, title: &str, lang: &str) -> Result<Vec<Hit>, String>;
    fn episodes(&self, _hit: &Hit, _seasons: &BTreeSet<u32>, _lang: &str) -> Result<Episodes, String> {
        Ok(Episodes::new())
    }
}

/// Alle eingebauten Quellen; `base` ist fuer Tests ueberschreibbar.
pub struct Online {
    id: &'static str,
    base: String,
    key: Mutex<String>,
    token: Mutex<(String, String)>,
    known: Mutex<HashMap<(Kind, String), Vec<Hit>>>,
}

impl Online {
    pub fn new(id: &'static str) -> Online {
        let base = match id {
            "tvmaze" => "https://api.tvmaze.com",
            "wikidata" => "https://www.wikidata.org",
            "anilist" => "https://graphql.anilist.co",
            "tmdb" => "https://api.themoviedb.org/3",
            "tvdb" => "https://api4.thetvdb.com/v4",
            _ => "https://www.omdbapi.com",
        };
        Online { id, base: base.into(), key: Default::default(), token: Default::default(), known: Default::default() }
    }
    pub fn with_base(id: &'static str, base: &str) -> Online {
        Online { base: base.into(), ..Online::new(id) }
    }

    fn tvmaze(&self, path: &str, ttl: u64) -> Result<Value, String> {
        fetch(Req { gap_ms: 550, ttl, ..Req::get(format!("{}{path}", self.base)) })
    }

    fn wiki(&self, params: &[(&str, &str)]) -> Result<Value, String> {
        fetch(Req { gap_ms: 450, ..Req::get(format!("{}/w/api.php?format=json&{}", self.base, query(params))) })
    }

    /// Bis zu 40 Titel je Anfrage, exakte Namen und Aliase auf Deutsch, Englisch und "mul".
    fn wikidata_batch(&self, titles: &[(Kind, String)]) -> Result<(), String> {
        let block = |kind: Kind, prop: &str| {
            let mut vals = String::new();
            for (_, t) in titles.iter().filter(|t| t.0 == kind) {
                for form in spellings(t) {
                    let lit = Value::String(form).to_string(); // JSON-Zeichenkette ist gueltiges SPARQL-Literal
                    for l in ["de", "en", "mul"] {
                        vals += &format!("{lit}@{l} ");
                    }
                }
            }
            let name = kind.as_str();
            (!vals.is_empty()).then(|| format!(r#"{{ VALUES ?hit {{ {vals} }} ?item (rdfs:label|skos:altLabel) ?hit ; wdt:{prop} ?tmdb . BIND("{name}" AS ?kind) }}"#))
        };
        let parts: Vec<String> = [block(Kind::Movie, "P4947"), block(Kind::Tv, "P4983")].into_iter().flatten().collect();
        if parts.is_empty() {
            return Ok(());
        }
        let sparql = format!(
            r#"SELECT ?kind ?item ?tmdb (SAMPLE(?imdb0) AS ?imdb) (SAMPLE(?tvdb0) AS ?tvdb) (MIN(YEAR(?d)) AS ?year) (SAMPLE(?de0) AS ?de) (SAMPLE(?en0) AS ?en) (SAMPLE(?mul0) AS ?mul) (GROUP_CONCAT(DISTINCT STR(?hit); separator="\u001f") AS ?hits) (SAMPLE(?art0) AS ?art) WHERE {{ {} OPTIONAL {{ ?item wdt:P577|wdt:P580 ?d }} OPTIONAL {{ ?item wdt:P345 ?imdb0 }} OPTIONAL {{ ?item wdt:P4835 ?tvdb0 }} OPTIONAL {{ ?item rdfs:label ?de0 FILTER(LANG(?de0)="de") }} OPTIONAL {{ ?item rdfs:label ?en0 FILTER(LANG(?en0)="en") }} OPTIONAL {{ ?item rdfs:label ?mul0 FILTER(LANG(?mul0)="mul") }} OPTIONAL {{ ?a schema:about ?item ; schema:isPartOf <https://en.wikipedia.org/> ; schema:name ?art0 }} }} GROUP BY ?kind ?item ?tmdb LIMIT 800"#,
            parts.join(" UNION ")
        );
        let url = if self.base.contains("wikidata.org") { "https://query.wikidata.org/sparql?format=json".to_string() } else { format!("{}/sparql", self.base) };
        let data = fetch(Req { form: Some(("query", sparql)), gap_ms: 1500, ttl: 2 * 86400, tries: 2, ..Req::get(url) })?;
        let rows = arr(&data["results"]["bindings"]);
        let val = |r: &Value, k: &str| s(&r[k]["value"]);
        let arts: Vec<String> = rows.iter().filter(|r| val(r, "kind") == "movie").map(|r| val(r, "art")).filter(|a| !a.is_empty()).collect();
        let mut pics = HashMap::new();
        for chunk in arts.chunks(50) {
            pics.extend(self.page_images(chunk));
        }
        let mut known = self.known.lock().unwrap();
        for (k, t) in titles {
            known.entry((*k, crate::names::norm(t))).or_default();
        }
        for r in rows {
            let kind = if val(r, "kind") == "tv" { Kind::Tv } else { Kind::Movie };
            let or = |a: String, b: String| if a.is_empty() { b } else { a };
            let (de, en) = (or(val(r, "de"), val(r, "mul")), or(val(r, "en"), val(r, "mul")));
            let seen: Vec<String> = val(r, "hits").split('\u{1f}').map(String::from).collect();
            let hit = Hit {
                titles: pairs(&[("de", de.clone()), ("en", en.clone())]),
                aka: seen.clone(),
                ids: pairs(&[("tmdb", val(r, "tmdb")), ("imdb", val(r, "imdb")), ("tvdb", val(r, "tvdb"))]),
                poster: pics.get(&val(r, "art")).cloned().unwrap_or_default(),
                ..Hit::new(self.id, val(r, "item").rsplit('/').next().unwrap_or(""), kind, or(en, or(de, seen[0].clone())), year_of(&r["year"]["value"]))
            };
            for key in seen.iter().map(|t| crate::names::norm(t)).collect::<BTreeSet<_>>() {
                let list = known.entry((kind, key)).or_default();
                if !list.iter().any(|h| h.id == hit.id) {
                    list.push(hit.clone());
                }
            }
        }
        Ok(())
    }

    fn wikidata(&self, kind: Kind, title: &str, lang: &str) -> Result<Vec<Hit>, String> {
        let key = (kind, crate::names::norm(title));
        if !self.known.lock().unwrap().contains_key(&key) {
            let _ = self.wikidata_batch(&[(kind, title.to_string())]);
        }
        if let Some(hits) = self.known.lock().unwrap().get(&key).filter(|h| !h.is_empty()) {
            return Ok(hits.clone());
        }
        let prop = if kind == Kind::Movie { "P4947" } else { "P4983" }; // TMDB-Film- bzw. -Serien-ID
        let find = format!("{title} haswbstatement:{prop}");
        let found = self.wiki(&[("action", "query"), ("list", "search"), ("srlimit", "10"), ("srnamespace", "0"), ("srsearch", &find)])?;
        let ids: Vec<String> = arr(&found["query"]["search"]).iter().map(|r| s(&r["title"])).collect();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let data = self.wiki(&[
            ("action", "wbgetentities"),
            ("ids", &ids.join("|")),
            ("props", "labels|aliases|descriptions|claims|sitelinks"),
            ("sitefilter", "enwiki"),
            ("languages", "de|en|mul"),
        ])?;
        let pics = if kind == Kind::Movie { self.posters(&data, &ids) } else { HashMap::new() };
        let mut out = vec![];
        for qid in ids {
            let e = &data["entities"][&qid];
            let Some(labels) = e["labels"].as_object().filter(|l| !l.is_empty()) else { continue };
            let label = |l: &str| labels.get(l).map(|v| s(&v["value"])).unwrap_or_default();
            let claims = |p: &str| arr(&e["claims"][p]).iter().map(|c| c["mainsnak"]["datavalue"]["value"].clone()).collect::<Vec<_>>();
            let year = ["P577", "P580"].iter().flat_map(|p| claims(p)).filter_map(|v| year_of(&v["time"])).min();
            let orig = claims("P1476").first().map(|v| s(&v["text"])).unwrap_or_default();
            let titles = pairs(&[("de", label("de")), ("en", label("en")), ("orig", orig)]);
            let first = labels.values().next().map(|v| s(&v["value"])).unwrap_or_default();
            let name = titles.iter().find(|t| t.0 == lang).map(|t| t.1.clone()).unwrap_or(first);
            let ext = |p: &str| claims(p).first().map(s).unwrap_or_default();
            let aka = e["aliases"].as_object().into_iter().flat_map(|o| o.values()).flat_map(arr).map(|a| s(&a["value"]));
            let d = &e["descriptions"];
            let text = if d["de"].is_null() { &d["en"] } else { &d["de"] };
            out.push(Hit {
                titles,
                aka: aka.collect(),
                ids: pairs(&[("tmdb", ext(prop)), ("imdb", ext("P345")), ("tvdb", ext("P4835"))]),
                overview: s(&text["value"]),
                poster: pics.get(&qid).cloned().unwrap_or_default(),
                ..Hit::new(self.id, qid, kind, name, year)
            });
        }
        Ok(out)
    }

    /// Filmplakate aus den zugehoerigen Wikipedia-Artikeln. Fehler hier sind unwichtig.
    fn posters(&self, data: &Value, ids: &[String]) -> HashMap<String, String> {
        let pages: Vec<(String, String)> = ids
            .iter()
            .map(|id| (id.clone(), s(&data["entities"][id]["sitelinks"]["enwiki"]["title"])))
            .filter(|p| !p.1.is_empty())
            .collect();
        let names: Vec<String> = pages.iter().map(|p| p.1.clone()).collect();
        let pics = self.page_images(&names);
        pages.into_iter().filter_map(|(id, t)| Some((id, pics.get(&t)?.clone()))).collect()
    }

    fn page_images(&self, names: &[String]) -> HashMap<String, String> {
        if names.is_empty() || !WIKI_POSTERS.load(Ordering::SeqCst) {
            return HashMap::new();
        }
        let host = if self.base.contains("wikidata.org") { "https://en.wikipedia.org" } else { &self.base };
        let titles = names.join("|");
        let q = [("action", "query"), ("prop", "pageimages"), ("piprop", "thumbnail"), ("pithumbsize", "320"), ("pilicense", "any"), ("pilimit", "50"), ("titles", &titles)];
        let Ok(found) = fetch(Req { gap_ms: 300, tries: 1, ..Req::get(format!("{host}/w/api.php?format=json&{}", query(&q))) }) else {
            return HashMap::new();
        };
        found["query"]["pages"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.values())
            .map(|p| (s(&p["title"]), s(&p["thumbnail"]["source"]).split('?').next().unwrap_or("").to_string()))
            .filter(|p| p.1.starts_with("https://"))
            .collect()
    }

    fn anilist(&self, kind: Kind, title: &str) -> Result<Vec<Hit>, String> {
        const Q: &str = "query($s:String){Page(perPage:6){media(search:$s,type:ANIME){id format startDate{year} synonyms title{romaji english native} coverImage{large} description(asHtml:false)}}}";
        let body = json!({ "query": Q, "variables": { "s": title } });
        let data = fetch(Req { body: Some(body), gap_ms: 2100, ..Req::get(self.base.clone()) })?;
        let mut out = vec![];
        for m in arr(&data["data"]["Page"]["media"]) {
            let mk = if m["format"] == "MOVIE" { Kind::Movie } else { Kind::Tv };
            if mk != kind {
                continue;
            }
            let titles = pairs(&[("en", s(&m["title"]["english"])), ("orig", s(&m["title"]["romaji"]))]);
            let name = titles.first().map(|t| t.1.clone()).unwrap_or("?".into());
            out.push(Hit {
                titles,
                aka: arr(&m["synonyms"]).iter().map(s).collect(),
                poster: s(&m["coverImage"]["large"]),
                overview: strip_html(&s(&m["description"])),
                ..Hit::new(self.id, s(&m["id"]), kind, name, year_of(&m["startDate"]["year"]))
            });
        }
        Ok(out)
    }

    fn tmdb(&self, path: &str, params: &[(&str, &str)]) -> Result<Value, String> {
        let key = self.key();
        let ttl = if path.contains("/season/") { 43200 } else { 7 * 86400 };
        let mut r = Req { gap_ms: 50, ttl, ..Req::get(format!("{}{path}?{}", self.base, query(params))) };
        if key.starts_with("eyJ") {
            r.bearer = Some(key);
        } else {
            r.secret = Some(("api_key", key));
        }
        fetch(r)
    }

    fn tvdb(&self, path: &str) -> Result<Value, String> {
        let token = {
            let mut t = self.token.lock().unwrap();
            let key = self.key();
            if t.0 != key || t.1.is_empty() {
                let (k, pin) = key.split_once(':').unwrap_or((&key, "")); // "schluessel" oder "schluessel:PIN"
                let body = if pin.is_empty() { json!({ "apikey": k }) } else { json!({ "apikey": k, "pin": pin }) };
                let data = fetch(Req { body: Some(body), ttl: 0, ..Req::get(format!("{}/login", self.base)) })?;
                *t = (key.clone(), s(&data["data"]["token"]));
            }
            t.1.clone()
        };
        let ttl = if path.contains("/episodes/") { 43200 } else { 7 * 86400 };
        fetch(Req { bearer: Some(token), ttl, ..Req::get(format!("{}{path}", self.base)) })
    }

    fn omdb(&self, params: &[(&str, &str)]) -> Result<Value, String> {
        fetch(Req { secret: Some(("apikey", self.key())), ttl: 86400, ..Req::get(format!("{}/?{}", self.base, query(params))) })
    }
}

fn locale(lang: &str) -> &'static str {
    if lang == "de" {
        "de-DE"
    } else {
        "en-US"
    }
}

impl Source for Online {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        match self.id {
            "tvmaze" => "TVmaze",
            "wikidata" => "Wikidata",
            "anilist" => "AniList",
            "tmdb" => "TheMovieDB",
            "tvdb" => "TheTVDB",
            _ => "OMDb",
        }
    }
    fn kinds(&self) -> &[Kind] {
        if self.id == "tvmaze" {
            &[Kind::Tv]
        } else {
            &[Kind::Tv, Kind::Movie]
        }
    }
    fn needs_key(&self) -> bool {
        matches!(self.id, "tmdb" | "tvdb" | "omdb")
    }
    fn key_url(&self) -> &str {
        match self.id {
            "tmdb" => "https://www.themoviedb.org/settings/api",
            "tvdb" => "https://thetvdb.com/api-information",
            "omdb" => "https://www.omdbapi.com/apikey.aspx",
            _ => "",
        }
    }
    fn key(&self) -> String {
        self.key.lock().unwrap().clone()
    }
    fn set_key(&self, k: &str) {
        *self.key.lock().unwrap() = k.trim().into();
    }
    fn has_episodes(&self) -> bool {
        !matches!(self.id, "wikidata" | "anilist")
    }
    fn fallback(&self) -> bool {
        self.id == "anilist"
    }
    fn stage(&self, kind: Kind) -> u8 {
        match (self.id, kind) {
            ("anilist", _) => 2,
            ("wikidata", _) => 1,
            _ => 0,
        }
    }

    fn prefetch(&self, titles: &[(Kind, String)]) {
        if self.id == "wikidata" {
            for part in titles.chunks(40) {
                let _ = self.wikidata_batch(part); // scheitert es, folgt die Einzelsuche
            }
        }
    }

    fn search(&self, kind: Kind, title: &str, lang: &str) -> Result<Vec<Hit>, String> {
        let movie = kind == Kind::Movie;
        match self.id {
            "tvmaze" => {
                let data = self.tvmaze(&format!("/search/shows?q={}", enc(title)), 7 * 86400)?;
                Ok(arr(&data)
                    .iter()
                    .take(8)
                    .map(|r| {
                        let x = &r["show"];
                        Hit {
                            ids: pairs(&[("imdb", s(&x["externals"]["imdb"])), ("tvdb", s(&x["externals"]["thetvdb"]))]),
                            poster: s(&x["image"]["medium"]),
                            overview: strip_html(&s(&x["summary"])),
                            ..Hit::new(self.id, s(&x["id"]), Kind::Tv, s(&x["name"]), year_of(&x["premiered"]))
                        }
                    })
                    .collect())
            }
            "wikidata" => self.wikidata(kind, title, lang),
            "anilist" => self.anilist(kind, title),
            "tmdb" => {
                let loc = locale(lang);
                let path = if movie { "/search/movie" } else { "/search/tv" };
                let data = self.tmdb(path, &[("query", title), ("language", loc)])?;
                Ok(arr(&data["results"])
                    .iter()
                    .take(8)
                    .map(|r| {
                        let f = |m: &str, t: &str| s(&r[if movie { m } else { t }]);
                        let (name, orig) = (f("title", "name"), f("original_title", "original_name"));
                        let shown = if name.is_empty() { orig.clone() } else { name.clone() };
                        let pic = s(&r["poster_path"]);
                        Hit {
                            titles: pairs(&[(&loc[..2], name), ("orig", orig)]),
                            ids: pairs(&[("tmdb", s(&r["id"]))]),
                            poster: if pic.is_empty() { pic } else { format!("https://image.tmdb.org/t/p/w342{pic}") },
                            overview: s(&r["overview"]),
                            ..Hit::new(self.id, s(&r["id"]), kind, shown, year_of(&r[if movie { "release_date" } else { "first_air_date" }]))
                        }
                    })
                    .collect())
            }
            "tvdb" => {
                let t = if movie { "movie" } else { "series" };
                let data = self.tvdb(&format!("/search?{}", query(&[("query", title), ("type", t), ("limit", "8")])))?;
                Ok(arr(&data["data"])
                    .iter()
                    .map(|r| {
                        let tr = &r["translations"];
                        let titles = pairs(&[("de", s(&tr["deu"])), ("en", s(&tr["eng"])), ("orig", s(&r["name"]))]);
                        let name = titles.iter().find(|x| x.0 == lang).map(|x| x.1.clone()).unwrap_or(s(&r["name"]));
                        let id = s(&r["tvdb_id"]);
                        let text = if r["overviews"]["deu"].is_string() { &r["overviews"]["deu"] } else { &r["overview"] };
                        let pic = if truthy(&r["thumbnail"]) { &r["thumbnail"] } else { &r["image_url"] };
                        Hit {
                            titles,
                            aka: arr(&r["aliases"]).iter().map(s).collect(),
                            ids: if movie { vec![] } else { pairs(&[("tvdb", id.clone())]) },
                            poster: s(pic),
                            overview: s(text),
                            ..Hit::new(self.id, id, kind, name, year_of(&r["year"]))
                        }
                    })
                    .collect())
            }
            _ => {
                let data = self.omdb(&[("s", title), ("type", if movie { "movie" } else { "series" })])?;
                Ok(arr(&data["Search"])
                    .iter()
                    .map(|r| {
                        let pic = s(&r["Poster"]);
                        Hit {
                            titles: pairs(&[("en", s(&r["Title"]))]),
                            ids: pairs(&[("imdb", s(&r["imdbID"]))]),
                            poster: if pic.starts_with("https") { pic } else { String::new() },
                            ..Hit::new(self.id, s(&r["imdbID"]), kind, s(&r["Title"]), year_of(&r["Year"]))
                        }
                    })
                    .collect())
            }
        }
    }

    fn episodes(&self, hit: &Hit, seasons: &BTreeSet<u32>, lang: &str) -> Result<Episodes, String> {
        let mut out = Episodes::new();
        match self.id {
            "tvmaze" => {
                for r in arr(&self.tvmaze(&format!("/shows/{}/episodes?specials=1", enc(&hit.id)), 86400)?) {
                    if let (Some(a), Some(b)) = (num(&r["season"]), num(&r["number"])) {
                        out.insert((a, b), s(&r["name"]));
                    }
                }
            }
            "tmdb" => {
                for n in seasons {
                    let data = self.tmdb(&format!("/tv/{}/season/{n}", enc(&hit.id)), &[("language", locale(lang))])?;
                    for e in arr(&data["episodes"]) {
                        if let (Some(a), Some(b)) = (num(&e["season_number"]), num(&e["episode_number"])) {
                            out.insert((a, b), s(&e["name"]));
                        }
                    }
                }
            }
            "tvdb" => {
                let code = if lang == "de" { "deu" } else { "eng" };
                for page in 0..20 {
                    let data = self.tvdb(&format!("/series/{}/episodes/default/{code}?page={page}", enc(&hit.id)))?;
                    let rows = arr(&data["data"]["episodes"]);
                    for e in rows {
                        if let (Some(a), Some(b)) = (num(&e["seasonNumber"]), num(&e["number"])) {
                            out.insert((a, b), s(&e["name"]));
                        }
                    }
                    if rows.is_empty() || !truthy(&data["links"]["next"]) {
                        break;
                    }
                }
            }
            "omdb" => {
                for n in seasons {
                    let data = self.omdb(&[("i", &hit.id), ("Season", &n.to_string())])?;
                    for e in arr(&data["Episodes"]) {
                        if let Ok(k) = s(&e["Episode"]).parse() {
                            out.insert((*n, k), s(&e["Title"]));
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(out)
    }
}

pub type Sources = Vec<Arc<dyn Source>>;

pub const ORDER: &[&str] = &["tmdb", "tvdb", "tvmaze", "wikidata", "anilist", "omdb"];

pub fn all_sources() -> Sources {
    ORDER.iter().map(|id| Arc::new(Online::new(id)) as Arc<dyn Source>).collect()
}

// ---------------------------------------------------------------- Schluessel (mit Windows DPAPI verschluesselt)

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::*;
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: input zeigt auf gueltige Daten, out wird von Windows belegt und unten freigegeben.
    unsafe {
        let ok = if protect {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), 1, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), 1, &mut out)
        };
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Some(v)
    }
}

#[cfg(not(windows))]
fn dpapi(_: &[u8], _: bool) -> Option<Vec<u8>> {
    None
}

pub fn load_keys() -> HashMap<String, String> {
    fs::read(DATA.join("keys.bin")).ok().and_then(|b| dpapi(&b, false)).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_keys(keys: &HashMap<String, String>) -> Result<(), String> {
    let full: HashMap<_, _> = keys.iter().filter(|(_, v)| !v.is_empty()).collect();
    let data = serde_json::to_vec(&full).map_err(|e| e.to_string())?;
    let enc = dpapi(&data, true).ok_or("Verschlüsselung nicht möglich")?;
    let tmp = DATA.join("keys.tmp");
    fs::create_dir_all(&*DATA).and_then(|_| fs::write(&tmp, enc)).and_then(|_| fs::rename(&tmp, DATA.join("keys.bin"))).map_err(|e| e.to_string())
}
