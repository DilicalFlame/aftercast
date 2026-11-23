import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
const clear = () => root.replaceChildren();
const KEYS = { burst: 700, life: 2600 };
const MOD = { Ctrl: "C", Alt: "M", Shift: "S", Win: "D" };
const KEY = {
  Enter: "CR", Del: "Del", "↑": "Up", "↓": "Down", "←": "Left", "→": "Right",
  PgUp: "PageUp", PgDn: "PageDown",
};
const vim = (combo) => {
  const parts = combo.split("+");
  let key = parts.pop();
  key = KEY[key] || (key.length === 1 ? key.toLowerCase() : key);
  return [...parts.map((m) => MOD[m] || m), key].join("-");
};
let stack = null;
const frame = () => {
  stack = el("div", "keys");
  root.replaceChildren(stack);
};
// Shown even when empty, so the overlay editor can draw the frame.
frame();
let live = null;
let liveIdle = null;
// Moves the bubbles already shown smoothly to where `change` puts them.
const flip = (change) => {
  const kids = [...stack.children];
  const before = kids.map((k) => k.getBoundingClientRect().top);
  change();
  const scale = parseFloat(getComputedStyle(stack).getPropertyValue("--s")) || 1;
  kids.forEach((k, i) => {
    const dy = (before[i] - k.getBoundingClientRect().top) / scale;
    if (dy) k.animate([{ transform: `translateY(${dy}px)` }, { transform: "none" }], { duration: 220, easing: "cubic-bezier(.2,.8,.2,1)" });
  });
};
const fade = (b) => {
  if (b.leaving) return;
  b.leaving = true;
  clearTimeout(b.timer);
  // From the opacity it has now (older bubbles are dimmer), not from full.
  const from = parseFloat(getComputedStyle(b).opacity) || 1;
  b.animate([{ opacity: from }, { opacity: 0, transform: "translateY(-18px)" }],
    { duration: 380, easing: "ease-in", fill: "forwards" });
  // A timer, not the animation's end: a hidden page may never finish animations.
  setTimeout(() => b.remove(), 400);
  if (live && live.el === b) live = null;
};
const keep = (b) => {
  clearTimeout(b.timer);
  b.timer = setTimeout(() => fade(b), KEYS.life);
};
const bubble = (cls) => {
  if (!stack || !stack.isConnected) frame();
  const b = el("div", `key ${cls}`);
  flip(() => stack.append(b));
  b.animate([{ opacity: 0, transform: "translateY(24px) scale(.85)" }, { opacity: 1, transform: "none" }],
    { duration: 200, easing: "cubic-bezier(.2,.8,.2,1)" });
  keep(b);
  return b;
};
// Bubbles pushed above the frame's top (layout position, not the running animation).
const overflow = () => {
  for (const k of stack.children) if (k.offsetTop < 0) fade(k);
};
const endLive = () => {
  if (live) live.el.classList.remove("live");
  live = null;
};
const draw = (b, text) =>
  b.replaceChildren(...text.split(" ").flatMap((p, i) => (i ? [el("span", "space", "␣"), p] : [p])));
// Whole bubble (padding, border and cursor) inside the frame. Layout widths, so neither
// the entry animation's scale nor the overlay's size skews them.
const fits = (b) => b.offsetWidth <= stack.clientWidth;
const paint = () => {
  draw(live.el, live.text);
  live.el.classList.add("live");
  overflow();
  keep(live.el);
  clearTimeout(liveIdle);
  liveIdle = setTimeout(endLive, KEYS.burst);
};
// The live bubble just got wider than the frame with `ch`. A space ends it (the wrap
// swallows it, as in an editor); otherwise the word being typed carries over to a new
// bubble, or only `ch` when that word alone is too wide.
const wrap = (ch) => {
  const old = live;
  const before = old.text.slice(0, -1); // fitted
  const cut = before.lastIndexOf(" ");
  let stays = before, carry = ch;
  if (ch === " ") carry = "";
  else if (cut > 0) [stays, carry] = [before.slice(0, cut), old.text.slice(cut + 1)];
  old.text = stays;
  draw(old.el, stays);
  endLive();
  if (!stays) fade(old.el);
  if (!carry) return;
  live = { el: bubble("text"), text: carry };
  paint();
  if (carry.length > 1 && !fits(live.el)) {
    old.text = before;
    draw(old.el, before);
    live.text = ch;
    paint();
  }
};
// Both labels: Vim notation, and the plain one (Ctrl+S) for skins that show that.
const chord = (label, plain) => {
  endLive();
  const b = bubble("chord");
  b.append(el("span", "angle", "<"), el("span", "name", label), el("span", "angle", ">"),
    el("span", "plain", plain));
  overflow();
};

// Password boxes and denied apps never reach the overlay.
const keys = (d) => {
  if (d.combo) return chord(vim(d.combo), d.combo);
  if (d.back) {
    if (!live) return chord("BS", "Backspace");
    live.text = live.text.slice(0, -1);
    if (live.text) return paint();
    fade(live.el);
    return;
  }
  if (!d.ch) return;
  if (!live) live = { el: bubble("text"), text: "" };
  live.text += d.ch;
  paint();
  // Measured before the browser paints, so the overflow is never seen.
  if (!fits(live.el)) wrap(d.ch);
};
ac.on("keys", (m) => { if (m.action === "hide") return; keys(m.data || {}); });

// The editor's sample (and a skin thumbnail): someone typing, in a loop.
if (ac.mode !== "live") {
  const steps = ["c", "i", "w", 200, "Ctrl+S", "Esc", 400, ":", "w", "Enter", 500, "Ctrl+d", 300, "d", "d", "p", 600,
    "Ctrl+Shift+p", 400, "f", "n", " ", "m", "a", "i", "n", 900];
  let i = 0;
  const step = () => {
    const s = steps[i++ % steps.length];
    if (typeof s === "number") return setTimeout(step, s);
    if (s.length > 1) keys({ combo: s, ch: s === "Enter" ? "\n" : undefined });
    else keys({ ch: s });
    setTimeout(step, 110);
  };
  step();
}
