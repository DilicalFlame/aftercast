import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
let ticker = null;
const clear = () => { clearInterval(ticker); root.replaceChildren(); };
ac.on("now", (m) => {
  clear();
  if (m.action === "hide") return;
  const d = m.data || {};
  const time = el("span", "timer");
  root.replaceChildren(row("now", "Now", el("span", "quest", d.quest || ""), time));
  if (d.live_since_ms) {
    const f = () => (time.textContent = clock((Date.now() - d.live_since_ms) / 1000));
    f();
    ticker = setInterval(f, 1000);
  }
});
