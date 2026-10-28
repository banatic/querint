import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { gateFor, mountGates, setVisible } from "./gate";

type JobState = "held" | "queued" | "printing" | "waiting";
type Job = { id: number; name: string; pages: number; time: string; state: JobState };
type Printer = { name: string; offline: boolean; hold: boolean; jobs: Job[] };
type Snapshot = {
  printers: Printer[];
  choices: { name: string; managed: boolean }[];
  autostart: boolean;
  notify: boolean;
  held: number;
  active: number;
};

const panel = document.getElementById("panel")!;
let snap: Snapshot | null = null;
let settingsOpen = false;

const ICON = {
  pause: `<svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="5" y="4" width="5" height="16" rx="1.5"/><rect x="14" y="4" width="5" height="16" rx="1.5"/></svg>`,
  stop: `<svg width="9" height="9" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="5" y="5" width="14" height="14" rx="2"/></svg>`,
  x: `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true"><path d="M7 7l10 10M17 7L7 17"/></svg>`,
  spin: `<svg class="spin" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" aria-hidden="true"><path d="M12 3a9 9 0 1 0 9 9"/></svg>`,
  check: `<svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>`,
  chevron: `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6"/></svg>`,
};

const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

/** Spooler document names often carry the app name and the file extension. Web page titles like "naver.com" stay. */
const docName = (s: string) =>
  s
    .replace(/^Microsoft (Word|Excel|PowerPoint) - /, "")
    .replace(/\.(hwpx?|hwt|cell|show|pdf|docx?|xlsx?|xlsm|pptx?|txt|rtf|csv|jpe?g|png|gif|bmp|tiff?|html?)$/i, "")
    .trim() || s;

const META: Record<JobState, string> = { held: "", queued: "대기", printing: "인쇄 중", waiting: "연결 대기" };

function jobRow(p: Printer, j: Job): string {
  const isHeld = j.state === "held";
  const glyph = isHeld
    ? `<span style="color: var(--signal); display: flex">${ICON.pause}</span>`
    : j.state === "printing"
      ? ICON.spin
      : `<span class="pending"></span>`;
  const meta = isHeld ? (j.pages > 0 ? `${j.pages}p · ${j.time}` : j.time) : META[j.state];
  const data = `data-printer="${esc(p.name)}" data-job="${j.id}"`;
  return `
    <div class="row job is-${j.state}">
      <span class="glyph" aria-hidden="true">${glyph}</span>
      <span class="name">${esc(docName(j.name))}</span>
      <span class="meta num hover-hide">${meta}</span>
      ${
        isHeld
          ? `<div class="act">
               <button class="go" data-act="release" ${data}>인쇄</button>
               <button class="x" data-act="delete" ${data} aria-label="삭제">${ICON.x}</button>
             </div>`
          : ""
      }
      ${j.state === "printing" ? `<div class="bar"><div></div></div>` : ""}
    </div>`;
}

function printerBlock(p: Printer, managed: boolean): string {
  const active = p.jobs.some((j) => j.state !== "held");
  const data = `data-printer="${esc(p.name)}"`;
  const right = active ? `<button class="stop" data-act="stop" ${data}>${ICON.stop}정지</button>` : "";
  return `
    <div class="row printer${p.offline ? " offline" : ""}">
      <span class="name">${esc(p.name)}</span>
      ${p.offline ? `<span class="tag">오프라인</span>` : ""}
      <span style="flex-grow: 1"></span>
      ${right}
      ${managed ? `<span class="gate-slot" ${data}></span>` : ""}
    </div>
    ${p.jobs.map((j) => jobRow(p, j)).join("")}
    <div class="sep"></div>`;
}

function check(on: boolean): string {
  return `<span class="check${on ? " on" : ""}">${ICON.check}</span>`;
}

// A re-render between press and release would swap the button out and drop the click.
let pressed = false;
let stale = false;
panel.addEventListener("pointerdown", () => (pressed = true));
const unpress = () => {
  pressed = false;
  if (stale) setTimeout(render); // after the click has fired
};
for (const ev of ["pointerup", "pointercancel", "blur"]) window.addEventListener(ev, unpress);

function render() {
  if (!snap) return;
  if (pressed) return void (stale = true);
  stale = false;
  const s = snap;
  const managed = new Set(s.choices.filter((c) => c.managed).map((c) => c.name));
  const scroll = panel.querySelector(".list")?.scrollTop ?? 0;
  if (!s.choices.some((c) => c.managed)) settingsOpen = true;

  const settings = settingsOpen
    ? `
      <button class="item indent" data-act="option" data-key="autostart" aria-pressed="${s.autostart}">
        <span class="grow">Windows 시작 시 실행</span>${check(s.autostart)}
      </button>
      <button class="item indent" data-act="option" data-key="notify" aria-pressed="${s.notify}">
        <span class="grow">프린터 연결되면 알림</span>${check(s.notify)}
      </button>
      <div class="label">사용할 프린터</div>
      ${s.choices
        .map(
          (c) => `
        <button class="item indent small" data-act="manage" data-printer="${esc(c.name)}" aria-pressed="${c.managed}">
          <span class="grow">${esc(c.name)}</span>${check(c.managed)}
        </button>`,
        )
        .join("")}`
    : "";

  panel.innerHTML = `
    <div class="title">Querint</div>
    <div class="sep"></div>
    <button class="item strong" data-act="release-all">
      <span class="grow">전체 인쇄</span><span class="meta num">${s.held}건</span>
    </button>
    ${
      s.active > 0
        ? `<button class="item" data-act="stop-all">
             <span class="grow">전체 정지</span><span class="meta signal num">${s.active}건 진행 중</span>
           </button>`
        : ""
    }
    <div class="sep"></div>
    <div class="list">
      ${s.printers.map((p) => printerBlock(p, managed.has(p.name))).join("")}
      <button class="item" data-act="settings" aria-expanded="${settingsOpen}">
        <span class="grow">설정</span><span class="chev${settingsOpen ? " open" : ""}">${ICON.chevron}</span>
      </button>
      ${settings}
    </div>
    <div class="sep"></div>
    <button class="item muted" data-act="quit">종료</button>`;

  mountGates(panel, new Map(s.printers.map((p) => [p.name, p.hold])));
  invoke("fit", { height: panel.getBoundingClientRect().height });
  panel.querySelector(".list")!.scrollTop = scroll;
}

panel.addEventListener("click", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>("[data-act]");
  if (!el) return;
  const printer = el.dataset.printer;
  const job = el.dataset.job ? Number(el.dataset.job) : undefined;
  switch (el.dataset.act) {
    case "release":
      return invoke("release", { printer, job });
    case "release-all":
      return invoke("release_all");
    case "stop":
      return invoke("stop", { printer });
    case "stop-all":
      return invoke("stop", { printer: null });
    case "delete":
      return invoke("delete_job", { printer, job });
    case "hold": {
      // Flip right away so the arm moves on click, not on the next snapshot.
      const gate = gateFor(printer!);
      if (!gate) return;
      return invoke("set_hold", { printer, hold: gate.toggle() });
    }
    case "manage":
      return invoke("set_managed", { printer, managed: el.getAttribute("aria-pressed") !== "true" });
    case "option":
      return invoke("set_option", { key: el.dataset.key, value: el.getAttribute("aria-pressed") !== "true" });
    case "settings":
      settingsOpen = !settingsOpen;
      return render();
    case "quit":
      return invoke("quit");
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") invoke("hide");
});
document.addEventListener("contextmenu", (e) => e.preventDefault());

listen<boolean>("visible", (e) => setVisible(e.payload));
listen<Snapshot>("state", (e) => {
  snap = e.payload;
  render();
});
invoke<Snapshot>("get_state").then((s) => {
  snap = s;
  render();
});
