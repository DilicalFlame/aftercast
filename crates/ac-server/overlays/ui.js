// aftercast overlay helpers (plugin API v1): small DOM helpers the first party overlays share.
//
//   import { el, row, clock, nameColor } from "/sdk/ui.js";

/** A new element: el("span", "label", "text"). */
export const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

/** A small card: a label (a float title, a pill or a block, per skin) beside its content. */
export const row = (cls, label, ...kids) => {
  const c = el("div", `card row ${cls}`);
  c.append(el("span", "label", label), ...kids);
  return c;
};

/** 754 seconds as 12:34 (or 1:02:03 from an hour). */
export const clock = (secs) => {
  secs = Math.max(0, Math.floor(secs));
  const h = Math.floor(secs / 3600), m = Math.floor(secs / 60) % 60, s = secs % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
};

/** The TUI's name palette (golden-angle hues from blue), picked by the name. */
export const nameColor = (s) => {
  let h = 0;
  for (const c of s) h = (h * 31 + c.codePointAt(0)) >>> 0;
  return `hsl(${(210 + (h % 360) * 137.507764) % 360} 75% ${[70, 60, 80][h % 3]}%)`;
};
