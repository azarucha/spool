//! Kopieren mit Fortsetzen, Pruefsumme und Wiederholung. Ueberschreibt nie.
//!
//! Die Quelle wird so geoeffnet, dass niemand sie waehrenddessen schreiben kann; die Teildatei so,
//! dass es nur einen Schreiber gibt. Beide Griffe bleiben bis zum Schluss offen.
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CHUNK: usize = 4 * 1024 * 1024;
pub const RESERVE: u64 = 5 << 30;
pub const PARTIAL: &str = "spool-partial";

/// (Phase "copy" | "verify", Bytes, Gesamt) -> weitermachen?
pub type Progress<'a> = &'a (dyn Fn(&str, u64, u64) -> bool + Sync);

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn quiet(_: &str, _: u64, _: u64) -> bool {
    true
}

fn err(text: &str) -> io::Error {
    io::Error::other(text)
}

pub fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Abgebrochen")
}

#[cfg(windows)]
fn share(o: &mut OpenOptions) -> &mut OpenOptions {
    use std::os::windows::fs::OpenOptionsExt;
    o.share_mode(0x1 | 0x4) // andere duerfen lesen und umbenennen/loeschen, aber nicht schreiben
}

#[cfg(not(windows))]
fn share(o: &mut OpenOptions) -> &mut OpenOptions {
    o
}

/// Datei zum Lesen oeffnen und gegen fremdes Schreiben sperren. Scheitert, wenn gerade jemand schreibt.
pub fn hold(p: &Path) -> io::Result<File> {
    share(OpenOptions::new().read(true)).open(p)
}

/// Schreibt gerade jemand anderes (Download, Aufnahme)?
pub fn in_use(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33))
}

pub fn disk_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull || (cfg!(windows) && matches!(e.raw_os_error(), Some(39 | 112)))
}

/// Nur Netzwerk- und Zeitfehler lohnen einen neuen Versuch.
fn transient(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    let net = [51, 53, 54, 55, 59, 64, 65, 67, 121, 233, 240, 1231, 1232, 1236, 1237];
    matches!(e.kind(), TimedOut | ConnectionReset | ConnectionAborted | BrokenPipe | HostUnreachable | NetworkUnreachable | NetworkDown)
        || (cfg!(windows) && e.raw_os_error().is_some_and(|c| net.contains(&c)))
        || e.to_string().starts_with("SHA-256")
}

fn hash(f: &mut impl Read, buf: &mut [u8], total: u64, progress: Progress) -> io::Result<String> {
    let (mut h, mut pos) = (Sha256::new(), 0u64);
    loop {
        let n = f.read(buf)?;
        if n == 0 {
            return Ok(hex(&h.finalize()));
        }
        h.update(&buf[..n]);
        pos += n as u64;
        if !progress("verify", pos, total) {
            return Err(cancelled());
        }
    }
}

/// Moeglichst am Zwischenspeicher vorbei lesen, damit wirklich das Geschriebene geprueft wird.
pub fn sha256(path: &Path, progress: Progress) -> io::Result<String> {
    const ALIGN: usize = 4096;
    let total = fs::metadata(path)?.len();
    let mut raw = vec![0u8; CHUNK + ALIGN];
    let at = raw.as_ptr().align_offset(ALIGN);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const NO_BUFFERING: u32 = 0x2000_0000;
        if let Ok(mut f) = OpenOptions::new().read(true).custom_flags(NO_BUFFERING).open(path) {
            match hash(&mut f, &mut raw[at..at + CHUNK], total, progress) {
                Err(e) if e.kind() != io::ErrorKind::Interrupted => {} // Laufwerk kann das nicht: normal lesen
                done => return done,
            }
        }
    }
    hash(&mut File::open(path)?, &mut raw[at..at + CHUNK], total, progress)
}

pub fn sha256_held(f: &File, progress: Progress) -> io::Result<String> {
    let mut r = f;
    r.seek(SeekFrom::Start(0))?;
    hash(&mut r, &mut vec![0u8; CHUNK], f.metadata()?.len(), progress)
}

pub fn partial(dst: &Path) -> PathBuf {
    let mut n = dst.file_name().unwrap_or_default().to_os_string();
    n.push(".");
    n.push(PARTIAL);
    dst.with_file_name(n)
}

/// Langer, bereinigter Pfad fuer die Windows-Schnittstelle (loest ".." und "/" auf).
#[cfg(windows)]
pub fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let full = std::path::absolute(p).unwrap_or_else(|_| p.into());
    let s = full.to_string_lossy();
    let long = if s.starts_with(r"\\?\") {
        s.into_owned()
    } else if let Some(unc) = s.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{unc}")
    } else {
        format!(r"\\?\{s}")
    };
    std::ffi::OsStr::new(&long).encode_wide().chain([0]).collect()
}

/// Umbenennen, das scheitert, wenn das Ziel existiert. std::fs::rename wuerde unter Windows ersetzen.
#[cfg(windows)]
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let (a, b) = (wide(from), wide(to));
    // SAFETY: beide Zeiger zeigen auf nullterminierte UTF-16-Puffer, die den Aufruf ueberleben.
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

/// Liegen beide auf verschiedenen Laufwerken?
pub fn other_volume(e: &io::Error) -> bool {
    cfg!(windows) && e.raw_os_error() == Some(17) || e.kind() == io::ErrorKind::CrossesDevices
}

fn copy_once(src: &File, dst: &Path, progress: Progress) -> io::Result<String> {
    if dst.exists() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let part = partial(dst);
    let meta = src.metadata()?;
    let (mut h, mut buf, mut pos) = (Sha256::new(), vec![0u8; CHUNK], 0u64);
    let mut reader = src;
    reader.seek(SeekFrom::Start(0))?;
    // Einziger Schreiber: ein zweiter Versuch, egal woher, scheitert hier
    let mut fo = share(OpenOptions::new().read(true).write(true).create(true).truncate(false)).open(&part)?;
    let mut offset = fo.metadata()?.len();
    if offset > meta.len() {
        fo.set_len(0)?;
        offset = 0;
    }
    fo.seek(SeekFrom::Start(offset))?;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        let skip = offset.saturating_sub(pos).min(n as u64) as usize; // schon uebertragene Bytes
        fo.write_all(&buf[skip..n])?;
        pos += n as u64;
        if !progress("copy", pos, meta.len()) {
            return Err(cancelled()); // Teildatei bleibt zum Fortsetzen
        }
    }
    fo.sync_all()?;
    let digest = hex(&h.finalize());
    if pos != meta.len() || fo.metadata()?.len() != pos || sha256(&part, progress)? != digest {
        drop(fo);
        fs::remove_file(&part)?;
        return Err(err("SHA-256 stimmt nicht, Teilkopie verworfen"));
    }
    if let Ok(t) = meta.modified() {
        let _ = fo.set_modified(t); // SMB-Freigaben erlauben das nicht immer
    }
    rename_new(&part, dst)?;
    if fo.metadata()?.len() != meta.len() {
        return Err(err("Zieldatei hat nach dem Kopieren eine andere Größe"));
    }
    Ok(digest)
}

pub fn copy_verified(src: &File, dst: &Path, progress: Progress, tries: u32, mut wait: Duration) -> io::Result<String> {
    let mut attempt = 1;
    loop {
        let result = dst.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| copy_once(src, dst, progress));
        match result {
            Err(e) if transient(&e) && attempt < tries => {
                let end = std::time::Instant::now() + wait;
                while std::time::Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(200).min(wait));
                    if !progress("wait", 0, 0) {
                        return Err(cancelled());
                    }
                }
                wait *= 2;
                attempt += 1;
            }
            other => return other,
        }
    }
}

pub fn copy(src: &File, dst: &Path, progress: Progress) -> io::Result<String> {
    copy_verified(src, dst, progress, 5, Duration::from_secs(5))
}

/// Liegengebliebene Teildateien unterhalb der Ordner.
pub fn leftovers(roots: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() && depth < 4 {
                walk(&e.path(), out, depth + 1);
            } else if t.is_file() && e.path().extension().is_some_and(|x| x == PARTIAL) {
                out.push(e.path());
            }
        }
    }
    let mut out = vec![];
    roots.iter().filter(|r| !r.as_os_str().is_empty()).for_each(|r| walk(r, &mut out, 0));
    out
}
