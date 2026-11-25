//! Nur zum Entwickeln: zeigt ui/ im Browser (`cargo run --example dev -- ui 4173`), und
//! `dev --parse < liste` gibt die Namenserkennung zeilenweise als JSON aus. Wird nicht ausgeliefert.
use kern::api::{Api, Dialogs};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

struct None_;
impl Dialogs for None_ {
    fn free(&self, _: &Path) -> Option<u64> {
        Some(923_000_000_000)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args[0] == "--parse" {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let m = kern::names::parse(Path::new(&line));
            println!("{}", serde_json::json!([m.kind.as_str(), m.title, m.year, m.season, m.episodes, m.ep_title]));
        }
        return;
    }
    if args[0] == "--plan" {
        // nur lesen: Abgleich gegen die eingestellte Bibliothek, mit Zeiten
        use kern::engine::*;
        let cfg = Settings::load();
        let t = std::time::Instant::now();
        let lib = kern::names::Library::new(Path::new(&cfg.tv), &Default::default());
        let lib2 = kern::names::Library::new(Path::new(&cfg.movies), &Default::default());
        println!("Bibliothek {} + {} Titel in {:?}", lib.folders.len(), lib2.folders.len(), t.elapsed());
        let files = kern::names::scan(&[PathBuf::from(&args[1])], 0, false);
        let mut rows: Vec<Row> = files.iter().filter_map(|p| Row::new(p).ok()).collect();
        let t = std::time::Instant::now();
        let f = Finder::new(kern::sources::all_sources(), "en", &Default::default());
        match_rows(&mut rows, &cfg, &f, &|_, _, _| {}, &|_| {});
        println!("Abgleich {} Dateien in {:?} {}", rows.len(), t.elapsed(), f.seen_errors());
        for r in rows {
            println!("{} | {} | {}", r.status.as_str(), r.dst.unwrap_or_default().display(), r.note);
        }
        return;
    }
    let api = Api::new(Box::new(None_), kern::sources::all_sources());
    let ui = PathBuf::from(&args[0]);
    let server = TcpListener::bind(format!("127.0.0.1:{}", args[1])).unwrap();
    for s in server.incoming().flatten() {
        let (api, ui) = (api.clone(), ui.clone());
        std::thread::spawn(move || {
            let mut r = BufReader::new(&s);
            let (mut first, mut len, mut own) = (String::new(), 0, true);
            let _ = r.read_line(&mut first);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) < 3 {
                    break;
                }
                let low = h.to_lowercase();
                let host = low.strip_prefix("host:").or(low.strip_prefix("origin:"));
                if let Some(v) = host {
                    let v = v.trim().trim_start_matches("http://");
                    own &= v.starts_with("localhost:") || v.starts_with("127.0.0.1:"); // fremde Webseiten duerfen nichts ausloesen
                }
                if let Some(v) = low.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            if !own {
                let _ = (&s).write_all(b"HTTP/1.1 403 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                return;
            }
            let path = first.split(' ').nth(1).unwrap_or("/").split('?').next().unwrap().to_string();
            let (kind, data) = if first.starts_with("POST /call") {
                let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                let out = api.call(v["method"].as_str().unwrap_or(""), v["args"].as_array().map(Vec::as_slice).unwrap_or_default());
                ("application/json", out.to_string().into_bytes())
            } else {
                let name = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
                let kind = if name.ends_with("css") { "text/css" } else if name.ends_with("js") { "text/javascript" } else { "text/html" };
                (kind, std::fs::read(ui.join(Path::new(name).file_name().unwrap())).unwrap_or_default())
            };
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", data.len());
            let _ = (&s).write_all(head.as_bytes()).and_then(|_| (&s).write_all(&data));
        });
    }
}
