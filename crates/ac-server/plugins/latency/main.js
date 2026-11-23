import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
let ticker = null;
ac.on("latency", (m) => {
  clearInterval(ticker);
  if (m.action === "hide") return root.replaceChildren();
  const d = m.data || {};
  const num = el("span", "num");
  root.replaceChildren(row("latency", "Lat", num));
  const f = () => (num.textContent = Math.floor((Date.now() - d.start_ms) / 100));
  f();
  ticker = setInterval(f, 50);
});
