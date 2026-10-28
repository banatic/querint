/**
 * Barrier-arm hold control. Small blocks stream through a lane; with hold on the
 * arm drops, blocks that already reached it pass, the rest brake and queue up.
 * The element outlives re-renders so its motion never restarts.
 */

const SPEED = 26; // cruise, px/s
const ACCEL = 90; // px/s²
const BRAKE = 160; // px/s²
const TRACK = 40;
const ENTRY = -9;
const W = 4.5; // block width
const GAP = 1.5;
const STOP = 19; // arm's left edge
const LINE = STOP - 1.5 - W; // where the front block waits
const SPRING = 230; // arm, 1/s²
const DAMP = 11;
const BOUNCE = 0.3; // energy kept when the arm hits its stop

type Part = { x: number; v: number; through: boolean; node: HTMLElement };

export class Gate {
  readonly el: HTMLButtonElement;
  private label: HTMLElement;
  private lane: HTMLElement;
  private parts: Part[] = [];
  private spawnIn = 0;
  private arm: HTMLElement;
  private angle = 0; // 0 = down (blocking), 90 = up
  private spin = 0;
  /** A click wins over snapshots taken before it reached the backend. */
  private clickedAt = -Infinity;
  hold = true;

  constructor(printer: string, hold: boolean) {
    this.el = document.createElement("button");
    this.el.className = "gate";
    this.el.dataset.act = "hold";
    this.el.dataset.printer = printer;
    this.el.innerHTML = `<span class="lbl"></span><span class="gt"><span class="lane"></span><span class="arm"></span><span class="pv"></span></span>`;
    this.label = this.el.querySelector(".lbl")!;
    this.lane = this.el.querySelector(".lane")!;
    this.arm = this.el.querySelector(".arm")!;
    this.set(hold);
    this.angle = hold ? 0 : 90;
    for (let i = 0; i < 300; i++) this.step(1 / 60); // open mid-flow
    this.draw();
  }

  set(hold: boolean) {
    this.hold = hold;
    this.el.classList.toggle("off", !hold);
    this.el.setAttribute("aria-pressed", String(hold));
    this.el.setAttribute("aria-label", hold ? "보류 끄기: 새 인쇄를 바로 보냄" : "보류 켜기");
    this.label.textContent = hold ? "보류" : "바로";
  }

  toggle(): boolean {
    this.clickedAt = performance.now();
    this.set(!this.hold);
    return this.hold;
  }

  /** Applies the backend's value unless a click just changed it. */
  sync(hold: boolean) {
    if (hold !== this.hold && performance.now() - this.clickedAt > 1500) this.set(hold);
  }

  step(dt: number) {
    // Arm: a spring toward its target that bounces off the mechanical stops at 0° and 90°.
    const target = this.hold ? 0 : 90;
    this.spin += (SPRING * (target - this.angle) - DAMP * this.spin) * dt;
    this.angle += this.spin * dt;
    if (this.angle < 0 || this.angle > 90) {
      this.angle = Math.max(0, Math.min(90, this.angle));
      this.spin *= -BOUNCE;
    }
    // While the arm is still mostly up, blocks reaching it slip through.
    const blocking = this.angle < 45;
    // Front-most first, so each block can follow the one ahead.
    this.parts.sort((a, b) => b.x - a.x);
    this.parts.forEach((q, i) => {
      // A block that can no longer brake before the line carries on instead of snapping back.
      if (blocking && !q.through && q.x + (q.v * q.v) / (2 * BRAKE) > LINE + 0.5) q.through = true;
      let limit = i > 0 ? this.parts[i - 1].x - (W + GAP) : Infinity;
      if (blocking && !q.through) limit = Math.min(limit, LINE);
      const want = Math.min(SPEED, Math.sqrt(2 * BRAKE * Math.max(0, limit - q.x)));
      q.v = Math.max(0, q.v + Math.max(-BRAKE * dt, Math.min(ACCEL * dt, want - q.v)));
      q.x = Math.max(q.x, Math.min(q.x + q.v * dt, limit));
      if (q.x + W > STOP) q.through = true;
    });
    this.parts = this.parts.filter((q) => {
      if (q.x < TRACK + 4) return true;
      q.node.remove();
      return false;
    });
    this.spawnIn -= dt;
    const last = this.parts[this.parts.length - 1];
    if (this.spawnIn <= 0 && (!last || last.x > ENTRY + W + GAP + 1)) {
      const node = document.createElement("span");
      node.className = "blk";
      this.lane.appendChild(node);
      this.parts.push({ x: ENTRY, v: SPEED * 0.8, through: false, node });
      this.spawnIn = 0.42 + Math.random() * 0.3;
    }
  }

  draw() {
    this.arm.style.transform = `rotate(${this.angle.toFixed(2)}deg)`;
    for (const q of this.parts) q.node.style.transform = `translateX(${q.x.toFixed(2)}px)`;
  }
}

const gates = new Map<string, Gate>();
let raf = 0;
let prev = 0;

function frame(now: number) {
  const dt = Math.min(0.05, (now - prev) / 1000);
  prev = now;
  for (const g of gates.values()) {
    g.step(dt);
    g.draw();
  }
  raf = requestAnimationFrame(frame);
}

/** Runs the animation only while the window is on screen. */
export function setVisible(visible: boolean) {
  cancelAnimationFrame(raf);
  if (!visible) return;
  prev = performance.now();
  raf = requestAnimationFrame(frame);
}

/** Swaps each `.gate-slot[data-printer]` for that printer's persistent gate. */
export function mountGates(root: HTMLElement, holds: Map<string, boolean>) {
  for (const name of gates.keys()) if (!holds.has(name)) gates.delete(name);
  root.querySelectorAll<HTMLElement>(".gate-slot").forEach((slot) => {
    const name = slot.dataset.printer!;
    const hold = holds.get(name) ?? true;
    let gate = gates.get(name);
    if (!gate) gates.set(name, (gate = new Gate(name, hold)));
    else gate.sync(hold);
    slot.replaceWith(gate.el);
  });
}

export function gateFor(name: string): Gate | undefined {
  return gates.get(name);
}
