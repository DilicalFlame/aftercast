import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
// Live captions: the last final line, then the partial Whisper is still revising.
// Cleared after 4 s without speech.
let idle = null, done = "";
const draw = (d) => {
  clearTimeout(idle);
  const partial = d.final ? "" : d.text || "";
  if (d.final) done = `${done} ${d.text || ""}`.slice(-120);
  const shown = `${done} ${partial}`.trim().slice(-160);
  const card = el("div", "card captions");
  // The words still being revised are dimmer.
  const tail = partial ? shown.slice(-partial.length) : "";
  card.append(el("span", "done", shown.slice(0, shown.length - tail.length)), el("span", "partial", tail));
  root.replaceChildren(card);
  if (ac.mode === "live") idle = setTimeout(() => { done = ""; root.replaceChildren(); }, 4000);
};
ac.on("captions", (m) => {
  if (m.action === "hide") { clearTimeout(idle); done = ""; return root.replaceChildren(); }
  draw(m.data || {});
});
if (ac.mode !== "live" && ac.mode !== "thumb") setInterval(() => ac.state("captions").get() && draw(ac.state("captions").get().data), 3000);
