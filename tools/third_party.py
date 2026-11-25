"""Erzeugt THIRD-PARTY.txt aus `cargo metadata` (alle Crates, die auf Windows eingebunden werden)."""
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"],
    cwd=ROOT, capture_output=True, text=True, encoding="utf-8", check=True).stdout)
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

# nur was wirklich im Programm landet: normale Abhaengigkeiten ab dem App-Paket
todo, used = [p["id"] for p in meta["packages"] if p["name"] == "spool"], set()
while todo:
    i = todo.pop()
    if i in used:
        continue
    used.add(i)
    for d in nodes[i]["deps"]:
        if any(k["kind"] is None for k in d["dep_kinds"]):
            todo.append(d["pkg"])

own = set(meta["workspace_members"])
crates = sorted((packages[i] for i in used if i not in own), key=lambda p: (p["name"], p["version"]))
texts: dict[str, list[str]] = {}
out = ["Spool verwendet die folgenden Bausteine. Ihre Lizenztexte stehen darunter.", ""]
for p in crates:
    name = f"{p['name']} {p['version']}"
    out.append(f"{name}  |  {p.get('license') or 'siehe Text'}  |  {p.get('repository') or ''}")
    folder = Path(p["manifest_path"]).parent
    for f in sorted(folder.iterdir()):
        if f.is_file() and f.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE")):
            texts.setdefault(f.read_text(encoding="utf-8", errors="replace").strip(), []).append(name)
out += ["Microsoft Edge WebView2 Loader  |  BSD-3-Clause  |  https://aka.ms/webview2", ""]
for text, names in sorted(texts.items(), key=lambda x: x[1][0]):
    out += ["=" * 78, ", ".join(names), "=" * 78, text, ""]
(ROOT / "THIRD-PARTY.txt").write_text("\n".join(out), encoding="utf-8", newline="\n")
print(len(crates), "Crates,", len(texts), "Lizenztexte")
missing = [p["name"] for p in crates if not p.get("license") and not p.get("license_file")]
print("ohne Lizenzangabe:", missing)
