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
type KimiLimits = {
  five_hour_pct: number | null;
  five_hour_resets_at: string | null;
  weekly_pct: number | null;
  weekly_resets_at: string | null;
  fetched_at: string;
  stale: boolean;
};
type KimiLocal = { stats: LocalStats; unpriced_today_tokens: number; unpriced_30d_tokens: number };
type KimiBalance = { available: number; currency: "USD" | "CNY"; fetched_at: string; stale: boolean };
type Kimi = {
  has_subscription: boolean;
  login_expired: boolean;
  limits: KimiLimits | null;
  local: KimiLocal | null;
  balance: KimiBalance | null;
};
type Snapshot = {
  limits: Limits | null;
  local: LocalStats | null;
  api_spend: ApiSpend | null;
  kimi: Kimi | null;
};
type Tab = "claude" | "kimi";

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

function limitRow(label: string, pct: number | null, resetsAt: string | null): string {
  const has = pct != null;
  const clamped = has ? Math.min(100, Math.max(0, pct)) : 0;
  return `
    <div class="limit-row">
      <div class="limit-line">
        <span class="label">${label}</span>
        <div class="bar"><div class="fill ${has ? barClass(pct) : "gray"}" style="width:${has ? clamped : 100}%"></div></div>
        <span class="pct">${has ? Math.round(clamped) + "%" : "—"}</span>
      </div>
      <div class="reset">${resetLabel(resetsAt)}</div>
    </div>`;
}

function renderLocal(l: LocalStats | null, emptyHint: string, unpricedToday = 0): string {
  if (!l) return `<div class="hint">${emptyHint}</div>`;
  const max = Math.max(1, ...l.days.map((d) => d.tokens));
  const bars = l.days
    .map(
      (d) =>
        `<div class="spark-bar" style="height:${Math.max(8, (d.tokens / max) * 100)}%"
              title="${d.date} · ${fmtTokens(d.tokens)} tok${d.cost_usd != null ? " · " + fmtUsd(d.cost_usd) : ""}"></div>`,
    )
    .join("");
  const approx = l.cost_is_estimate ? "≈" : "";
  const incl =
    unpricedToday > 0 ? ` <span title="Subscription usage has no per-token price">+ incl.</span>` : "";
  return `
    <div class="stat-row"><span class="label">Today</span>
      <span class="value">${fmtTokens(l.today_tokens)} tok&nbsp;&nbsp;${fmtUsd(l.today_cost_usd)}${incl}</span></div>
    <div class="spark">${bars}</div>
    <div class="spark-caption">last 7 days</div>
    <div class="stat-row"><span class="label">Total (30d)</span>
      <span class="value">${fmtTokens(l.total_30d_tokens)} tok&nbsp;&nbsp;${approx}${fmtUsd(l.total_30d_cost_usd)}</span></div>`;
}

function readTab(): Tab {
  try {
    return localStorage.getItem("popup-tab") === "kimi" ? "kimi" : "claude";
  } catch {
    return "claude";
  }
}

let lastSnap: Snapshot | null = null;

function selectTab(tab: Tab) {
  try {
    localStorage.setItem("popup-tab", tab);
  } catch {
    /* storage unavailable */
  }
  if (lastSnap) render(lastSnap);
}

function tabButton(tab: Tab, name: string, pct: number | null | undefined, active: boolean): string {
  const pctHtml =
    pct != null ? ` <span class="tab-pct ${barClass(pct)}">${Math.round(pct)}%</span>` : "";
  return `<button class="tab${active ? " active" : ""}" data-tab="${tab}">${name}${pctHtml}</button>`;
}

function renderKimi(k: Kimi): string {
  let html = "";
  if (k.has_subscription) {
    const lim = k.limits;
    html +=
      limitRow("5-hour", lim?.five_hour_pct ?? null, lim?.five_hour_resets_at ?? null) +
      limitRow("Weekly", lim?.weekly_pct ?? null, lim?.weekly_resets_at ?? null);
    if (k.login_expired) html += `<div class="hint">Open Kimi Code to refresh sign-in</div>`;
  }
  if (k.balance) {
    const sym = k.balance.currency === "CNY" ? "¥" : "$";
    html += `<div class="stat-row"><span class="label">API balance</span>
      <span class="value">${sym}${k.balance.available.toFixed(2)}</span></div>`;
  }
  if (html) html += `<div class="divider"></div>`;
  html += renderLocal(
    k.local?.stats ?? null,
    "No local Kimi Code data found",
    k.local?.unpriced_today_tokens ?? 0,
  );
  return html;
}

function render(snap: Snapshot) {
  lastSnap = snap;
  const limits = document.getElementById("limits")!;
  const local = document.getElementById("local")!;
  const spend = document.getElementById("spend")!;
  const stale = document.getElementById("stale")!;
  const tabs = document.getElementById("tabs")!;
  const title = document.getElementById("title")!;
  const panelClaude = document.getElementById("panel-claude")!;
  const panelKimi = document.getElementById("panel-kimi")!;

  const kimi = snap.kimi;
  const active: Tab = kimi && readTab() === "kimi" ? "kimi" : "claude";

  if (kimi) {
    tabs.hidden = false;
    title.hidden = true;
    tabs.innerHTML =
      tabButton("claude", "Claude", snap.limits?.weekly_pct, active === "claude") +
      tabButton("kimi", "Kimi", kimi.limits?.weekly_pct, active === "kimi");
    tabs.querySelectorAll<HTMLButtonElement>(".tab").forEach((b) => {
      b.onclick = () => selectTab(b.dataset.tab as Tab);
    });
  } else {
    tabs.hidden = true;
    tabs.innerHTML = "";
    title.hidden = false;
  }
  panelClaude.hidden = active !== "claude";
  panelKimi.hidden = active !== "kimi";

  limits.innerHTML = snap.limits
    ? limitRow("Session", snap.limits.session_pct, snap.limits.session_resets_at) +
      limitRow("Weekly", snap.limits.weekly_pct, snap.limits.weekly_resets_at)
    : `<div class="hint">Sign in to Claude Code to see limits</div>`;

  local.innerHTML = renderLocal(snap.local, "No local Claude Code data found");

  spend.innerHTML = snap.api_spend
    ? `<div class="divider"></div>
       <div class="stat-row"><span class="label">API key spend (mo)</span>
         <span class="value">${fmtUsd(snap.api_spend.month_to_date_usd)}</span></div>`
    : "";

  panelKimi.innerHTML = kimi ? renderKimi(kimi) : "";

  const anyStale =
    active === "kimi" && kimi
      ? kimi.limits?.stale || kimi.local?.stats.stale || kimi.balance?.stale
      : snap.limits?.stale || snap.local?.stale || snap.api_spend?.stale;
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
