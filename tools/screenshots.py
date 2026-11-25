"""Bildschirmfotos fuer das README, mit erfundenen Titeln und ohne Netz. Braucht Edge oder Chrome."""
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EDGE = next(p for p in map(Path, (r"C:/Program Files/Microsoft/Edge/Application/msedge.exe", r"C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", r"C:/Program Files/Google/Chrome/Application/chrome.exe")) if p.exists())


def row(i, kind, title, year, file, status, name, folder, sources, size, s=None, e=None, checked=False, note="", cands=()):
    root = r"D:\Plex\Serien" if kind == "tv" else r"D:\Plex\Filme"
    sub = rf"\{folder}\Season {s:02}\{name}" if kind == "tv" else rf"\{folder}\{name}"
    return dict(id=i, file=file, size=size, status=status, note=note, checked=checked, checkable=status in ("sicher", "pruefen", "lokal"), name=name,
                dst=root + sub + ".mkv", folder=folder, season=s, episode=e, title=title, year=year, kind=kind, query=title, poster="",
                overview="Auf einer abgelegenen Forschungsstation empfängt ein kleines Team ein Signal, das es nicht geben dürfte." if title == "Nordlicht" else "",
                sources=list(sources), pick=0 if cands else None, waiting=False, cands=list(cands))


c = [{"label": "Nordlicht (2024)", "score": 100}, {"label": "Nordlichter (2011)", "score": 71}]
rows = [
    row(1, "tv", "Nordlicht", 2024, "Nordlicht.S01E01.1080p.mkv", "sicher", "Nordlicht (2024) - S01E01 - Das Signal", "Nordlicht (2024)", ("tvmaze", "wikidata"), 2.4e9, 1, 1, True, cands=c),
    row(2, "tv", "Nordlicht", 2024, "Nordlicht.S01E02.1080p.mkv", "sicher", "Nordlicht (2024) - S01E02 - Funkstille", "Nordlicht (2024)", ("tvmaze", "wikidata"), 2.2e9, 1, 2, True, cands=c),
    row(3, "tv", "Nordlicht", 2024, "Nordlicht.S01E09.1080p.mkv", "pruefen", "Nordlicht (2024) - S01E09", "Nordlicht (2024)", ("tvmaze", "wikidata"), 2.3e9, 1, 9, note="S01E09 gibt es laut Quelle nicht", cands=c),
    row(4, "movie", "The Long Harbour", 2019, "Der lange Hafen 2019.mkv", "sicher", "The Long Harbour (2019)", "The Long Harbour (2019)", ("wikidata",), 6.1e9, checked=True),
    row(5, "movie", "Papierstadt", 2021, "Papierstadt (2021).mkv", "vorhanden", "Papierstadt (2021)", "Papierstadt (2021)", ("wikidata",), 4.8e9, note="liegt schon in Plex, gleiche Größe"),
    row(6, "movie", "Sommer am See", 2018, "Sommer am See 2018.mp4", "lokal", "Sommer am See (2018)", "Sommer am See (2018)", (), 1.1e9, note="keine Quelle kennt diesen Titel"),
]
names = [("tmdb", "TheMovieDB", True), ("tvdb", "TheTVDB", True), ("tvmaze", "TVmaze", False), ("wikidata", "Wikidata", False), ("anilist", "AniList", False), ("omdb", "OMDb", True)]
state = dict(v=1, rv=1, busy="", info="", version="", progress=dict(i=0, n=0, text="", phase="", bytes=0, total=0, speed=0), plex=dict(ok=True, free=920e9),
             cfg=dict(tv=r"D:\Plex\Serien", movies=r"D:\Plex\Filme", music="", lang="en", format_tv="{plex}", format_movie="{plex}", active=[], folders=[], watch=False, min_size_mb=40, min_age_minutes=10, wiki_posters=False),
             subKey=False, preview={}, rows=rows,
             sources=[dict(id=i, name=n, ready=not k, needsKey=k, hasKey=False, active=not k, movies=True, episodes=True) for i, n, k in names])
shots = {"transfer": "sel=new Set([1]);render()", "dialog": "confirmTransfer(rows.filter(r=>r.checked))"}
out = ROOT / "docs" / "screenshots"
out.mkdir(parents=True, exist_ok=True)
for name, js in shots.items():
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
        d = Path(tmp)
        for f in ("app.js", "style.css"):
            shutil.copy(ROOT / "ui" / f, d)
        demo = "let first=true;window.fetch=async(u,o)=>({json:async()=>{const m=JSON.parse(o.body).method;if(m!=='state')return null;if(!first)return null;first=false;return " + json.dumps(state) + "}})"
        (d / "index.html").write_text(f'<!doctype html><html lang="de"><meta charset="utf-8"><link rel="stylesheet" href="style.css"><body><nav id="side"></nav><main id="main"></main><div id="layer"></div>'
                                      f"<script>{demo}</script><script src=\"app.js\"></script><script>setTimeout(()=>{{{js}}},400)</script>", encoding="utf-8")
        target = out / f"{name}.png"
        subprocess.run([str(EDGE), "--headless=new", "--disable-gpu", "--hide-scrollbars", f"--user-data-dir={d / 'p'}", "--force-device-scale-factor=2",
                        "--window-size=1240,780", "--virtual-time-budget=3000", f"--screenshot={target}", (d / "index.html").as_uri()], capture_output=True, timeout=60)
        print(name, target.exists() and target.stat().st_size)
sys.exit(0)
