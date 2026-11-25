'use strict';
/* Alle fremden Texte (Dateinamen, Titel aus dem Netz) landen nur ueber textContent im Dokument. */
const native = window.__TAURI__;
const call = async (method, ...args) => {
  if (native) return native.core.invoke('call', { method, args });
  const r = await fetch('/call', { method: 'POST', body: JSON.stringify({ method, args }) });
  return r.json();
};
/* Aktion ausfuehren, Rueckmeldung zeigen, Anzeige auffrischen. */
const act = async (method, ...args) => {
  const m = await call(method, ...args);
  if (typeof m === 'string' && m) toast(m);
  await poll();
  return m;
};

const ICON = {
  note: 'M4 5.500h16v11H10l-4.500 3.500v-3.500H4zM8 9.500h8M8 12.800h5',
  move: 'M4 7h11M11 3l4 4-4 4M20 17H9M13 13l-4 4 4 4',
  list: 'M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01',
  shield: 'M12 3l7 3v5c0 4.6-2.9 8.2-7 10-4.1-1.8-7-5.4-7-10V6zM9 12l2.2 2.2L15.2 10',
  clock: 'M12 7v5l3.2 2M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0z',
  gear: 'M4 7h10M18 7h2M4 17h2M10 17h10M16 7a2 2 0 1 1-4 0 2 2 0 0 1 4 0zM10 17a2 2 0 1 1-4 0 2 2 0 0 1 4 0z',
  info: 'M12 11v5.500M12 7.500v.01M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0z',
  plus: 'M12 5v14M5 12h14',
  folder: 'M3 7.5A1.5 1.5 0 0 1 4.5 6H9l2 2.2h8.5A1.5 1.5 0 0 1 21 9.7v7.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  find: 'M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM20 20l-4-4',
  globe: 'M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0zM3.4 12h17.2M12 3c2.5 2.6 3.7 5.6 3.7 9s-1.2 6.400-3.700 9c-2.5-2.6-3.7-5.6-3.7-9s1.200-6.400 3.700-9z',
  down: 'M12 4v11M7.500 10.500L12 15l4.500-4.500M4.500 19.500h15',
  trash: 'M5 7h14M10 7V4.500h4V7M7 7l.8 12.500h8.400L17 7',
  redo: 'M19 12a7 7 0 1 1-2.100-5M19 4v4h-4',
};
const LABEL = {
  sicher: 'Sicher', pruefen: 'Prüfen', lokal: 'Nur Dateiname', kein: 'Kein Treffer', vorhanden: 'Schon in Plex', konflikt: 'Konflikt',
  laeuft: 'Noch in Arbeit', fertig: 'Fertig', fehler: 'Fehler', wartet: 'Wartet',
  stimmt: 'Stimmt', abweichend: 'Weicht ab', fehlt: 'Fehlt', berechnet: 'Berechnet', rechnet: 'Rechnet', fremd: 'Fremder Pfad',
};
const PAGES = [['transfer', 'Übertragen', 'move'], ['episodes', 'Episodenlisten', 'list'], ['subs', 'Untertitel', 'note'], ['sums', 'Prüfsummen', 'shield'],
  ['history', 'Verlauf', 'clock'], ['settings', 'Einstellungen', 'gear'], ['about', 'Über Spool', 'info']];
const OPEN = ['pruefen', 'lokal', 'kein', 'konflikt', 'fehler'];

let S = null, rows = [], page = 'transfer', sel = new Set(), anchor = null, filter = 'alle', pop = null, lastInfo = '', wasBusy = '';
const byId = new Map();
const U = { shows: [], show: -1, eps: null, epsText: '', epsNote: '', sums: [], hist: [], keyMsg: {}, fmtMsg: {}, search: {}, subs: [], subLang: 'de', subBusy: -1,
  draft: {}, pathMsg: {}, found: undefined, aliases: [], partials: null };

/* ---------------------------------------------------------------- Bausteine */
function h(tag, props, ...kids) {
  const [name, ...cls] = tag.split('.');
  const e = document.createElement(name || 'div');
  if (cls.length) e.className = cls.join(' ');
  for (const [k, v] of Object.entries(props || {})) {
    if (v === false || v == null) continue;
    if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (k === 'value') e.value = v;
    else e.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat(9)) if (k != null && k !== false) e.append(k);
  return e;
}
function svg(d, rule) {
  const ns = 'http://www.w3.org/2000/svg', s = document.createElementNS(ns, 'svg'), p = document.createElementNS(ns, 'path');
  s.setAttribute('viewBox', '0 0 24 24');
  s.setAttribute('aria-hidden', 'true');
  p.setAttribute('d', d);
  if (rule) p.setAttribute('fill-rule', rule);
  s.append(p);
  return s;
}
const icon = name => svg(ICON[name]);
function reel() {
  const dot = (x, y, r) => `M${x - r} ${y}a${r} ${r} 0 1 0 ${2 * r} 0a${r} ${r} 0 1 0 ${-2 * r} 0z`;
  let d = dot(12, 12, 9.5) + dot(12, 12, 1.5);
  for (let i = 0; i < 5; i++) d += dot(12 + 5.4 * Math.sin(i * 1.25664), 12 - 5.4 * Math.cos(i * 1.25664), 2);
  return svg(d, 'evenodd');
}
const btn = (text, fn, o = {}) => h('button.btn' + (o.cls || ''), { onclick: fn, disabled: o.off, title: o.tip, 'aria-label': text ? null : o.tip, 'data-focus': 'b' + (o.key || '') + (text || o.tip) }, o.icon && icon(o.icon), text);
const box = (key, on, part, off, fn) => h('button.box' + (on ? '.on' : part ? '.part' : ''), {
  role: 'checkbox', 'aria-checked': on ? 'true' : part ? 'mixed' : 'false', 'aria-label': 'Auswählen', 'data-focus': 'x' + key, disabled: off, onclick: e => { e.stopPropagation(); fn(!on); },
}, icon('check'));
const toggle = (on, fn, label, off) => h('button.switch' + (on ? '.on' : ''), { role: 'switch', 'aria-checked': String(!!on), 'aria-label': label, 'data-focus': 't' + label, disabled: off, onclick: () => fn(!on) });
const seg = (opts, cur, fn) => h('.seg', { role: 'group' }, opts.map(([k, t]) => h('button' + (k === cur ? '.on' : ''), { 'aria-pressed': String(k === cur), 'data-focus': 's' + k, onclick: () => fn(k) }, t)));
const pill = s => h('span.pill.' + s, null, LABEL[s] || s);
const gb = n => n >= 1e9 ? (n / 1e9).toFixed(n >= 1e11 ? 0 : 1).replace('.', ',') + ' GB' : Math.max(1, Math.round(n / 1e6)) + ' MB';
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const line = (title, sub, ...right) => h('.ln', null, h('.grow', null, title, sub ? h('small', null, sub) : null), right);
const code = (s, e) => `S${String(s).padStart(2, '0')}E${String(e).padStart(2, '0')}`;
function art(url, title) {
  if (url && url.startsWith('https://')) {
    const i = h('img.art', { src: url, alt: '', loading: 'lazy', draggable: 'false', referrerpolicy: 'no-referrer' });
    i.addEventListener('error', () => i.replaceWith(art('', title)));
    return i;
  }
  let hue = 0;
  for (const c of title || '?') hue = (hue * 31 + c.codePointAt(0)) % 360;
  const words = (title || '?').split(/\s+/).filter(Boolean);
  const e = h('.art.none', null, words.slice(0, 2).map(w => [...w][0]).join('').toUpperCase());
  e.style.background = `linear-gradient(160deg, hsl(${hue} 52% 62%), hsl(${(hue + 30) % 360} 58% 42%))`;
  return e;
}
const srcName = id => (S.sources.find(s => s.id === id) || { name: id }).name;
function toast(text) {
  if (!text) return;
  document.querySelectorAll('.toast').forEach(t => t.remove());
  const t = h('.toast', { role: 'status' }, text);
  (document.querySelector('dialog[open]') || document.body).append(t);
  setTimeout(() => t.remove(), Math.min(12000, 3500 + text.length * 45));
}

/* ---------------------------------------------------------------- Zeichnen */
function keep(root, draw) {
  // Rollstand und Fokus ueber das Neuzeichnen retten
  const pos = [...root.querySelectorAll('[data-keep]')].map(e => [e.dataset.keep, e.scrollTop, e.scrollLeft]);
  const f = document.activeElement, id = f && root.contains(f) && f.dataset ? f.dataset.focus : null;
  const at = id && f.tagName === 'INPUT' ? [f.value, f.selectionStart, f.selectionEnd] : null;
  draw();
  for (const [k, t, l] of pos) {
    const e = root.querySelector(`[data-keep="${k}"]`);
    if (e) { e.scrollTop = t; e.scrollLeft = l; }
  }
  if (!id || root.contains(document.activeElement) && document.activeElement.dataset.focus === id) return;
  const n = root.querySelector(`[data-focus="${CSS.escape(id)}"]`);
  if (!n) return;
  n.focus({ preventScroll: true });
  if (at && n.tagName === 'INPUT') { n.value = at[0]; try { n.setSelectionRange(at[1], at[2]); } catch { /* Zahlenfelder */ } }
}
function render() {
  if (!S) return;
  const side = document.getElementById('side'), main = document.getElementById('main');
  const setUp = S.cfg.tv || S.cfg.movies || S.cfg.music;
  keep(side, () => side.replaceChildren(
    h('.brand', null, h('i', null, reel()), 'Spool'),
    ...PAGES.map(([k, t, i]) => h('button.nav' + (k === page ? '.on' : ''), { onclick: () => go(k), 'aria-current': k === page ? 'page' : null, 'data-focus': 'n' + k }, icon(i), t,
      k === 'transfer' && rows.length ? h('b', null, String(rows.length)) : null)),
    h('.plex' + (S.plex.ok ? '.ok' : ''), null, h('i'), h('div', null, h('span', null, S.plex.ok ? 'Plex-Ordner bereit' : setUp ? 'Plex nicht erreichbar' : 'Plex einrichten'),
      S.plex.ok ? gb(S.plex.free) + ' frei' : setUp ? 'Netzlaufwerk prüfen' : 'Ordner noch nicht gewählt')),
  ));
  keep(main, () => main.replaceChildren(...[VIEW[page]()].flat()));
  if (page === 'transfer') showSelection();
  drawPop();
}
function go(k) {
  page = k;
  if (k === 'history') call('history').then(r => { U.hist = r || []; render(); });
  if (k === 'sums') pullSums();
  if (k === 'subs') call('subs').then(pullSubs);
  if (k === 'about') call('aliases').then(r => { U.aliases = r || []; render(); });
  render();
}

/* ---------------------------------------------------------------- Ersteinrichtung und Pfade */
async function savePath(k, v) {
  if (v === S.cfg[k]) { delete U.draft['p' + k]; U.pathMsg[k] = ''; return; }
  U.pathMsg[k] = await call('set', k, v);
  if (!U.pathMsg[k]) delete U.draft['p' + k];
  await poll(); render();
}
const choosePath = async k => { U.pathMsg[k] = await call('choose_dir', k) || ''; delete U.draft['p' + k]; await poll(); render(); };
const pathLine = (k, label, hint) => line(label, U.pathMsg[k] || hint, input('p' + k, S.cfg[k], v => savePath(k, v), { hint: 'Pfad eingeben oder wählen', label }), btn('Wählen', () => choosePath(k), { key: k }));
/* Eingabefeld, dessen ungesicherter Inhalt das Neuzeichnen uebersteht. */
function input(key, value, commit, o = {}) {
  const e = h('input.field', { 'data-focus': 'f' + key, value: U.draft[key] ?? value ?? '', placeholder: o.hint, 'aria-label': o.label || o.hint, type: o.type, style: o.style, spellcheck: 'false', autocomplete: 'off', inputmode: o.mode });
  e.addEventListener('input', () => { U.draft[key] = e.value; });
  e.addEventListener('keydown', ev => { if (ev.key === 'Enter') { ev.preventDefault(); commit(e.value); } });
  if (!o.enterOnly) e.addEventListener('blur', () => setTimeout(() => {
    const f = document.activeElement;
    if (!(f && f.dataset && f.dataset.focus === 'f' + key) && U.draft[key] !== undefined) commit(U.draft[key]);
  }));
  return e;
}
async function findPlex() {
  U.found = null; render();
  U.found = await call('find_plex') || []; render();
}
const takeFound = async f => { for (const k of ['tv', 'movies', 'music']) if (f[k]) U.pathMsg[k] = await call('set', k, f[k]); U.draft = {}; await poll(); render(); };
const viewSetup = () => h('.page', null, h('.scroll', { 'data-keep': 'setup' }, h('.setwrap', null,
  h('.welcome', null, h('.drop', null, reel()), h('h2', null, 'Wo liegt deine Plex-Bibliothek?'), h('p', null, 'Spool sortiert deine Dateien in diese Ordner ein. Eine Art genügt, Laufwerke und Netzwerkpfade wie \\\\server\\plex\\Filme gehen beide.')),
  U.found === undefined ? h('.center', null, btn('Alle Laufwerke durchsuchen', findPlex, { cls: '.blue', icon: 'find', tip: 'Sieht auf jedem verbundenen Laufwerk nach, auch im Netzwerk' }), btn('Hauptordner wählen', () => act('choose_root').then(render), { icon: 'folder' }))
    : U.found === null ? h('.center', null, h('span', null, 'Suche auf allen Laufwerken …'))
      : U.found.length ? h('.set', null, U.found.map((f, i) => line(f.movies.replace(/\\[^\\]*$/, '') || f.movies, [f.tv, f.movies, f.music].filter(Boolean).join('  ·  '), btn('Übernehmen', () => takeFound(f), { cls: '.blue', key: i }))))
        : h('.center', null, h('span', null, 'Nichts gefunden. Bitte von Hand wählen.')),
  h('h3', null, 'Oder einzeln'),
  h('.set', null, pathLine('tv', 'Serien'), pathLine('movies', 'Filme'), pathLine('music', 'Musik')),
  h('p.foot', null, 'Zum Erkennen fragt Spool Online-Datenbanken mit dem Titel aus dem Datei- oder Ordnernamen. Was genau wohin geht, steht unter „Über Spool“.'))));

/* ---------------------------------------------------------------- Übertragen */
const isOpen = r => !r.waiting && OPEN.includes(r.status);
const shown = () => rows.filter(r => filter === 'alle' || (filter === 'offen' ? isOpen(r) : r.status === 'sicher'));
const picked = () => rows.filter(r => sel.has(r.id));
const visibleIds = () => [...document.querySelectorAll('.row[data-id]')].map(x => +x.dataset.id);
function choose(r, e) {
  if (e.shiftKey && anchor != null) {
    const ids = visibleIds(), a = ids.indexOf(anchor), b = ids.indexOf(r.id);
    if (a >= 0 && b >= 0) sel = new Set(ids.slice(Math.min(a, b), Math.max(a, b) + 1));
  } else if (e.ctrlKey) {
    sel.has(r.id) ? sel.delete(r.id) : sel.add(r.id);
    anchor = r.id;
  } else {
    sel = new Set([r.id]);
    anchor = r.id;
  }
  showSelection();
}
/* Nur die Markierung und die Detailspalte erneuern, nicht die ganze Liste. */
let painted = new Set();
function showSelection() {
  for (const id of new Set([...painted, ...sel])) {
    const e = document.querySelector(`.row[data-id="${id}"]`);
    if (!e) continue;
    e.classList.toggle('sel', sel.has(id));
    e.setAttribute('aria-selected', String(sel.has(id)));
    e.tabIndex = sel.has(id) ? 0 : -1;
  }
  painted = new Set(sel);
  const main = document.getElementById('main');
  keep(main, () => {
    main.querySelector('.insp')?.remove();
    main.append(...[inspector()].flat());
  });
}
function groups(list) {
  const m = new Map();
  for (const r of list) {
    const k = r.kind === 'movie' ? 'm' + r.id : r.kind + (r.folder || r.title);
    m.has(k) ? m.get(k).push(r) : m.set(k, [r]);
  }
  return m;
}
const check = (list, on) => act('check', list.filter(r => r.checkable).map(r => r.id), on);
const rowProps = r => ({
  'data-id': r.id, title: r.dst, role: 'option', tabindex: r === rows[0] ? '0' : '-1', 'aria-selected': 'false', 'data-focus': 'r' + r.id,
  onclick: e => choose(byId.get(r.id) || r, e), ondblclick: () => call('reveal', r.id),
});
const filmRow = (r, busy) => h('.row.film', rowProps(r),
  box('r' + r.id, r.checked, false, !r.checkable || busy, v => check([r], v)), art(r.poster, r.title),
  h('.grow', null, h('h3', null, r.title, r.year ? h('span', null, ` ${r.year}`) : null), h('.o', null, r.file)), h('.sz', null, gb(r.size)), pill(r.waiting ? 'wartet' : r.status));
function groupView(g, busy) {
  const f = g[0], can = g.filter(r => r.checkable), on = can.filter(r => r.checked).length;
  if (f.kind === 'movie') return h('.group', { role: 'presentation' }, filmRow(f, busy));
  return h('.group', { role: 'group', 'aria-label': f.title },
    h('.ghead', null, box('g' + f.id, can.length > 0 && on === can.length, on > 0, !can.length || busy, v => check(g, v)), art(f.poster, f.title),
      h('.grow', null, h('h3', null, f.title, f.year ? h('span', null, ` ${f.year}`) : null),
        h('p', null, [f.kind === 'music' ? plural(g.length, 'Titel', 'Titel') : plural(g.length, 'Folge', 'Folgen'), gb(g.reduce((a, r) => a + r.size, 0)),
          f.waiting ? 'wartet auf Abgleich' : f.kind === 'music' ? 'aus den Tags' : f.sources.length ? f.sources.map(srcName).join(', ') : 'nur Dateiname'].join('  ·  ')))),
    g.map(r => cachedRow(r, busy, () => h('.row', rowProps(r),
      box('r' + r.id, r.checked, false, !r.checkable || busy, v => check([r], v)),
      h('.grow', null, h('.n', null, r.name || r.file), h('.o', null, r.name ? r.file : r.note)),
      h('.sz', null, gb(r.size)), pill(r.waiting ? 'wartet' : r.status)))));
}
let rowCache = new Map();
function cachedRow(r, busy, make) {
  const sig = busy + r.sig, old = rowCache.get(r.id);
  if (old && old.sig === sig) return old.el;
  const el = make();
  rowCache.set(r.id, { sig, el });
  return el;
}
/* Unveraenderte Gruppen werden wiederverwendet, damit auch Tausende Zeilen fluessig bleiben. */
let cache = new Map();
function groupList(list, busy) {
  const fresh = new Map(), out = [];
  for (const [k, g] of groups(list)) {
    const sig = busy + '|' + g.map(r => r.sig).join();
    const old = cache.get(k), e = old && old.sig === sig ? old : { sig, el: groupView(g, busy) };
    fresh.set(k, e);
    out.push(e.el);
  }
  cache = fresh;
  if (rowCache.size > byId.size + 500) for (const id of [...rowCache.keys()]) if (!byId.has(id)) rowCache.delete(id);
  return out;
}
const viewTransfer = () => {
  if (!S.cfg.tv && !S.cfg.movies && !S.cfg.music) return viewSetup();
  const busy = !!S.busy, list = shown(), on = S.sources.filter(s => s.active).length;
  const head = h('.head', null, h('h1', null, 'Übertragen'),
    btn(`${plural(on, 'Quelle', 'Quellen')} aktiv`, e => { pop = pop ? null : e.currentTarget.getBoundingClientRect(); drawPop(); e.stopPropagation(); }, { icon: 'globe', tip: 'Datenquellen ein- und ausschalten', key: 'src' }),
    rows.length ? [btn('', () => act('match', false), { icon: 'redo', cls: '.icon', off: busy, tip: 'Neu abgleichen, Bestätigtes bleibt' }),
      btn('', () => { sel.clear(); act('clear'); }, { icon: 'trash', cls: '.icon', off: busy, tip: 'Liste leeren' })] : null,
    btn('Dateien', () => act('add_files'), { icon: 'plus' }),
    btn('Ordner', () => act('add_folder'), { icon: 'folder' }));
  if (!rows.length) {
    return h('.page', null, head, h('.empty', null, h('.drop', null, icon('down')), h('h2', null, 'Dateien hierher ziehen'),
      h('p', null, 'Spool erkennt Filme, Serien und Musik, entpackt Archive und zeigt dir den Zielnamen, bevor etwas passiert.'),
      h('.acts', null, btn('Quellordner einlesen', async () => { if (await call('add_sources') === 'keine') go('settings'); poll(); }, { cls: '.blue', off: busy }),
        btn('Dateien wählen', () => act('add_files')))), busy ? dock() : null);
  }
  const n = f => rows.filter(f).length;
  const tools = h('.tools', null, seg([['alle', `Alle ${rows.length}`], ['sicher', `Sicher ${n(r => r.status === 'sicher')}`], ['offen', `Offen ${n(isOpen)}`]], filter, k => { filter = k; render(); }),
    h('.sp'), btn('Alle sicheren wählen', () => check(rows.filter(r => r.status === 'sicher'), true), { cls: '.plain', off: busy }),
    btn('Keine', () => check(rows, false), { cls: '.plain', off: busy }));
  const body = h('.scroll', { 'data-keep': 'list', role: 'listbox', 'aria-multiselectable': 'true', 'aria-label': 'Dateien', onclick: e => { if (e.target === e.currentTarget) { sel.clear(); showSelection(); } } },
    list.length ? groupList(list, busy) : h('.blank', null, 'Hier ist gerade nichts.'));
  return h('.page', null, head, tools, body, dock());
};
function dock() {
  const p = S.progress;
  if (S.busy) {
    const moving = S.busy === 'transfer' || S.busy === 'undo';
    const part = p.total ? p.bytes / p.total : 0, done = p.n ? (p.i + (moving ? (p.phase === 'verify' ? .6 + part * .4 : part * .6) : 0)) / p.n : 0;
    const word = { verify: 'Prüfe', wait: 'Warte auf das Netzwerk' }[p.phase] || 'Kopiere';
    const head = S.busy === 'undo' ? 'Mache rückgängig' : moving ? `${word} ${Math.min(p.i + 1, p.n)} von ${p.n}` : { match: 'Gleiche ab', extract: 'Entpacke', scan: 'Lese ein' }[S.busy] || 'Suche';
    const extra = moving && p.speed ? `  ·  ${Math.round(p.speed / 1e6)} MB/s` : '';
    const bar = h('i');
    bar.style.transform = `scaleX(${Math.min(done, 1)})`;
    return h('.dock', { role: 'status' }, h('.grow', null, h('.t', null, head + extra), h('.s', null, p.text || '…'), h('.bar', null, bar)),
      btn('Abbrechen', () => act('stop')));
  }
  const todo = rows.filter(r => r.checked);
  return h('.dock', null, h('.grow', null, h('.t', null, todo.length ? `${plural(todo.length, 'Datei', 'Dateien')} ausgewählt` : 'Nichts ausgewählt'),
    h('.s', null, todo.length ? `${gb(todo.reduce((a, r) => a + r.size, 0))} nach Plex` : S.info || 'Wähle Dateien aus oder bestätige offene Treffer.')),
  btn('Übertragen …', () => confirmTransfer(todo), { cls: '.blue', off: !todo.length }));
}
function confirmTransfer(todo) {
  const list = todo.map(r => ({ id: r.id, dst: r.dst })), total = todo.reduce((a, r) => a + r.size, 0), open = todo.filter(r => r.status !== 'sicher').length;
  let action = 'copy'; // jedes Mal neu: Verschieben ist nie voreingestellt
  sheet(() => [h('h2', { id: 'sheet-title' }, `${plural(todo.length, 'Datei', 'Dateien')} übertragen? (${gb(total)})`),
    h('.plan', { 'data-keep': 'plan', tabindex: '0', 'aria-label': 'Geplante Ziele' }, todo.map(r => h('div', null, h('b', null, r.file), h('span', null, '→ ' + r.dst)))),
    open ? h('.note', null, `${open} davon hast du selbst bestätigt oder ausgewählt, ohne dass eine Quelle sie sicher kennt.`) : null,
    seg([['copy', 'Kopieren'], ['move', 'Verschieben']], action, k => { action = k; drawSheet(); }),
    h('p', { style: 'margin-top:14px' }, action === 'move' ? 'Die Originale werden nach bestandener Prüfsumme endgültig gelöscht, nicht in den Papierkorb gelegt.' : 'Die Originale bleiben, wo sie sind.'),
    h('.acts', null, btn('Zurück', closeSheet), btn(action === 'move' ? `${todo.length} verschieben` : `${todo.length} kopieren`, () => { closeSheet(); act('transfer', list, action); }, { cls: action === 'move' ? '.red' : '.blue' }))], '.wide');
}
function inspector() {
  const list = picked(), r = list[0];
  if (!r) return [];
  const ids = list.map(x => x.id), busy = !!S.busy, q = U.search;
  if (q.id !== r.id) { Object.assign(q, { id: r.id, kind: r.kind === 'music' ? 'movie' : r.kind }); for (const k of ['q', 'nm', 'se', 'ep']) delete U.draft[k]; }
  const ok = list.some(x => ['pruefen', 'lokal', 'laeuft'].includes(x.status)), later = list.every(x => x.status === 'laeuft');
  const search = () => {
    const text = U.draft.q ?? (r.query || r.title), titles = new Set(list.map(x => x.query)).size;
    const run = () => { closeSheet(); act('search', ids, text, q.kind); };
    if (titles < 2) return run();
    sheet(() => [h('h2', { id: 'sheet-title' }, 'Wirklich alle zusammen?'), h('p', null, `Markiert sind ${titles} verschiedene Titel. Die Suche „${text}“ würde für alle ${list.length} Dateien gelten.`),
      h('.acts', null, btn('Zurück', closeSheet), btn('Für alle suchen', run, { cls: '.blue' }))]);
  };
  const number = () => {
    const s = parseInt(U.draft.se ?? r.season, 10), e = parseInt(U.draft.ep ?? r.episode, 10);
    if (s >= 0 && e >= 0) act('set_episode', ids, s, e).then(() => { delete U.draft.se; delete U.draft.ep; }); else toast('Bitte Staffel und Folge als Zahl eintragen.');
  };
  return h('.insp', { 'data-keep': 'insp', role: 'complementary', 'aria-label': 'Details' }, art(r.poster, r.title),
    h('div', null, h('h2', null, r.title), h('.sub', null, list.length > 1 ? `${list.length} Dateien markiert` : [r.year, { tv: 'Serie', music: 'Musik' }[r.kind] || 'Film', r.folder && r.folder !== r.title ? r.folder : null].filter(Boolean).join('  ·  ')),
      r.sources.length ? h('.chips', null, r.sources.map(s => h('span.chip', null, srcName(s)))) : null),
    r.overview ? h('.txt', { title: r.overview }, r.overview) : null,
    r.note ? h('.note', null, r.note) : null,
    ok ? btn(later ? 'Trotzdem verwenden' : 'Treffer bestätigen', () => act('approve', ids), { cls: '.blue.wide', off: busy }) : null,
    r.cands.length ? h('div', null, h('.lab', null, 'Treffer'), h('.opts', { 'data-keep': 'opts' }, r.cands.map((c, i) =>
      h('button' + (i === r.pick ? '.on' : ''), { disabled: busy, 'data-focus': 'c' + i, onclick: () => act('pick', ids, i) }, h('span', null, c.label), h('em', null, `${c.score} %`))))) : null,
    r.kind === 'music' ? null : h('div', null, h('.lab', null, 'Anderen Titel suchen'),
      h('.find', { style: 'margin-bottom:8px' }, icon('find'), input('q', r.query || r.title, search, { label: 'Titel suchen', enterOnly: true })),
      h('.pair', null, seg([['tv', 'Serie'], ['movie', 'Film']], q.kind, k => { q.kind = k; render(); }), btn('Suchen', search, { off: busy }))),
    r.kind === 'tv' ? h('div', null, h('.lab', null, list.length > 1 ? 'Staffel und erste Folge' : 'Staffel und Folge'),
      h('.pair', null, input('se', r.season ?? '', number, { label: 'Staffel', hint: 'Staffel', mode: 'numeric', enterOnly: true }), input('ep', r.episode ?? '', number, { label: 'Folge', hint: 'Folge', mode: 'numeric', enterOnly: true }), btn('Setzen', number, { off: busy })),
      list.length > 1 ? h('.foot', { style: 'margin:6px 2px 0' }, 'Die markierten Dateien werden der Reihe nach weitergezählt.') : null) : null,
    list.length === 1 && r.checkable ? h('div', null, h('.lab', null, 'Zielname'),
      h('.pair', null, input('nm', r.name, v => act('rename', r.id, v).then(() => { delete U.draft.nm; render(); }), { label: 'Zielname', enterOnly: true }), btn('Ändern', () => act('rename', r.id, U.draft.nm ?? r.name).then(() => { delete U.draft.nm; render(); }), { off: busy, key: 'nm' }))) : null,
    h('.pair', null, btn('Ordner zeigen', () => call('reveal', r.id), { cls: '.plain' }), btn('Entfernen', () => { sel.clear(); act('remove', ids); }, { cls: '.plain', off: busy, tip: 'Nur aus der Liste, die Datei bleibt' })));
}
function drawPop() {
  const layer = document.getElementById('layer');
  layer.querySelectorAll('.pop,.veil').forEach(e => e.remove());
  if (!pop || page !== 'transfer') { pop = null; return; }
  const e = h('.pop', { role: 'dialog', 'aria-label': 'Datenquellen' }, S.sources.map(s => h('.it', null, h('.grow', null, s.name, h('small', null, s.ready ? (s.movies ? 'Filme und Serien' : 'Serien') : 'Braucht einen eigenen Schlüssel')),
    s.ready ? toggle(s.active, v => act('toggle_source', s.id, v), s.name, !!S.busy) : btn('Einrichten', () => { pop = null; go('settings'); }, { cls: '.plain', key: s.id }))));
  e.style.top = pop.bottom + 8 + 'px';
  e.style.left = Math.max(12, Math.min(pop.left, innerWidth - 330)) + 'px';
  layer.append(h('.veil', { onclick: () => { pop = null; drawPop(); } }), e);
}

/* ---------------------------------------------------------------- Dialog */
let sheetFn = null, sheetEl = null;
function sheet(fn, cls = '') {
  sheetFn = fn;
  if (!sheetEl) {
    sheetEl = h('dialog.sheet' + cls, { 'aria-labelledby': 'sheet-title' });
    sheetEl.addEventListener('close', () => { sheetEl.remove(); sheetEl = sheetFn = null; });
    sheetEl.addEventListener('click', e => { if (e.target === sheetEl) closeSheet(); });
    document.body.append(sheetEl);
    drawSheet();
    sheetEl.showModal();
  } else drawSheet();
}
function closeSheet() { if (sheetEl) sheetEl.close(); }
function drawSheet() { if (sheetEl) keep(sheetEl, () => sheetEl.replaceChildren(h('.in', null, sheetFn()))); }

/* ---------------------------------------------------------------- Episodenlisten */
async function searchShows() {
  U.epsNote = 'Suche …'; U.shows = []; U.eps = null; U.show = -1; render();
  const r = await call('episodes_search', U.draft.ep0 ?? U.epsText);
  U.shows = r.shows; U.epsNote = r.shows.length ? '' : 'Nichts gefunden. ' + r.note;
  render();
  if (r.shows.length) loadShow(0);
}
async function loadShow(i) {
  U.show = i; U.eps = null; U.epsNote = 'Lade Folgen …'; render();
  const e = await call('episodes_load', i);
  if (U.show !== i) return;
  U.eps = e; U.epsNote = e && e.rows.length ? '' : 'Keine der aktiven Quellen liefert hier eine Episodenliste.';
  render();
}
const viewEpisodes = () => h('.page', null,
  h('.head', null, h('h1', null, 'Episodenlisten'), U.eps && U.eps.rows.length ? btn('Liste kopieren', () => {
    navigator.clipboard.writeText(U.eps.rows.map(r => `${code(r.s, r.e)} - ${r.t.find(Boolean) || ''}`).join('\n'));
    toast(`${U.eps.rows.length} Folgen kopiert`);
  }) : null),
  h('.tools', null, h('.find', null, icon('find'), input('ep0', U.epsText, searchShows, { hint: 'Serie suchen', enterOnly: true })), btn('Suchen', searchShows, { cls: '.blue' })),
  U.shows.length ? h('.shows', { 'data-keep': 'shows' }, U.shows.map((s, i) => h('button.show' + (i === U.show ? '.on' : ''), { onclick: () => loadShow(i), title: s.title, 'data-focus': 'w' + i }, art(s.poster, s.title), h('b', null, s.title), h('span', null, String(s.year || ''))))) : null,
  h('.scroll', { 'data-keep': 'eps', style: 'padding-bottom:28px' }, U.eps && U.eps.rows.length ? h('.card', null, h('table', null,
    h('thead', null, h('tr', null, h('th', null, 'Folge'), U.eps.cols.map(c => h('th', null, c)))),
    h('tbody', null, U.eps.rows.map(r => h('tr', null, h('td.c', null, code(r.s, r.e)), r.t.map(t => h('td', null, t || '–'))))))) : h('.blank', null, U.epsNote || 'Vergleiche die Folgen aller aktiven Quellen nebeneinander.')));

/* ---------------------------------------------------------------- Untertitel */
const pullSubs = list => { U.subs = list || []; if (page === 'subs') render(); };
async function subDo(i, method, ...args) {
  U.subBusy = i; render();
  const list = await call(method, i, ...args);
  U.subBusy = -1; pullSubs(list);
}
const viewSubs = () => h('.page', null,
  h('.head', null, h('h1', null, 'Untertitel'), U.subs.length ? btn('', () => call('subs_clear').then(() => pullSubs([])), { icon: 'trash', cls: '.icon', tip: 'Liste leeren' }) : null,
    btn('Letzte Übertragungen', () => call('subs_recent').then(() => call('subs')).then(pullSubs), { icon: 'clock' }),
    btn('Videos wählen', () => call('subs_add').then(() => call('subs')).then(pullSubs), { icon: 'plus' })),
  S.subKey ? h('.tools', null, seg([['de', 'Deutsch'], ['en', 'Englisch'], ['fr', 'Französisch'], ['es', 'Spanisch']], U.subLang, k => { U.subLang = k; render(); }))
    : h('.tools', null, h('.note.grow', null, 'Untertitel kommen von OpenSubtitles.com und brauchen deinen eigenen kostenlosen Schlüssel.'), btn('Einrichten', () => go('settings'), { cls: '.blue' })),
  h('.scroll', { 'data-keep': 'subs', style: 'padding-bottom:28px' }, U.subs.length ? U.subs.map((s, i) => h('.group', null,
    h('.row.film', { title: s.path }, h('.grow', null, h('.n', null, s.file), h('.o', null, s.note || (s.have.length ? 'Daneben: ' + s.have.join(', ') : 'Noch keine Untertitel'))),
      btn(U.subBusy === i ? 'Suche …' : 'Suchen', () => subDo(i, 'subs_search', U.subLang), { off: !S.subKey || U.subBusy >= 0, key: i })),
    s.hits.map(x => h('.row', null, h('.grow', null, h('.n', null, x.name), h('.o', null, `${x.exact ? 'Passt genau zu dieser Datei  ·  ' : ''}${x.count.toLocaleString('de')} Downloads`)),
      btn('Laden', () => subDo(i, 'subs_load', x.id, U.subLang), { cls: '.plain', off: U.subBusy >= 0, key: i + '-' + x.id }))))) : h('.blank', null, 'Wähle Videos oder übernimm die letzten Übertragungen.')));

/* ---------------------------------------------------------------- Prüfsummen */
async function pullSums() {
  U.sums = await call('sums') || [];
  if (page === 'sums') { render(); if (U.sums.some(s => s.result === 'rechnet')) setTimeout(pullSums, 500); }
}
const viewSums = () => h('.page', null,
  h('.head', null, h('h1', null, 'Prüfsummen'), U.sums.length ? btn('', () => call('sums_clear').then(pullSums), { icon: 'trash', cls: '.icon', tip: 'Liste leeren' }) : null,
    btn('Dateien wählen', () => call('sums_add').then(pullSums), { icon: 'plus' })),
  h('p.lead', null, 'Berechnet CRC32 und SHA-256. Eine .sfv- oder .sha256-Datei wird gegen die Dateien daneben geprüft.'),
  h('.scroll', { 'data-keep': 'sums', style: 'padding-bottom:28px' }, U.sums.length ? h('.card', null, h('table', null,
    h('thead', null, h('tr', null, ['Datei', 'CRC32', 'SHA-256', 'Ergebnis'].map(t => h('th', null, t)))),
    h('tbody', null, U.sums.map(s => h('tr', null, h('td', null, s.file), h('td.h', null, s.crc || '…'), h('td.h', null, s.sha || '…'), h('td', null, pill(s.result))))))) : h('.blank', null, 'Noch keine Dateien.')));

/* ---------------------------------------------------------------- Verlauf */
const ACTION = { copy: 'Kopiert', move: 'Verschoben', undo: 'Rückgängig', untertitel: 'Untertitel' };
const viewHistory = () => h('.page', null, h('.head', null, h('h1', null, 'Verlauf')),
  h('p.lead', null, 'Rückgängig geht, solange die Datei in Plex unverändert ist.'),
  h('.scroll', { 'data-keep': 'hist' }, U.hist.length ? h('.card', null, h('table', null,
    h('thead', null, h('tr', null, ['Datei', 'Aktion', 'Zeit', 'Größe', ''].map(t => h('th', null, t)))),
    h('tbody', null, U.hist.map(e => h('tr', null, h('td', null, e.name, h('small', null, e.ziel)), h('td.c', null, ACTION[e.aktion] || e.aktion),
      h('td.c', null, e.zeit.slice(8, 10) + '.' + e.zeit.slice(5, 7) + '. ' + e.zeit.slice(11, 16)), h('td.c', null, gb(e.bytes)),
      h('td.r', null, e.aktion === 'undo' || e.undone ? null : btn('Rückgängig', () => sheet(() => [h('h2', { id: 'sheet-title' }, 'Übertragung rückgängig machen?'),
        h('p', null, `${e.name}\n\nDie Datei wird aus Plex entfernt. Fehlt das Original, wird es vorher geprüft zurückkopiert.`),
        h('.acts', null, btn('Zurück', closeSheet), btn('Rückgängig machen', () => { closeSheet(); act('undo', e.i); }, { cls: '.red' }))]), { cls: '.plain', off: !!S.busy, key: e.i }))))))) : h('.blank', null, 'Noch nichts übertragen.')),
  S.busy ? dock() : null);

/* ---------------------------------------------------------------- Einstellungen */
const save = (k, v) => act('set', k, v).then(render);
const number = (k, unit) => [input(k, S.cfg[k], async v => { const m = await call('set', k, v); if (m) toast(m); delete U.draft[k]; await poll(); render(); }, { label: k, mode: 'numeric', style: 'width:80px' }), h('span', { style: 'color:var(--mute);width:56px' }, unit)];
const keyLine = s => line(s.name, U.keyMsg[s.id] || (s.hasKey ? 'Eingerichtet' : 'Eigener Schlüssel nötig'),
  input('k' + s.id, '', () => saveKey(s), { type: 'password', hint: s.hasKey ? '••••••••' : 'Schlüssel', label: s.name + ' Schlüssel', style: 'width:170px', enterOnly: true }),
  btn('Sichern', () => saveKey(s), { key: s.id }), btn('Holen', () => call('key_page', s.id), { cls: '.plain', tip: 'Öffnet die Seite des Anbieters', key: s.id }));
async function saveKey(s) {
  const v = U.draft['k' + s.id] ?? '';
  U.keyMsg[s.id] = 'Teste …'; render();
  const m = await call('save_key', s.id, v);
  U.keyMsg[s.id] = m || (v ? 'Funktioniert und ist aktiv' : 'Schlüssel entfernt');
  if (!m) delete U.draft['k' + s.id];
  await poll(); render();
}
const viewSettings = () => h('.page', null, h('.head', null, h('h1', null, 'Einstellungen')),
  h('.scroll', { 'data-keep': 'set', style: 'padding-bottom:40px' }, h('.setwrap', null,
    h('h3', null, 'Plex-Bibliothek'),
    h('.set', null, pathLine('tv', 'Serien'), pathLine('movies', 'Filme'), pathLine('music', 'Musik'),
      line('Titelsprache', null, seg([['en', 'Englisch'], ['de', 'Deutsch'], ['orig', 'Original']], S.cfg.lang, k => save('lang', k)))),
    h('p.foot', null, 'Nicht Benötigtes einfach leer lassen. Vorhandene Ordner werden immer weiterbenutzt, egal in welcher Sprache sie heißen.'),
    h('h3', null, 'Quellordner'),
    h('.set', null, S.cfg.folders.map(f => line(f, null, btn('Entfernen', () => act('remove_watch', f).then(render), { cls: '.plain', key: f, tip: 'Nur aus dieser Liste, der Ordner bleibt' }))),
      line('Ordner überwachen', 'Neue Videos erscheinen automatisch in der Liste und werden online abgeglichen. Dateien ohne erkennbares Muster werden dabei nicht online gesucht.', toggle(S.cfg.watch, v => save('watch', v), 'Ordner überwachen')),
      line('Videos ab', 'Kleinere werden beim Einlesen von Ordnern übersprungen', number('min_size_mb', 'MB')),
      line('Ruhezeit', 'So lange muss eine Datei unverändert sein', number('min_age_minutes', 'Minuten')),
      h('.ln', null, btn('Ordner hinzufügen', () => act('add_watch').then(render), { cls: '.plain' }))),
    h('p.foot', null, 'Übertragen wird nie automatisch, nur nach deinem Klick.'),
    h('h3', null, 'Datenquellen'),
    h('.set', null, S.sources.map(s => s.needsKey ? keyLine(s) : line(s.name, s.movies ? 'Filme und Serien, sofort nutzbar' : 'Serien, sofort nutzbar', toggle(s.active, v => act('toggle_source', s.id, v), s.name, !!S.busy))),
      keyLine({ id: 'opensubtitles', name: 'OpenSubtitles', hasKey: S.subKey }),
      line('Filmplakate aus Wikipedia', 'Oft urheberrechtlich geschützt; nur zur Anzeige bei dir. Mit TheMovieDB-Schlüssel nicht nötig.', toggle(S.cfg.wiki_posters, v => save('wiki_posters', v), 'Filmplakate aus Wikipedia'))),
    h('p.foot', null, 'Schlüssel werden mit deinem Windows-Konto verschlüsselt gespeichert. TheTVDB mit Abo: „Schlüssel:PIN“.'),
    h('h3', null, 'Namensvorlagen'),
    h('.set', null, [['format_tv', 'Serien'], ['format_movie', 'Filme']].map(([k, t]) => line(t, U.fmtMsg[k] || S.preview[k], input(k, S.cfg[k], async v => {
      U.fmtMsg[k] = v === S.cfg[k] ? '' : await call('set', k, v);
      if (!U.fmtMsg[k]) delete U.draft[k];
      await poll(); render();
    }, { label: t })))),
    h('p.foot', null, '{plex} ist der Plex-Standard. Eigene Vorlagen aus {n} Titel, {y} Jahr, {s00e00}, {sxe}, {s}, {e}, {t} Folgentitel und {folder}. Ein Schrägstrich trennt Ordner.'))));

/* ---------------------------------------------------------------- Über Spool */
const CREDITS = [
  ['tmdb', 'TheMovieDB', 'This product uses the TMDB API but is not endorsed or certified by TMDB.'],
  ['tvdb', 'TheTVDB', 'Metadata provided by TheTVDB. Please consider adding missing information or subscribing.'],
  ['tvmaze', 'TVmaze', 'Serien- und Episodendaten, Lizenz CC BY-SA.'],
  ['wikidata', 'Wikidata', 'Titel, Jahre und Kennungen, Lizenz CC0.'],
  ['wikipedia', 'Wikipedia', 'Plakatbilder, falls eingeschaltet. Rechte bei den jeweiligen Inhabern.'],
  ['anilist', 'AniList', 'Anime-Daten.'], ['omdb', 'OMDb API', 'Film- und Seriendaten, Lizenz CC BY-NC 4.0.'], ['opensubtitles', 'OpenSubtitles.com', 'Untertitel.'],
];
const viewAbout = () => h('.page', null, h('.head', null, h('h1', null, 'Über Spool')),
  h('.scroll', { 'data-keep': 'about', style: 'padding-bottom:40px' }, h('.setwrap', null,
    h('.set', null, line('Version', null, S.version), line('Lizenzen der verwendeten Bausteine', 'THIRD-PARTY.txt im Programmordner')),
    h('.set', { style: 'margin-top:14px' }, line('Spool unterstützen', 'Spool ist kostenlos und bleibt es. Wenn es dir hilft, freue ich mich über einen Kaffee.', btn('Kaffee auf Ko-fi', () => call('link', 'kofi'), { cls: '.blue' })),
      line('Quelltext und Fehler melden', 'github.com/azarucha/spool', btn('Öffnen', () => call('link', 'code'), { key: 'code' }))),
    h('p.foot', null, 'Spool steht in keiner Verbindung zu Plex. „Plex“ ist eine Marke von Plex, Inc.'),
    h('h3', null, 'Was das Programm nach außen gibt'),
    h('.set', null, line('An die Datenquellen', 'Den Titel, den Spool aus Datei- oder Ordnernamen erkennt, dazu Jahr, Staffel und Folge. Hat eine Datei kein erkennbares Muster, ist das der ganze Dateiname ohne Endung. Vollständige Pfade nie.'),
      line('An OpenSubtitles', 'Zusätzlich Dateigröße und ein Prüfwert aus Anfang und Ende des Videos, nur wenn du suchst.'),
      line('Bilder', 'Plakate werden direkt beim jeweiligen Anbieter geladen; er sieht dabei wie bei jeder Webseite deine IP-Adresse.'),
      line('Sonst nichts', 'Keine Nutzungsdaten, kein Konto, keine Werbung.')),
    h('h3', null, 'Datenquellen'),
    h('.set', null, CREDITS.map(([id, n, t]) => line(n, t, btn('Webseite', () => call('link', id), { cls: '.plain', key: id })))),
    h('h3', null, 'Gelernte Zuordnungen'),
    h('.set', null, U.aliases.length ? U.aliases.map(a => line(a.key, '→ ' + a.folder + (a.sure ? '' : '  ·  wird beim nächsten Mal nachgefragt'), btn('Vergessen', () => act('alias_delete', a.key).then(() => go('about')), { cls: '.plain', key: a.key }))) : line('Noch keine', 'Spool merkt sich einen Titel, wenn du einen Treffer ausdrücklich bestätigst.')),
    h('h3', null, 'Aufräumen'),
    h('.set', null, line('Angefangene Kopien', U.partials === null ? 'Reste abgebrochener Übertragungen in deiner Bibliothek' : U.partials.length ? null : 'Keine gefunden', btn('Suchen', async () => { U.partials = await call('partials') || []; render(); }, { key: 'part' })),
      (U.partials || []).map(p => line(p.path, gb(p.bytes), btn('Löschen', async () => { const m = await call('partial_delete', p.path); if (m) toast(m); U.partials = await call('partials') || []; render(); }, { cls: '.plain', off: !!S.busy, key: p.path }))),
      line('Zwischenspeicher', 'Antworten der Datenquellen, werden bei Bedarf neu geladen', btn('Leeren', () => call('cache_clear').then(() => toast('Geleert')), { key: 'cache' }))))));

const VIEW = { transfer: viewTransfer, episodes: viewEpisodes, subs: viewSubs, sums: viewSums, history: viewHistory, settings: viewSettings, about: viewAbout };

/* ---------------------------------------------------------------- Takt */
let polling = null;
function poll() {
  polling = polling || (async () => {
    try {
      const n = await call('state', S ? S.v : 0, S ? S.rv : 0);
      if (!n) return;
      if (n.rows) {
        for (const r of n.rows) { r.sig = JSON.stringify(r); byId.set(r.id, r); }
        if (n.ids) { const live = new Set(n.ids); for (const i of [...byId.keys()]) if (!live.has(i)) byId.delete(i); }
        rows = [...byId.values()].sort((a, b) => a.id - b.id);
        sel = new Set([...sel].filter(i => byId.has(i)));
      }
      const rowsChanged = n.rows && (n.rows.length || n.ids);
      S = n;
      if (n.info && n.info !== lastInfo && !n.busy) toast(n.info);
      lastInfo = n.info;
      const finished = wasBusy && !n.busy;
      const same = wasBusy === n.busy;
      wasBusy = n.busy;
      if (finished && page === 'history') call('history').then(r => { U.hist = r || []; render(); });
      const d = document.querySelector('.dock');
      if (same && n.busy && !rowsChanged && d) keep(d.parentNode, () => d.replaceWith(dock()));
      else render();
    } finally { polling = null; }
  })();
  return polling;
}
document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && !sheetEl) { if (pop) { pop = null; drawPop(); } else if (sel.size) { sel.clear(); showSelection(); } }
  if (e.target.tagName === 'INPUT' || sheetEl || page !== 'transfer' || !rows.length) return;
  if (e.ctrlKey && e.key === 'a') { e.preventDefault(); sel = new Set(shown().map(r => r.id)); showSelection(); }
  if (e.key === ' ' && sel.size && e.target.tagName !== 'BUTTON') { e.preventDefault(); const l = picked(); check(l, !l.every(r => r.checked || !r.checkable)); }
  if (e.key === 'Delete' && sel.size && !S.busy) { const ids = [...sel]; sel.clear(); act('remove', ids); }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const ids = visibleIds(), i = ids.indexOf(anchor), n = ids[Math.max(0, Math.min(ids.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))];
    if (n == null) return;
    if (e.shiftKey) sel.add(n); else sel = new Set([n]);
    anchor = n;
    showSelection();
    const el = document.querySelector(`.row[data-id="${n}"]`);
    if (el) { el.focus({ preventScroll: true }); el.scrollIntoView({ block: 'nearest' }); }
  }
});
document.addEventListener('contextmenu', e => { if (e.target.tagName !== 'INPUT') e.preventDefault(); });
if (native) {
  const over = on => () => { document.body.classList.toggle('over', on); if (on && page !== 'transfer') go('transfer'); };
  native.event.listen('tauri://drag-enter', over(true));
  native.event.listen('tauri://drag-leave', over(false));
  native.event.listen('tauri://drag-drop', over(false));
}
poll();
setInterval(poll, 300);
