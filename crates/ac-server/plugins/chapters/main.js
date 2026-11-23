import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
ac.on("chapters", (m) => {
  if (m.action === "hide") return root.replaceChildren();
  root.replaceChildren(row("chapters", "Chapter", el("span", "title", (m.data || {}).title || "")));
});
