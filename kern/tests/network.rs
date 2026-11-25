//! Gegen ein echtes Netzlaufwerk, nur auf Wunsch:
//! `SPOOL_NET=X:\neuer-ordner cargo test -p kern --test network -- --ignored --nocapture`
//! Der Ordner darf noch nicht existieren, wird angelegt und am Ende entfernt.
use kern::copy::{self, quiet};
use kern::engine::*;
use kern::library;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore]
fn real_network_drive() {
    let root = PathBuf::from(std::env::var("SPOOL_NET").expect("SPOOL_NET setzen"));
    assert!(!root.exists(), "nur ein neuer Ordner");
    fs::create_dir(&root).unwrap();
    let _d = Dir(root.clone());
    let here = tempfile::tempdir().unwrap();
    std::env::set_var("SPOOL_HOME", here.path().join("home"));
    let data: Vec<u8> = (0..60_000_000u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    let src = here.path().join("Old 2021.mkv");
    fs::write(&src, &data).unwrap();

    println!("Ordnerprobe: {:?}", library::check(&root.to_string_lossy()));
    let dst = root.join("Größe & Äpfel (2021)").join("Größe & Äpfel (2021).mkv");
    let part = copy::partial(&dst);

    let held = copy::hold(&src).unwrap();
    let e = copy::copy(&held, &dst, &|_, pos, _| pos < 20_000_000).unwrap_err();
    println!("Abbruch: {e}; Rest {} MB", fs::metadata(&part).unwrap().len() / 1_000_000);

    let rival = Mutex::new(None);
    let t = Instant::now();
    let sha = copy::copy_verified(
        &held,
        &dst,
        &|phase, _, _| {
            let mut r = rival.lock().unwrap();
            if phase == "verify" && r.is_none() {
                let write = fs::OpenOptions::new().append(true).open(&part).is_err();
                let again = copy::copy_verified(&copy::hold(&src).unwrap(), &dst, &quiet, 1, Duration::ZERO).is_err();
                *r = Some((write, again));
            }
            true
        },
        1,
        Duration::ZERO,
    )
    .unwrap();
    println!("fortgesetzt und geprüft in {:?}; fremder Schreiber abgewiesen: {:?}", t.elapsed(), rival.lock().unwrap());
    assert_eq!(*rival.lock().unwrap(), Some((true, true)));
    assert!(fs::read(&dst).unwrap() == data && !part.exists());
    assert_eq!(copy::sha256(&dst, &quiet).unwrap(), sha);
    assert!(copy::copy(&held, &dst, &quiet).is_err(), "überschreiben");
    drop(held);

    // Verschieben ueber Laufwerksgrenzen, mit Untertitel, dann rueckgaengig
    let film = here.path().join("Neu 2022.mkv");
    fs::write(&film, &data[..5_000_000]).unwrap();
    fs::write(here.path().join("Neu 2022.de.srt"), "1").unwrap();
    let mut row = Row::new(&film).unwrap();
    row.dst = Some(root.join("Neu (2022)").join("Neu (2022).mkv"));
    let mut log = vec![];
    execute(&mut row, "move", &mut |e| { log.push(e); Ok(()) }, &quiet).unwrap();
    println!("Verschoben: {} ({} Einträge)", row.note, log.len());
    assert!(!film.exists() && log.len() == 2 && row.dst.as_ref().unwrap().exists());
    let cfg = Settings { movies: root.to_string_lossy().into(), ..Default::default() };
    for e in log.iter().rev() {
        println!("{}", undo(e, &cfg, &mut |_| Ok(()), &quiet).unwrap());
    }
    assert!(fs::read(&film).unwrap() == data[..5_000_000] && !root.join("Neu (2022)").exists() && root.exists());

    // Datei auf dem NAS, in die noch geschrieben wird
    let open = root.join("laeuft.mkv");
    let w = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&open).unwrap();
    println!("offene Datei auf dem NAS erkannt: {}", copy::hold(&open).is_err_and(|e| copy::in_use(&e)));
    drop(w);
    assert!(copy::hold(&open).is_ok());

    // Verschieben innerhalb des Netzlaufwerks: umbenennen
    let mut row = Row::new(&dst).unwrap();
    row.dst = Some(root.join("B").join("b.mkv"));
    let t = Instant::now();
    execute(&mut row, "move", &mut |_| Ok(()), &quiet).unwrap();
    println!("auf demselben Laufwerk verschoben in {:?}", t.elapsed());
    assert!(root.join("B").join("b.mkv").exists() && !dst.exists());
    assert!(copy::leftovers(std::slice::from_ref(&root)).is_empty());
}
