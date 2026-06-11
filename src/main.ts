import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type Limits = {
  session_pct: number;
  session_resets_at: string | null;
  weekly_pct: number;
  weekly_resets_at: string | null;
  stale: boolean;
};
type DayStat = { date: string; tokens: number; cost_usd: number | null };
type LocalStats = {
  today_tokens: number;
  today_cost_usd: number;
  days: DayStat[];
  total_30d_tokens: number;
  total_30d_cost_usd: number;
  cost_is_estimate: boolean;
  stale: boolean;
};
type ApiSpend = { month_to_date_usd: number; stale: boolean };
type Snapshot = { limits: Limits | null; local: LocalStats | null; api_spend: ApiSpend | null };

const fmtTokens = (n: number) =>
  n >= 1e9 ? (n / 1e9).toFixed(1) + "B"
  : n >= 1e6 ? (n / 1e6).toFixed(1) + "M"
  : n >= 1e3 ? (n / 1e3).toFixed(1) + "K"
  : String(n);

const fmtUsd = (n: number) => "$" + (n >= 100 ? n.toFixed(0) : n.toFixed(2));

// Thresholds mirror AMBER_THRESHOLD/RED_THRESHOLD in src-tauri/src/tray_icon.rs — keep in sync.
const barClass = (pct: number) => (pct >= 85 ? "red" : pct >= 60 ? "amber" : "green");

function resetLabel(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const time = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const day = d.toLocaleDateString([], { weekday: "short" });
  return "resets " + (sameDay ? time : `${day} ${time}`);
}

function limitRow(label: string, pct: number, resetsAt: string | null): string {
  const clamped = Math.min(100, Math.max(0, pct));
  return `
    <div class="limit-row">
      <div class="limit-line">
        <span class="label">${label}</span>
        <div class="bar"><div class="fill ${barClass(pct)}" style="width:${clamped}%"></div></div>
        <span class="pct">${Math.round(clamped)}%</span>
      </div>
      <div class="reset">${resetLabel(resetsAt)}</div>
    </div>`;
}

function render(snap: Snapshot) {
  const limits = document.getElementById("limits")!;
  const local = document.getElementById("local")!;
  const spend = document.getElementById("spend")!;
  const stale = document.getElementById("stale")!;

  limits.innerHTML = snap.limits
    ? limitRow("Session", snap.limits.session_pct, snap.limits.session_resets_at) +
      limitRow("Weekly", snap.limits.weekly_pct, snap.limits.weekly_resets_at)
    : `<div class="hint">Sign in to Claude Code to see limits</div>`;

  if (snap.local) {
    const l = snap.local;
    const max = Math.max(1, ...l.days.map((d) => d.tokens));
    const bars = l.days
      .map(
        (d) =>
          `<div class="spark-bar" style="height:${Math.max(8, (d.tokens / max) * 100)}%"
                title="${d.date} · ${fmtTokens(d.tokens)} tok${d.cost_usd != null ? " · " + fmtUsd(d.cost_usd) : ""}"></div>`,
      )
      .join("");
    const approx = l.cost_is_estimate ? "≈" : "";
    local.innerHTML = `
      <div class="stat-row"><span class="label">Today</span>
        <span class="value">${fmtTokens(l.today_tokens)} tok&nbsp;&nbsp;${fmtUsd(l.today_cost_usd)}</span></div>
      <div class="spark">${bars}</div>
      <div class="spark-caption">last 7 days</div>
      <div class="stat-row"><span class="label">Total (30d)</span>
        <span class="value">${fmtTokens(l.total_30d_tokens)} tok&nbsp;&nbsp;${approx}${fmtUsd(l.total_30d_cost_usd)}</span></div>`;
  } else {
    local.innerHTML = `<div class="hint">No local Claude Code data found</div>`;
  }

  spend.innerHTML = snap.api_spend
    ? `<div class="divider"></div>
       <div class="stat-row"><span class="label">API key spend (mo)</span>
         <span class="value">${fmtUsd(snap.api_spend.month_to_date_usd)}</span></div>`
    : "";

  const anyStale = snap.limits?.stale || snap.local?.stale || snap.api_spend?.stale;
  stale.textContent = anyStale ? "stale" : "";
}

async function init() {
  render(await invoke<Snapshot>("get_snapshot"));
  await listen<Snapshot>("snapshot", (e) => render(e.payload));

  document.body.addEventListener("mouseenter", () => invoke("popup_hover", { inside: true }));
  document.body.addEventListener("mouseleave", () => invoke("popup_hover", { inside: false }));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") invoke("dismiss_popup").catch(() => {});
  });
}

init();
