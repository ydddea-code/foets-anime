// Small UI helpers: toast, modal, context menu. No heavy framework needed.

export function toast(msg: string, kind: "" | "error" | "ok" = "") {
  const root = document.getElementById("toast-root");
  if (!root) return;
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = msg;
  root.appendChild(el);
  setTimeout(() => {
    el.style.opacity = "0";
    el.style.transition = "opacity .3s";
    setTimeout(() => el.remove(), 320);
  }, kind === "error" ? 5200 : 3000);
}

export interface ModalResult {
  confirmed: boolean;
  value?: string;
}

/**
 * Promise-based modal. `opts.input` shows a text field whose initial value is
 * `opts.value`; confirmation button enabled only when the typed value equals
 * `opts.requireMatch` (used for destructive folder deletion).
 */
export function modal(opts: {
  title: string;
  bodyHtml: string;
  confirmLabel?: string;
  danger?: boolean;
  input?: boolean;
  value?: string;
  requireMatch?: string;
}): Promise<ModalResult> {
  return new Promise((resolve) => {
    const back = document.createElement("div");
    back.className = "modal-back";
    back.innerHTML = `
      <div class="modal">
        <h3>${escapeHtml(opts.title)}</h3>
        <div>${opts.bodyHtml}</div>
        ${opts.input ? `<input class="txt" type="text" spellcheck="false" />` : ""}
        <div class="modal-actions">
          <button class="btn" data-act="cancel">Batal</button>
          <button class="btn ${opts.danger ? "danger" : "primary"}" data-act="ok">
            ${escapeHtml(opts.confirmLabel ?? "OK")}
          </button>
        </div>
      </div>`;
    document.body.appendChild(back);
    const input = back.querySelector("input") as HTMLInputElement | null;
    const okBtn = back.querySelector('[data-act="ok"]') as HTMLButtonElement;
    if (input && opts.requireMatch) {
      input.value = "";
      okBtn.disabled = true;
      input.addEventListener("input", () => {
        okBtn.disabled = input.value.trim() !== opts.requireMatch;
      });
    } else if (input) {
      input.value = opts.value ?? "";
    }
    const close = (confirmed: boolean) => {
      back.remove();
      resolve({ confirmed, value: input?.value });
    };
    back.addEventListener("click", (e) => {
      if (e.target === back) close(false);
    });
    back.querySelector('[data-act="cancel"]')!.addEventListener("click", () => close(false));
    okBtn.addEventListener("click", () => close(true));
    if (input) {
      input.focus();
      input.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && !okBtn.disabled) close(true);
      });
    }
  });
}

export function contextMenu(x: number, y: number, items: { label: string; danger?: boolean; action: () => void }[]) {
  document.querySelectorAll(".ctx-menu").forEach((m) => m.remove());
  const menu = document.createElement("div");
  menu.className = "ctx-menu";
  for (const it of items) {
    const b = document.createElement("button");
    b.className = `ctx-item${it.danger ? " danger" : ""}`;
    b.textContent = it.label;
    b.addEventListener("click", () => {
      menu.remove();
      it.action();
    });
    menu.appendChild(b);
  }
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, window.innerWidth - r.width - 8)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - r.height - 8)}px`;
  const dismiss = (e: MouseEvent) => {
    if (!menu.contains(e.target as Node)) {
      menu.remove();
      window.removeEventListener("mousedown", dismiss, true);
    }
  };
  setTimeout(() => window.addEventListener("mousedown", dismiss, true), 0);
  window.addEventListener(
    "blur",
    () => {
      menu.remove();
    },
    { once: true }
  );
}

export function escapeHtml(s: string): string {
  return s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

export function debounce<T extends (...args: any[]) => void>(fn: T, ms: number): T {
  let t: number | undefined;
  return ((...args: any[]) => {
    clearTimeout(t);
    t = window.setTimeout(() => fn(...args), ms);
  }) as unknown as T;
}
