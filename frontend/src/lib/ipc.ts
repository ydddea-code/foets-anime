// Typed bridge to the Rust backend. All IPC goes through invoke(); the
// frontend never executes shell commands or touches the filesystem directly.
import { invoke } from "@tauri-apps/api/core";
import { convertFileSrc } from "@tauri-apps/api/core";

export interface AnimeSummary {
  id: number;
  displayName: string;
  folderPath: string;
  episodeCount: number;
  status: "ok" | "missing" | string;
  coverUrl: string | null;
  lastWatchedAt: number | null;
  maxEpisodeSort: number | null;
}

export interface EpisodeInfo {
  id: number;
  animeId: number;
  fileName: string;
  relPath: string;
  path: string;
  durationSecs: number | null;
  width: number | null;
  height: number | null;
  size: number;
  positionSecs: number | null;
  watchedAt: number | null;
  completed: boolean;
}

export interface HistoryEntry {
  videoId: number;
  animeId: number;
  animeName: string;
  fileName: string;
  relPath: string;
  path: string;
  positionSecs: number;
  durationSecs: number | null;
  watchedAt: number;
  completed: boolean;
  coverUrl: string | null;
}

export interface PlayerStatus {
  idle: boolean;
  paused: boolean;
  position: number;
  duration: number;
  volume: number;
  muted: boolean;
  speed: number;
  filename: string;
}

export interface TrackInfo {
  id: number;
  kind: string; // video | audio | sub
  selected: boolean;
  title?: string | null;
  lang?: string | null;
}

export interface ScanProgress {
  state: "scanning" | "completed" | "cancelled" | "error" | string;
  filesChecked: number;
  videosFound: number;
  current: string;
  totalEstimate: number | null;
  message: string | null;
}

export interface Settings {
  theme: string;
  historyEnabled: boolean;
  autoThumbnail: boolean;
  subtitleLang: string;
  audioLang: string;
  defaultSpeed: number;
  defaultVolume: number;
  keepAwake: boolean;
}

export interface AppInfo {
  name: string;
  version: string;
  platform: string;
  mpvEmbedded: boolean;
  dataDir: string | null;
}

const api = {
  addAnimeFolder: (path: string, asParent: boolean) =>
    invoke<{ added: { id: number; name: string; created: boolean }[] }>("add_anime_folder", {
      path,
      asParent,
    }),
  listAnime: (search?: string, sort?: string) =>
    invoke<AnimeSummary[]>("list_anime", { search: search ?? "", sort: sort ?? "name" }),
  getAnime: (id: number) => invoke<AnimeSummary>("get_anime", { id }),
  listEpisodes: (animeId: number) => invoke<EpisodeInfo[]>("list_episodes", { animeId }),
  renameAnime: (id: number, name: string) => invoke("rename_anime", { id, name }),
  removeFromLibrary: (id: number, confirm: boolean) =>
    invoke("remove_from_library", { id, confirm }),
  deleteFolderFromDisk: (id: number, confirmedPath: string) =>
    invoke("delete_folder_from_disk", { id, confirmedPath }),
  changeCover: (animeId: number, imagePath: string) =>
    invoke<string>("change_cover", { animeId, imagePath }),
  resetCover: (animeId: number) => invoke("reset_cover", { animeId }),
  removeCustomCover: (animeId: number) => invoke("remove_custom_cover", { animeId }),
  scanLibrary: (animeIds: number[] | null, forceFull: boolean) =>
    invoke<{ running: boolean; jobId: number }>("scan_library", {
      animeIds,
      forceFull,
    }),
  cancelScan: () => invoke<boolean>("cancel_scan"),
  scanStatus: () => invoke<{ running: boolean; jobId: number }>("scan_status"),
  playerInit: () => invoke("player_init"),
  playerSetBounds: (x: number, y: number, w: number, h: number) =>
    invoke("player_set_bounds", { x, y, w, h }),
  playerLoad: (path: string, startPos: number | null, videoId: number | null) =>
    invoke("player_load", { path, startPos, videoId }),
  playerPause: (pause: boolean) => invoke("player_pause", { pause }),
  playerSeek: (secs: number, absolute: boolean) => invoke("player_seek", { secs, absolute }),
  playerVolume: (volume: number) => invoke("player_volume", { volume }),
  playerMute: (muted: boolean) => invoke("player_mute", { muted }),
  playerSpeed: (speed: number) => invoke("player_speed", { speed }),
  playerSetTrack: (kind: string, id: number) => invoke("player_set_track", { kind, id }),
  playerStatus: () => invoke<PlayerStatus>("player_status"),
  playerTracks: () => invoke<TrackInfo[]>("player_tracks"),
  playerStop: () => invoke("player_stop"),
  continueWatching: () => invoke<HistoryEntry[]>("continue_watching"),
  recentlyWatched: () => invoke<HistoryEntry[]>("recently_watched"),
  clearHistory: () => invoke("clear_history"),
  reportPosition: (videoId: number, position: number, duration: number | null) =>
    invoke("report_position", { videoId, position, duration }),
  getSettings: () => invoke<Settings>("get_settings"),
  setSetting: (key: string, value: string) => invoke("set_setting", { key, value }),
  openDataDir: () => invoke<string>("open_data_dir"),
  appInfo: () => invoke<AppInfo>("app_info"),
};

export function coverSrc(a: { coverUrl: string | null }): string | null {
  if (!a.coverUrl) return null;
  try {
    return convertFileSrc(a.coverUrl);
  } catch {
    return null;
  }
}

export function fmtTime(secs: number | null | undefined): string {
  if (secs == null || !isFinite(secs) || secs < 0) return "--:--";
  const s = Math.floor(secs % 60);
  const m = Math.floor((secs / 60) % 60);
  const h = Math.floor(secs / 3600);
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${m}:${ss}`;
}

export function fmtSize(bytes: number): string {
  if (bytes <= 0) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = bytes;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}

export function timeAgo(epochSecs: number | null): string {
  if (!epochSecs) return "";
  const diff = Date.now() / 1000 - epochSecs;
  if (diff < 3600) return `${Math.max(1, Math.floor(diff / 60))} menit lalu`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} jam lalu`;
  if (diff < 30 * 86400) return `${Math.floor(diff / 86400)} hari lalu`;
  return new Date(epochSecs * 1000).toLocaleDateString();
}

export default api;
