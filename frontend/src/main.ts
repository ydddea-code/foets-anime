// Fets Anime — frontend entry point.
// Vanilla TS + Vite. All backend access goes through the typed IPC bridge;
// the UI never touches the filesystem or shell directly.
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import api, { coverSrc, fmtTime, fmtSize, timeAgo } from "./lib/ipc";
import type {
  AnimeSummary, EpisodeInfo, HistoryEntry, PlayerStatus, TrackInfo, Settings, AppInfo,
} from "./lib/ipc";
import { toast, modal, contextMenu, escapeHtml } from "./lib/ui";
import { renderGrid, type CardActions } from "./components/AnimeCard";
import { initScanBanner } from "./components/ScanBanner";
import "./styles/app.css";

/* ------------------------------------------------------------------ state */

type Page = "home" | "library" | "recent" | "continue" | "settings" | "detail" | "player";

const S = {
  page: "home" as Page,
  detailId: 0,
  search: "",
  sort: "name",
  settings: null as Settings | null,
  appInfo: null as AppInfo | null,
  playerEpisodes: [] as EpisodeInfo[],
  playerIndex: -1,
  playerPath: "",
  playerStatus: { idle: true, paused: true, position: 0, duration: 0, volume: 100, muted: false, speed: 1, filename: "" } as PlayerStatus,
  tracks: [] as TrackInfo[],
  controlsTimer: 0,
  posTimer: 0,
  unsubs: [] as UnlistenFn[],
};

const NAV: { id: Page; label: string; icon: string }[] = [
  { id: "home", label: "Dashboard", icon: "⌂" },
  { id: "library", label: "Library", icon: "▦" },
  { id: "recent", label: "Recently Watched", icon: "◷" },
  { id: "continue", label: "Continue Watching", icon: "▶" },
  { id: "settings", label: "Settings", icon: "⚙" },
];

/* ----------------------------------------------------------------- layout */

function buildSidebar() {
  const sb = document.getElementById("sidebar")!;
  sb.innerHTML = `
    <div class="brand"><span class="logo">F</span><div><b>Fets Anime</b><small>local player</small></div></div>
    <nav>${NAV.map((n) => `<button class="nav-item" data-page="${n.id}"><span class="ico">${n.icon}</span>${n.label}</button>`).join("")}</nav>
    <div class="sb-foot"><span id="mpv-badge" class="badge">mpv: …</span></div>`;
  sb.querySelectorAll(".nav-item").forEach((b) =>
    b.addEventListener("click", () => go((b as HTMLElement).dataset.page as Page))
  );
}

function setActiveNav(page: Page) {
  document.querySelectorAll(".nav-item").forEach((b) => {
    b.classList.toggle("active", (b as HTMLElement).dataset.page === page);
  });
}

async function refreshBadge() {
  const el = document.getElementById("mpv-badge");
  if (!el || !S.appInfo) return;
  el.textContent = S.appInfo.mpvEmbedded ? "mpv: embedded" : "mpv: DLL belum ada";
  el.classList.toggle("ok", S.appInfo.mpvEmbedded);
}

function main(): HTMLElement {
  return document.getElementById("main")!;
}

async function go(page: Page, arg?: number) {
  // leaving player: stop rendering so no orphan mpv surface stays behind
  if (S.page === "player" && page !== "player") teardownPlayerUi();
  S.page = page;
  setActiveNav(page === "detail" || page === "player" ? "library" : page);
  const m = main();
  m.innerHTML = "";
  switch (page) {
    case "home": await renderHome(m); break;
    case "library": await renderLibrary(m); break;
    case "detail": await renderDetail(m, arg ?? S.detailId); break;
    case "recent": await renderHistory(m, "Recently Watched", api.recentlyWatched); break;
    case "continue": await renderHistory(m, "Continue Watching", api.continueWatching); break;
    case "settings": await renderSettings(m); break;
    case "player": await renderPlayer(m, arg ?? 0); break;
  }
}

/* ------------------------------------------------------------------- home */

async function renderHome(m: HTMLElement) {
  const [anime, cont, recent] = await Promise.all([
    api.listAnime("", "lastwatched").catch(() => []),
    api.continueWatching().catch(() => []),
    api.recentlyWatched().catch(() => []),
  ]);
  m.innerHTML = `
    <header class="page-head"><h1>Dashboard</h1>
      <div class="head-actions">
        <button class="btn primary" id="hb-add">+ Add Anime</button>
        <button class="btn" id="hb-scan">Scan Library</button>
      </div></header>
    ${anime.length === 0 ? emptyState() : `
    <section><h2>Lanjutkan menonton</h2>
      ${cont.length === 0 ? `<p class="muted">Belum ada riwayat.</p>` : historyRow(cont)}</section>
    <section><h2>Baru ditonton</h2>
      ${recent.length === 0 ? `<p class="muted">Belum ada riwayat.</p>` : historyRow(recent)}</section>
    <section><h2>Anime terbaru di library</h2><div class="grid" id="home-grid"></div></section>`}`;
  if (anime.length > 0) {
    renderGrid(m.querySelector("#home-grid")!, anime.slice(0, 12), cardActions());
    bindHeaderButtons();
  } else {
    bindEmptyAdd(m);
  }
}

function historyRow(list: HistoryEntry[]): string {
  return `<div class="hrow">${list.slice(0, 8)
    .map(
      (h) => `<div class="hcard" data-vid="${h.videoId}" data-path="${escapeHtml(h.path)}" data-pos="${h.positionSecs}">
        <div class="hcover">${h.coverUrl ? `<img loading="lazy" src="${coverSrc(h) ?? ""}" alt="" onerror="this.remove()"/>` : `<span>${escapeHtml(h.animeName[0] ?? "?")}</span>`}</div>
        <div class="hmeta"><b>${escapeHtml(h.animeName)}</b><small>${escapeHtml(h.fileName)}</small>
        <small class="muted">${fmtTime(h.positionSecs)}${h.durationSecs ? " / " + fmtTime(h.durationSecs) : ""} · ${timeAgo(h.watchedAt)}</small></div>
      </div>`
    )
    .join("")}</div>`;
}

function bindHistoryRow(root: HTMLElement, openIt: (path: string, vid: number, pos: number) => void) {
  root.querySelectorAll<HTMLElement>(".hcard").forEach((c) =>
    c.addEventListener("click", () =>
      openIt(c.dataset.path!, Number(c.dataset.vid || 0), Number(c.dataset.pos || 0))
    )
  );
}

function emptyState(): string {
  return `<div class="empty">
    <div class="empty-ico">▦</div>
    <h2>Library masih kosong</h2>
    <p>Tambahkan folder anime dari laptop Anda. Menambahkan folder <b>tidak</b> memindai isi folder —<br/>
    jalankan pemindaian manual kapan saja lewat tombol Scan Library.</p>
    <button class="btn primary big" id="empty-add">+ Add Anime</button>
  </div>`;
}

function bindEmptyAdd(m: HTMLElement) {
  m.querySelector("#empty-add")?.addEventListener("click", addAnimeFlow);
}

/* --------------------------------------------------------------- library */

async function renderLibrary(m: HTMLElement) {
  m.innerHTML = `
    <header class="page-head"><h1>Library</h1>
      <div class="head-actions">
        <input class="txt search" id="q" placeholder="Cari anime…" value="${escapeHtml(S.search)}"/>
        <select class="sel" id="sort">
          <option value="name">Nama (A→Z)</option>
          <option value="episodes">Jumlah episode</option>
          <option value="lastwatched">Terakhir ditonton</option>
        </select>
        <button class="btn primary" id="lb-add">+ Add Anime</button>
        <button class="btn" id="lb-scan">Scan Library</button>
      </div></header>
    <div class="grid" id="grid"><div class="loading">Memuat…</div></div>`;
  (m.querySelector("#sort") as HTMLSelectElement).value = S.sort;
  bindHeaderButtons();
  const q = m.querySelector<HTMLInputElement>("#q")!;
  let deb = 0;
  q.addEventListener("input", () => {
    clearTimeout(deb);
    deb = window.setTimeout(async () => {
      S.search = q.value.trim();
      await loadGrid();
    }, 250);
  });
  m.querySelector("#sort")!.addEventListener("change", async (e) => {
    S.sort = (e.target as HTMLSelectElement).value;
    await loadGrid();
  });
  await loadGrid();
}

async function loadGrid() {
  const g = document.getElementById("grid");
  if (!g) return;
  try {
    const list = await api.listAnime(S.search, S.sort);
    if (list.length === 0) {
      g.innerHTML = S.search
        ? `<p class="muted nopad">Tidak ada anime yang cocok dengan "${escapeHtml(S.search)}".</p>`
        : emptyState();
      bindEmptyAdd(g as HTMLElement);
      return;
    }
    renderGrid(g, list, cardActions());
  } catch (e) {
    g.innerHTML = `<p class="err nopad">Gagal memuat library: ${escapeHtml(String(e))}</p>`;
  }
}

function bindHeaderButtons() {
  document.getElementById("hb-add")?.addEventListener("click", addAnimeFlow);
  document.getElementById("lb-add")?.addEventListener("click", addAnimeFlow);
  document.getElementById("hb-scan")?.addEventListener("click", scanFlow);
  document.getElementById("lb-scan")?.addEventListener("click", scanFlow);
}

function cardActions(): CardActions {
  return {
    open: (a) => go("detail", a.id),
    menu: (a, x, y) => animeContextMenu(a, x, y),
  };
}

/* ------------------------------------------------------- add-anime flow */

async function addAnimeFlow() {
  const dir = await open({ directory: true, multiple: false, title: "Pilih folder anime" });
  if (!dir || typeof dir !== "string") return;
  const r = await modal({
    title: "Tambah anime",
    bodyHtml: `<p>${escapeHtml(dir)}</p>
      <p class="muted">Folder ini berisi <b>satu anime</b>, atau <b>folder induk</b> dengan banyak subfolder anime?</p>`,
    confirmLabel: "Tambah sebagai 1 anime",
  });
  if (!r.confirmed) return;
  const asParent = await modal({
    title: "Folder induk?",
    bodyHtml: `<p>Jika folder tadi adalah <b>folder induk</b> (mis. <i>D:\\Anime</i> dengan subfolder per judul),
      setiap subfolder langsung akan ditambahkan sebagai entri terpisah.<br/><br/>
      Tambahkan sebagai folder induk?</p>`,
    confirmLabel: "Ya, sebagai folder induk",
  });
  try {
    const res = await api.addAnimeFolder(dir, asParent.confirmed);
    const created = res.added.filter((a) => a.created).length;
    const dup = res.added.length - created;
    toast(
      `${created} entri ditambahkan${dup ? `, ${dup} sudah ada (dilewati)` : ""}. ` +
        `Folder disimpan — tekan Scan Library untuk memindai kapan saja.`,
      "ok"
    );
    await loadGrid();
  } catch (e) {
    toast(String(e), "error");
  }
}

async function scanFlow() {
  try {
    await api.scanLibrary(null, false);
    toast("Pemindaian dimulai…", "");
  } catch (e) {
    toast(String(e), "error");
  }
}

/* ------------------------------------------------------------- context menu */

async function animeContextMenu(a: AnimeSummary, x: number, y: number) {
  contextMenu(x, y, [
    { label: "Buka detail", action: () => go("detail", a.id) },
    { label: "Scan folder ini", action: async () => { try { await api.scanLibrary([a.id], false); } catch (e) { toast(String(e), "error"); } } },
    { label: "Ganti cover…", action: () => changeCoverFlow(a.id) },
    { label: "Reset cover", action: () => run(() => api.resetCover(a.id)) },
    { label: "Hapus cover khusus", action: () => run(() => api.removeCustomCover(a.id)) },
    { label: "Ubah nama tampilan…", action: () => renameFlow(a) },
    { label: "Remove from Library", danger: true, action: () => removeFlow(a) },
    { label: "Delete Folder from Disk…", danger: true, action: () => deleteDiskFlow(a) },
  ]);
}

async function run(fn: () => Promise<any>) {
  try { await fn(); toast("Tersimpan.", "ok"); await loadGrid(); } catch (e) { toast(String(e), "error"); }
}

async function changeCoverFlow(animeId: number) {
  const img = await open({
    multiple: false,
    title: "Pilih gambar cover",
    filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp"] }],
  });
  if (!img || typeof img !== "string") return;
  try {
    await api.changeCover(animeId, img);
    toast("Cover diperbarui.", "ok");
    await loadGrid();
    if (S.page === "detail") await go("detail", animeId);
  } catch (e) { toast(String(e), "error"); }
}

async function renameFlow(a: AnimeSummary) {
  const r = await modal({ title: "Ubah nama tampilan", bodyHtml: "<p>Nama baru:</p>", input: true, value: a.displayName, confirmLabel: "Simpan" });
  if (!r.confirmed || !r.value?.trim()) return;
  await run(() => api.renameAnime(a.id, r.value!.trim()));
}

async function removeFlow(a: AnimeSummary) {
  const r = await modal({
    title: "Remove from Library",
    bodyHtml: `<p>Hapus <b>${escapeHtml(a.displayName)}</b> dari aplikasi?</p>
      <p class="muted">File video asli di <code>${escapeHtml(a.folderPath)}</code> <b>tidak akan dihapus</b>.</p>`,
    confirmLabel: "Remove", danger: true,
  });
  if (!r.confirmed) return;
  await run(() => api.removeFromLibrary(a.id, true));
}

async function deleteDiskFlow(a: AnimeSummary) {
  const PATH = "HAPUS-FOLDER";
  const r = await modal({
    title: "Delete Folder from Disk",
    bodyHtml: `<p class="danger-text">Berbahaya! Semua file di dalam folder berikut akan <b>dihapus permanen</b>:</p>
      <p><code>${escapeHtml(a.folderPath)}</code></p>
      <p>Ketik <b>${PATH}</b> untuk konfirmasi:</p>`,
    input: true, requireMatch: PATH, confirmLabel: "Hapus permanen", danger: true,
  });
  if (!r.confirmed) return;
  try {
    await api.deleteFolderFromDisk(a.id, a.folderPath);
    toast("Folder terhapus dari disk dan entri library dibersihkan.", "ok");
    await loadGrid();
  } catch (e) { toast(String(e), "error"); }
}

/* ---------------------------------------------------------------- detail */

async function renderDetail(m: HTMLElement, id: number) {
  S.detailId = id;
  let a: AnimeSummary;
  try { a = await api.getAnime(id); } catch (e) { m.innerHTML = `<p class="err nopad">${escapeHtml(String(e))}</p>`; return; }
  const eps: EpisodeInfo[] = await api.listEpisodes(id).catch(() => [] as EpisodeInfo[]);
  m.innerHTML = `
    <header class="page-head">
      <button class="btn back" id="back">← Library</button>
      <h1>${escapeHtml(a.displayName)}</h1>
      <div class="head-actions">
        <button class="btn" id="d-cover">Change Cover</button>
        <button class="btn" id="d-rename">Rename</button>
        <button class="btn" id="d-scan">Scan ulang</button>
      </div>
    </header>
    ${a.status === "missing" ? `<div class="warn">⚠ Folder sumber tidak ditemukan: <code>${escapeHtml(a.folderPath)}</code></div>` : ""}
    <div class="detail-top">
      <div class="bigcover">${a.coverUrl ? `<img src="${coverSrc(a) ?? ""}" alt="" onerror="this.parentNode.classList.add('broken')"/>` : `<span>${escapeHtml(a.displayName[0] ?? "?")}</span>`}</div>
      <div class="dmeta">
        <p><b>${eps.length}</b> episode ditemukan</p>
        <p class="muted mono">${escapeHtml(a.folderPath)}</p>
        <p class="muted">Ditambahkan terakhir ditonton: ${timeAgo(a.lastWatchedAt) || "belum pernah"}</p>
        <div class="row">
          <button class="btn small" id="d-reset">Reset Cover</button>
          <button class="btn small" id="d-rmcov">Remove Custom Cover</button>
          <button class="btn small danger" id="d-remove">Remove from Library</button>
        </div>
      </div>
    </div>
    <h2>Episode</h2>
    ${eps.length === 0
      ? `<p class="muted">Belum ada video. Tekan “Scan ulang” untuk memindai folder ini.</p>`
      : `<table class="tbl"><thead><tr><th>Nama file</th><th>Durasi</th><th>Resolusi</th><th>Ukuran</th><th>Progres</th><th></th></tr></thead>
         <tbody>${eps.map((e) => `<tr data-i="${eps.indexOf(e)}">
            <td class="mono">${escapeHtml(e.relPath || e.fileName)}</td>
            <td>${fmtTime(e.durationSecs)}</td>
            <td>${e.width && e.height ? e.width + "×" + e.height : "-"}</td>
            <td>${fmtSize(e.size)}</td>
            <td>${e.completed ? "✓ selesai" : e.positionSecs ? fmtTime(e.positionSecs) : "belum"}</td>
            <td><button class="btn small primary play">▶ Tonton</button></td></tr>`).join("")}</tbody></table>`}
  `;
  m.querySelector("#back")!.addEventListener("click", () => go("library"));
  m.querySelector("#d-cover")!.addEventListener("click", () => changeCoverFlow(id));
  m.querySelector("#d-rename")!.addEventListener("click", () => renameFlow(a));
  m.querySelector("#d-reset")!.addEventListener("click", () => run(() => api.resetCover(id)));
  m.querySelector("#d-rmcov")!.addEventListener("click", () => api.removeCustomCover(id).then(() => go("detail", id)).catch((e) => toast(String(e), "error")));
  m.querySelector("#d-remove")!.addEventListener("click", () => removeFlow(a).then(() => { if (S.page === "detail") go("library"); }));
  m.querySelector("#d-scan")!.addEventListener("click", async () => {
    try { await api.scanLibrary([id], false); } catch (e) { toast(String(e), "error"); }
  });
  m.querySelectorAll<HTMLTableRowElement>(".tbl tbody tr").forEach((tr) => {
    const e = eps[Number(tr.dataset.i)];
    const play = () => startPlayback(e.path, e.id, e.positionSecs ?? 0, eps as EpisodeInfo[], eps.indexOf(e));
    tr.addEventListener("dblclick", play);
    tr.querySelector(".play")!.addEventListener("click", (ev) => { ev.stopPropagation(); play(); });
  });
}

/* --------------------------------------------------------------- history */

async function renderHistory(m: HTMLElement, title: string, fetcher: () => Promise<HistoryEntry[]>) {
  m.innerHTML = `<header class="page-head"><h1>${title}</h1>
    <div class="head-actions"><button class="btn danger" id="clear-hist">Clear History</button></div></header>
    <div id="hist"><div class="loading">Memuat…</div></div>`;
  m.querySelector("#clear-hist")!.addEventListener("click", async () => {
    const r = await modal({ title: "Clear History", bodyHtml: "<p>Hapus seluruh riwayat tontonan?</p>", danger: true, confirmLabel: "Hapus" });
    if (!r.confirmed) return;
    await api.clearHistory().catch((e) => toast(String(e), "error"));
    await go(S.page);
  });
  try {
    const list = await fetcher();
    const box = m.querySelector("#hist")!;
    box.innerHTML = list.length
      ? historyRow(list)
      : `<p class="muted nopad">Belum ada riwayat tontonan.</p>`;
    bindHistoryRow(box as HTMLElement, (path, vid, pos) => startPlayback(path, vid, pos, [] as EpisodeInfo[], -1));
  } catch (e) {
    m.querySelector("#hist")!.innerHTML = `<p class="err nopad">${escapeHtml(String(e))}</p>`;
  }
}

/* -------------------------------------------------------------- settings */

async function renderSettings(m: HTMLElement) {
  const st = await api.getSettings();
  S.settings = st;
  const info = S.appInfo;
  m.innerHTML = `
    <header class="page-head"><h1>Settings</h1></header>
    <div class="settings">
      <section class="set-card"><h2>Tampilan</h2>
        <label class="set-row">Tema
          <select id="s-theme" class="sel"><option value="dark">Gelap</option><option value="light">Terang</option></select></label>
      </section>
      <section class="set-card"><h2>Pemutaran</h2>
        <label class="set-row">Volume default <input id="s-vol" type="number" min="0" max="130" value="${st.defaultVolume}" class="num"/></label>
        <label class="set-row">Kecepatan default <input id="s-speed" type="number" step="0.25" min="0.25" max="4" value="${st.defaultSpeed}" class="num"/></label>
        <label class="set-row">Bahasa subtitle preferensi <input id="s-sub" class="txt" placeholder="mis. id, en (kosongkan = auto)" value="${escapeHtml(st.subtitleLang)}"/></label>
        <label class="set-row">Bahasa audio preferensi <input id="s-aud" class="txt" placeholder="mis. jp, en" value="${escapeHtml(st.audioLang)}"/></label>
      </section>
      <section class="set-card"><h2>Riwayat & pemindaian</h2>
        <label class="set-row chk"><input id="s-hist" type="checkbox" ${st.historyEnabled ? "checked" : ""}/> Simpan riwayat tontonan</label>
        <label class="set-row chk"><input id="s-thumb" type="checkbox" ${st.autoThumbnail ? "checked" : ""}/> Buat thumbnail otomatis dari video (opsional, saat scan)</label>
        <div class="row">
          <button class="btn" id="s-forcerescan">Force Full Rescan</button>
        </div>
      </section>
      <section class="set-card"><h2>Aplikasi</h2>
        <table class="kv">
          <tr><td>Versi</td><td>${info?.version ?? "-"} (${info?.platform ?? "-"})</td></tr>
          <tr><td>Integrasi MPV</td><td>${info?.mpvEmbedded ? "libmpv tertanam (render API pada HWND)" : "libmpv-2.dll belum ditemukan — lihat MPV_SETUP.md"}</td></tr>
          <tr><td>Data folder</td><td class="mono">${escapeHtml(info?.dataDir ?? "-")}</td></tr>
        </table>
        <div class="row"><button class="btn" id="s-open">Buka folder data/log/cache</button></div>
      </section>
    </div>`;
  (m.querySelector("#s-theme") as HTMLSelectElement).value = st.theme;
  const save = async (k: string, v: string) => { try { await api.setSetting(k, v); } catch (e) { toast(String(e), "error"); } };
  m.querySelector("#s-theme")!.addEventListener("change", (e) => { applyTheme((e.target as HTMLSelectElement).value); save("theme", (e.target as HTMLSelectElement).value); });
  m.querySelector("#s-vol")!.addEventListener("change", (e) => save("default_volume", String((e.target as HTMLInputElement).valueAsNumber)));
  m.querySelector("#s-speed")!.addEventListener("change", (e) => save("default_speed", String((e.target as HTMLInputElement).valueAsNumber)));
  m.querySelector("#s-sub")!.addEventListener("change", (e) => save("subtitle_lang", (e.target as HTMLInputElement).value.trim()));
  m.querySelector("#s-aud")!.addEventListener("change", (e) => save("audio_lang", (e.target as HTMLInputElement).value.trim()));
  m.querySelector("#s-hist")!.addEventListener("change", (e) => save("history_enabled", String((e.target as HTMLInputElement).checked)));
  m.querySelector("#s-thumb")!.addEventListener("change", (e) => save("auto_thumbnail", String((e.target as HTMLInputElement).checked)));
  m.querySelector("#s-forcerescan")!.addEventListener("click", async () => {
    const r = await modal({ title: "Force full rescan", bodyHtml: "<p>Cache metadata dibersihkan dan semua video dianalisis ulang. Lanjutkan?</p>", confirmLabel: "Scan penuh" });
    if (!r.confirmed) return;
    try { await api.scanLibrary(null, true); } catch (e) { toast(String(e), "error"); }
  });
  m.querySelector("#s-open")!.addEventListener("click", () => api.openDataDir().catch((e) => toast(String(e), "error")));
}

function applyTheme(t: string) {
  document.documentElement.dataset.theme = t === "light" ? "light" : "dark";
}

/* ---------------------------------------------------------------- player */

async function startPlayback(path: string, videoId: number, resumePos: number, eps: EpisodeInfo[], idx: number) {
  S.playerEpisodes = eps;
  S.playerIndex = idx;
  S.playerPath = path;
  localStorage.setItem("fets:vid", String(videoId || 0));
  await go("player", videoId);
  // offer resume when there is a valid saved position
  if (resumePos > 30) {
    const r = await modal({
      title: "Lanjutkan menonton?",
      bodyHtml: `<p>Posisi terakhir: <b>${fmtTime(resumePos)}</b>. Lanjutkan dari sana, atau mulai dari awal?</p>`,
      confirmLabel: "Resume",
    });
    await api.playerLoad(path, r.confirmed ? resumePos : 0, videoId || null).catch((e) => toast(String(e), "error"));
  } else {
    await api.playerLoad(path, null, videoId || null).catch((e) => toast(String(e), "error"));
  }
}

async function renderPlayer(m: HTMLElement, videoId: number) {
  m.innerHTML = `
    <div class="player-wrap">
      <div id="player-host" class="player-host"></div>
      <div id="controls" class="controls">
        <div class="ctl-row seek-row">
          <span class="t" id="t-cur">0:00</span>
          <input id="seek" class="rng seek" type="range" min="0" max="1000" value="0"/>
          <span class="t" id="t-dur">--:--</span>
        </div>
        <div class="ctl-row">
          <button class="ic" id="c-prev" title="Episode sebelumnya (Shift+←)">⏮</button>
          <button class="ic big" id="c-play" title="Play/Pause (Spasi)">▶</button>
          <button class="ic" id="c-next" title="Episode berikutnya (Shift+→)">⏭</button>
          <button class="ic" id="c-mute" title="Mute (M)">🔊</button>
          <input id="vol" class="rng vol" type="range" min="0" max="130" value="100" title="Volume"/>
          <select id="spd" class="sel tiny" title="Playback speed">
            ${[0.5, 0.75, 1, 1.25, 1.5, 2].map((x) => `<option value="${x}">${x}×</option>`).join("")}
          </select>
          <select id="sub-trk" class="sel tiny" title="Subtitle"><option value="-1">Sub: off</option></select>
          <select id="aud-trk" class="sel tiny" title="Audio track"><option value="-1">Audio: auto</option></select>
          <select id="vid-trk" class="sel tiny" title="Video track"><option value="-1">Video: auto</option></select>
          <span class="grow"></span>
          <span class="fname" id="c-name"></span>
          <button class="ic" id="c-stop" title="Stop (Esc)">■</button>
          <button class="ic" id="c-fs" title="Fullscreen (F)">⛶</button>
        </div>
      </div>
    </div>`;
  const host = m.querySelector("#player-host")! as HTMLElement;

  // measure and create the native child window over #player-host
  try {
    await api.playerInit();
    syncBounds();
  } catch (e) {
    host.innerHTML = `<div class="ply-err"><h2>Player tidak tersedia</h2><p>${escapeHtml(String(e))}</p>
      <p class="muted">Unduh libmpv x64 dan taruh <code>libmpv-2.dll</code> di samping fets-anime.exe atau di folder <code>mpv\\</code> — lihat docs/MPV_SETUP.md.</p></div>`;
    return;
  }

  // subscribe to real MPV events before loading
  for (const u of S.unsubs) { try { u(); } catch {} }
  S.unsubs = [];
  S.unsubs.push(await listen<{ name: string; value: any }>("player://event", (ev) => onMpvEvent(ev.payload)));
  S.unsubs.push(await listen("player://resize", () => syncBounds()));

  bindControls(m);
  showControlsTemporarily();
  // periodic light status poll as safety net (events are primary source)
  clearInterval(S.posTimer);
  S.posTimer = window.setInterval(async () => {
    try { const st = await api.playerStatus(); mergeStatus(st); } catch { /* ignore transient */ }
  }, 2000);
}

function syncBounds() {
  const host = document.getElementById("player-host");
  if (!host) return;
  const r = host.getBoundingClientRect();
  // WebView2 reports CSS px == device px at scale 1; use device metrics directly
  const dpr = window.devicePixelRatio || 1;
  api.playerSetBounds(Math.round(r.left * dpr), Math.round(r.top * dpr), Math.round(r.width * dpr), Math.round(r.height * dpr)).catch(() => {});
}

function bindControls(m: HTMLElement) {
  m.querySelector("#c-play")!.addEventListener("click", () => togglePlay());
  m.querySelector("#c-prev")!.addEventListener("click", () => prevNext(-1));
  m.querySelector("#c-next")!.addEventListener("click", () => prevNext(1));
  m.querySelector("#c-stop")!.addEventListener("click", () => go("library"));
  m.querySelector("#c-mute")!.addEventListener("click", () => api.playerMute(!S.playerStatus.muted).catch(toastErr));
  m.querySelector("#c-fs")!.addEventListener("click", toggleFullscreen);
  const seek = m.querySelector<HTMLInputElement>("#seek")!;
  let seeking = false;
  seek.addEventListener("input", () => { seeking = true; m.querySelector("#t-cur")!.textContent = fmtTime((seek.valueAsNumber / 1000) * (S.playerStatus.duration || 0)); });
  seek.addEventListener("change", () => {
    const t = (seek.valueAsNumber / 1000) * (S.playerStatus.duration || 0);
    api.playerSeek(t, true).catch(toastErr);
    setTimeout(() => (seeking = false), 400);
  });
  (seek as any)._isSeeking = () => seeking;
  m.querySelector("#vol")!.addEventListener("input", (e) => api.playerVolume((e.target as HTMLInputElement).valueAsNumber).catch(toastErr));
  m.querySelector("#spd")!.addEventListener("change", (e) => api.playerSpeed(Number((e.target as HTMLSelectElement).value)).catch(toastErr));
  m.querySelector("#sub-trk")!.addEventListener("change", (e) => api.playerSetTrack("sub", Number((e.target as HTMLSelectElement).value)).catch(toastErr));
  m.querySelector("#aud-trk")!.addEventListener("change", (e) => api.playerSetTrack("audio", Number((e.target as HTMLSelectElement).value)).catch(toastErr));
  m.querySelector("#vid-trk")!.addEventListener("change", (e) => api.playerSetTrack("video", Number((e.target as HTMLSelectElement).value)).catch(toastErr));
  const wrap = m.querySelector(".player-wrap")!;
  wrap.addEventListener("mousemove", showControlsTemporarily);
  wrap.addEventListener("mouseleave", () => document.getElementById("controls")?.classList.add("hide"));
  document.addEventListener("keydown", onKey);
}

function unbindPlayerKeys() { document.removeEventListener("keydown", onKey); }

function onKey(e: KeyboardEvent) {
  if ((e.target as HTMLElement)?.tagName === "INPUT" || (e.target as HTMLElement)?.tagName === "SELECT") return;
  switch (e.key) {
    case " ": e.preventDefault(); togglePlay(); break;
    case "ArrowRight": if (e.shiftKey) { prevNext(1); } else api.playerSeek(5, false).catch(toastErr); break;
    case "ArrowLeft": if (e.shiftKey) { prevNext(-1); } else api.playerSeek(-5, false).catch(toastErr); break;
    case "ArrowUp": e.preventDefault(); api.playerVolume(Math.min(130, S.playerStatus.volume + 5)).catch(toastErr); break;
    case "ArrowDown": e.preventDefault(); api.playerVolume(Math.max(0, S.playerStatus.volume - 5)).catch(toastErr); break;
    case "m": case "M": api.playerMute(!S.playerStatus.muted).catch(toastErr); break;
    case "f": case "F": toggleFullscreen(); break;
    case "Escape": go("library"); break;
  }
}

function showControlsTemporarily() {
  const c = document.getElementById("controls");
  if (!c) return;
  c.classList.remove("hide");
  clearTimeout(S.controlsTimer);
  S.controlsTimer = window.setTimeout(() => { if (!document.fullscreenElement || true) c.classList.add("hide"); }, 3000);
}

async function togglePlay() {
  try { await api.playerPause(!S.playerStatus.paused); } catch (e) { toastErr(e); }
}

async function toggleFullscreen() {
  const w = document.querySelector(".player-wrap");
  if (!document.fullscreenElement) { await w?.requestFullscreen?.(); } else { await document.exitFullscreen?.(); }
  setTimeout(syncBounds, 120);
}

function prevNext(d: number) {
  if (S.playerEpisodes.length === 0) { toast("Tidak ada episode lain dalam daftar ini.", ""); return; }
  const ni = S.playerIndex + d;
  if (ni < 0 || ni >= S.playerEpisodes.length) { toast(d > 0 ? "Episode terakhir." : "Episode pertama.", ""); return; }
  const ep = S.playerEpisodes[ni];
  S.playerIndex = ni;
  S.playerPath = ep.path;
  api.playerLoad(ep.path, ep.positionSecs && ep.positionSecs > 30 ? ep.positionSecs : null, ep.id).catch(toastErr);
}

function teardownPlayerUi() {
  clearInterval(S.posTimer);
  unbindPlayerKeys();
  for (const u of S.unsubs) { try { u(); } catch {} }
  S.unsubs = [];
  api.playerStop().catch(() => {});
}

/* ---- MPV event sync: UI state derives from real backend events only ---- */

function onMpvEvent(ev: { name: string; value: any }) {
  const st = S.playerStatus;
  switch (ev.name) {
    case "pause": st.paused = !!ev.value; break;
    case "time-pos": if (!(document.querySelector("#seek") as any)?._isSeeking?.()) st.position = Number(ev.value || 0); break;
    case "duration": st.duration = Number(ev.value || 0); break;
    case "volume": st.volume = Number(ev.value || 0); break;
    case "mute": st.muted = !!ev.value; break;
    case "speed": st.speed = Number(ev.value || 1); break;
    case "filename": st.filename = String(ev.value ?? ""); break;
    case "idle-active": st.idle = !!ev.value; break;
    case "file-error": toast(String(ev.value), "error"); break;
    case "track-list": updateTracks(ev.value as TrackInfo[]); break;
  }
  paintControls();
}

function mergeStatus(p: PlayerStatus) {
  // poll result only fills gaps; events remain authoritative when fresh
  Object.assign(S.playerStatus, { ...p, position: S.playerStatus.position || p.position });
  paintControls();
}

function updateTracks(list: TrackInfo[]) {
  S.tracks = Array.isArray(list) ? list : [];
  const fill = (selId: string, kind: string, noneLabel: string) => {
    const sel = document.getElementById(selId) as HTMLSelectElement | null;
    if (!sel) return;
    const cur = sel.value;
    const items = S.tracks.filter((t) => t.kind === kind);
    sel.innerHTML = `<option value="-1">${noneLabel}</option>` +
      items.map((t) => `<option value="${t.id}">#${t.id}${t.title ? " " + escapeHtml(t.title) : ""}${t.lang ? " [" + t.lang + "]" : ""}${t.selected ? " ✓" : ""}</option>`).join("");
    const selected = items.find((t) => t.selected);
    sel.value = selected ? String(selected.id) : cur;
  };
  fill("sub-trk", "sub", "Sub: off");
  fill("aud-trk", "audio", "Audio: auto");
  fill("vid-trk", "video", "Video: auto");
}

function paintControls() {
  if (S.page !== "player") return;
  const st = S.playerStatus;
  const play = document.getElementById("c-play"); if (play) play.textContent = st.paused ? "▶" : "⏸";
  const mute = document.getElementById("c-mute"); if (mute) mute.textContent = st.muted || st.volume === 0 ? "🔇" : "🔊";
  const cur = document.getElementById("t-cur"); if (cur) cur.textContent = fmtTime(st.position);
  const dur = document.getElementById("t-dur"); if (dur) dur.textContent = fmtTime(st.duration);
  const seek = document.getElementById("seek") as HTMLInputElement | null;
  if (seek && !(seek as any)._isSeeking?.()) seek.value = String(st.duration > 0 ? Math.round((st.position / st.duration) * 1000) : 0);
  const vol = document.getElementById("vol") as HTMLInputElement | null; if (vol) vol.value = String(st.volume);
  const spd = document.getElementById("spd") as HTMLSelectElement | null; if (spd) spd.value = String(st.speed);
  const nm = document.getElementById("c-name"); if (nm) nm.textContent = st.filename;
  // report position to backend (throttled there too; backend ignores when history disabled)
  if (st.duration > 0 && !st.paused) api.reportPosition(Number(localStorage.getItem("fets:vid") || 0), st.position, st.duration).catch(() => {});
}

function toastErr(e: any) { toast(String(e), "error"); }

/* ------------------------------------------------------------------ boot */

async function boot() {
  buildSidebar();
  S.appInfo = await api.appInfo().catch(() => null);
  applyTheme((await api.getSettings().catch(() => null))?.theme ?? "dark");
  await refreshBadge();
  await initScanBanner(() => {
    // scan finished: refresh whatever list is on screen
    if (S.page === "library") loadGrid();
    else if (S.page === "home" || S.page === "detail") go(S.page, S.detailId || undefined);
  });
  await go("home");
  setInterval(refreshBadge, 15000);
}

boot().catch((e) => toast("Gagal memulai UI: " + e, "error"));
