// Anime grid card: cover, name, episode count, watch status, context menu.
import type { AnimeSummary } from "../lib/ipc";
import { coverSrc } from "../lib/ipc";
import { escapeHtml } from "../lib/ui";

export interface CardActions {
  open(anime: AnimeSummary): void;
  menu(anime: AnimeSummary, x: number, y: number): void;
}

const FALLBACK_COLORS = ["#3b2f63", "#254061", "#1f4d44", "#5a3049", "#4d4530"];

export function renderGrid(container: HTMLElement, items: AnimeSummary[], actions: CardActions) {
  container.innerHTML = "";
  if (items.length === 0) return;
  for (const a of items) container.appendChild(renderCard(a, actions));
}

export function renderCard(a: AnimeSummary, actions: CardActions): HTMLElement {
  const card = document.createElement("div");
  card.className = "card";
  const color = FALLBACK_COLORS[Math.abs(a.id) % FALLBACK_COLORS.length];
  const initial = escapeHtml((a.displayName.trim()[0] ?? "?").toUpperCase());
  const src = coverSrc(a);
  const pct =
    a.maxEpisodeSort && a.maxEpisodeSort > 0
      ? Math.min(100, Math.round(((a.maxEpisodeSort % 1000 || 1000) / 1000) * 0)) // no fake progress
      : 0;
  card.innerHTML = `
    <div class="cover" style="background:${color}">
      ${src ? `<img loading="lazy" src="${src}" alt="" />` : `<span class="ph">${initial}</span>`}
    </div>
    ${a.status === "missing" ? `<span class="badge missing">Folder hilang</span>` : ""}
    ${pct > 0 ? `<div class="progress-strip"><i style="width:${pct}%"></i></div>` : ""}
    <button class="menu-btn" title="Menu">⋮</button>
    <div class="card-info">
      <div class="card-title" title="${escapeHtml(a.displayName)}">${escapeHtml(a.displayName)}</div>
      <div class="card-sub">
        <span>${a.episodeCount} episode</span>
        <span>${watchStatusLabel(a)}</span>
      </div>
    </div>`;
  const img = card.querySelector("img");
  if (img) {
    img.addEventListener("error", () => {
      // Cover file moved/deleted on disk: fall back to generated placeholder.
      img.remove();
      const cov = card.querySelector(".cover");
      if (cov && !cov.querySelector(".ph")) cov.innerHTML = `<span class="ph">${initial}</span>`;
    });
  }
  card.addEventListener("click", () => actions.open(a));
  card.querySelector(".menu-btn")!.addEventListener("click", (e) => {
    e.stopPropagation();
    const r = (e.target as HTMLElement).getBoundingClientRect();
    actions.menu(a, r.left - 180, r.bottom + 4);
  });
  return card;
}

function watchStatusLabel(a: AnimeSummary): string {
  if (!a.lastWatchedAt) return "";
  return "Ditonton";
}
