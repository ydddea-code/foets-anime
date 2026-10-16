// Global scan-progress banner driven by real backend events (scan://progress).
// Cancel here calls the backend cancel command — it stops the actual worker,
// not just the UI.
import { listen } from "@tauri-apps/api/event";
import api from "../lib/ipc";
import type { ScanProgress } from "../lib/ipc";
import { toast } from "../lib/ui";

let listening = false;
let cancelRequested = false;

export async function initScanBanner(onFinished: () => void) {
  const banner = document.getElementById("scan-banner")!;
  banner.innerHTML = `
    <div class="scan-row">
      <span class="lbl" id="sc-state">Memindai…</span>
      <span class="scan-stats" id="sc-stats"></span>
      <button class="btn small danger" id="sc-cancel">Batal</button>
    </div>
    <div class="pbar indet" id="sc-pbar"><i style="width:0%"></i></div>
    <div class="scan-current" id="sc-current"></div>`;
  banner.classList.add("hidden");

  banner.querySelector("#sc-cancel")!.addEventListener("click", async () => {
    if (cancelRequested) return;
    cancelRequested = true;
    (banner.querySelector("#sc-cancel") as HTMLButtonElement).disabled = true;
    banner.querySelector<HTMLElement>("#sc-state")!.textContent = "Mengbatalkan…";
    try {
      const did = await api.cancelScan();
      if (!did) {
        cancelRequested = false;
        (banner.querySelector("#sc-cancel") as HTMLButtonElement).disabled = false;
        banner.querySelector<HTMLElement>("#sc-state")!.textContent = "Memindai…";
      }
    } catch (e) {
      cancelRequested = false;
      (banner.querySelector("#sc-cancel") as HTMLButtonElement).disabled = false;
      toast(String(e), "error");
    }
  });

  if (listening) return;
  listening = true;
  await listen<ScanProgress>("scan://progress", (ev) => update(ev.payload, onFinished));
  // Re-attach: if a scan survived page navigation, show its state again.
  const st = await api.scanStatus().catch(() => null);
  if (st?.running) banner.classList.remove("hidden");
}

function update(p: ScanProgress, onFinished: () => void) {
  const banner = document.getElementById("scan-banner")!;
  const done = p.state !== "scanning";
  if (done) {
    banner.classList.add("hidden");
    banner.querySelector<HTMLElement>("#sc-cancel")!.textContent = "Batal";
    (banner.querySelector("#sc-cancel") as HTMLButtonElement).disabled = false;
    cancelRequested = false;
    if (p.state === "completed") toast(`Pemindaian selesai — ${p.videosFound} video ditemukan`, "ok");
    if (p.state === "cancelled") toast("Pemindaian dibatalkan. Hasil yang sudah tersimpan dipertahankan.", "");
    if (p.state === "error") toast(`Pemindaian gagal: ${p.message ?? ""}`, "error");
    onFinished();
    return;
  }
  banner.classList.remove("hidden");
  banner.querySelector<HTMLElement>("#sc-state")!.textContent = "Memindai…";
  banner.querySelector<HTMLElement>("#sc-stats")!.textContent =
    `${p.filesChecked} file diperiksa · ${p.videosFound} video`;
  banner.querySelector<HTMLElement>("#sc-current")!.textContent = p.current ?? "";
  const bar = banner.querySelector<HTMLElement>("#sc-pbar")!;
  const fill = bar.querySelector("i")!;
  if (p.totalEstimate && p.totalEstimate > 0) {
    bar.classList.remove("indet");
    fill.style.width = `${Math.min(99, Math.round((p.filesChecked / p.totalEstimate) * 100))}%`;
  } else {
    bar.classList.add("indet"); // unknown total → indeterminate, never fake %
    fill.style.width = "";
  }
}
