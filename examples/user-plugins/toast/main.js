import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
// Notices queue up, 5 s each; the editor's sample stays up so it can be placed.
const toasts = [];
let toasting = false;
const next = () => {
  const text = toasts.shift();
  if (text === undefined) { toasting = false; return; }
  toasting = true;
  root.replaceChildren(row("toast", "Notice", el("span", "text", text)));
  setTimeout(() => {
    root.replaceChildren();
    setTimeout(next, 300);
  }, 5000);
};
ac.on("toast", (m) => {
  const text = m.data && m.data.text;
  if (ac.mode !== "live") return root.replaceChildren(row("toast", "Notice", el("span", "text", text || "")));
  if (text) toasts.push(text);
  if (!toasting) next();
});
