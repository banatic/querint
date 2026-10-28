/**
 * Barrier-arm hold control. Small blocks stream through a lane; with hold on the
 * arm drops, blocks that already reached it pass, the rest brake and queue up.
 * The element outlives re-renders so its motion never restarts.
 *
 * Every lane runs on one shared conveyor: evenly spaced slots moving at SPEED,
 * driven by a single clock, so blocks line up across printers. A block freed
 * from a queue eases onto the nearest free slot.
 */

const SPEED = 26; // cruise, px/s
const ACCEL = 90; // px/s²
const BRAKE = 160; // px/s²
const PITCH = 11; // slot spacing, px
const CATCH = 4; // how hard a block closes on its slot, 1/s
const TRACK = 40;
const ENTRY = -9;
const W = 4.5; // block width
const GAP = 1.5;
const STOP = 19; // arm's left edge
const LINE = STOP - 1.5 - W; // where the front block waits
const SPRING = 230; // arm, 1/s²
const DAMP = 11;
const BOUNCE = 0.3; // energy kept when the arm hits its stop

/** Slot n sits at conveyor(t) - n * PITCH. */
const conveyor = (t: number) => SPEED * t;

/** `slot` is null while queued; the block picks one again once it moves freely. */
type Part = { x: number; v: number; through: boolean; slot: number | null; node: HTMLElement };

let clock = 0; // shared by all gates, advances only while animating

export class Gate {
  readonly el: HTMLButtonElement;
  private label: HTMLElement;
  private lane: HTMLElement;
  private parts: Part[] = [];
  private spawned = -Infinity; // last slot that reached the entry
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
    for (let i = 300; i > 0; i--) this.step(1 / 60, clock - i / 60); // open mid-flow, in step with the others
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

  step(dt: number, t: number) {
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
    const belt = conveyor(t);
    // Front-most first, so each block can follow the one ahead.
    this.parts.sort((a, b) => b.x - a.x);
    let taken = -Infinity;
    this.parts.forEach((q, i) => {
      // A block that can no longer brake before the line carries on instead of snapping back.
      if (blocking && !q.through && q.x + (q.v * q.v) / (2 * BRAKE) > LINE + 0.5) q.through = true;
      let limit = i > 0 ? this.parts[i - 1].x - (W + GAP) : Infinity;
      if (blocking && !q.through) limit = Math.min(limit, LINE);
      // Each slot holds one block, in order.
      if (q.slot === null) q.slot = Math.round((belt - q.x) / PITCH);
      if (q.slot <= taken) q.slot = taken + 1;
      taken = q.slot;
      const cruise = Math.max(SPEED * 0.45, Math.min(SPEED * 1.6, SPEED + CATCH * (belt - q.slot * PITCH - q.x)));
      const room = Math.max(0, limit - q.x);
      const want = Math.min(cruise, Math.sqrt(2 * BRAKE * room));
      q.v = Math.max(0, q.v + Math.max(-BRAKE * dt, Math.min(ACCEL * dt, want - q.v)));
      q.x = Math.max(q.x, Math.min(q.x + q.v * dt, limit));
      if (room < 3 && want < cruise) q.slot = null; // held back: rejoin the belt later
      if (q.x + W > STOP) q.through = true;
    });
    this.parts = this.parts.filter((q) => {
      if (q.x < TRACK + 4) return true;
      q.node.remove();
      return false;
    });
    // A new block boards each slot as it passes the entry, if there is room.
    const slot = Math.floor((belt - ENTRY) / PITCH);
    if (slot > this.spawned) {
      this.spawned = slot;
      const last = this.parts[this.parts.length - 1];
      if (!last || last.x > ENTRY + W + GAP + 1) {
        const node = document.createElement("span");
        node.className = "blk";
        this.lane.appendChild(node);
        this.parts.push({ x: belt - slot * PITCH, v: SPEED, through: false, slot, node });
      }
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
  clock += dt;
  for (const g of gates.values()) {
    g.step(dt, clock);
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
