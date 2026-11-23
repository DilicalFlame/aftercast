import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
let shown = null, typing = null;
// Fades the shown card out, unless something replaced it meanwhile.
const leave = () => {
  const card = root.firstElementChild;
  shown = null;
  if (!card) return;
  card.animate([{ opacity: 1 }, { opacity: 0, transform: "translateY(16px)" }], { duration: 220, easing: "ease-in", fill: "forwards" });
  setTimeout(() => { if (root.firstElementChild === card) root.replaceChildren(); }, 240);
};
const draw = (d) => {
    const key = `${d.author}\u0000${d.text}`;
    if (shown === key && root.firstElementChild) return;
    shown = key;
    clearTimeout(typing);
    // One markup for every skin: a head (avatar, name, "asks", tag) and the text line.
    const card = el("div", "card question");
    const author = d.author || "";
    card.style.setProperty("--name", nameColor(author));
    const title = el("div", "title");
    title.append(el("span", "avatar", [...author][0] || "?"), el("span", "who", author), el("span", "asks", " asks"));
    const support = d.support && d.support.label;
    if (support) card.classList.add("support");
    const head = el("div", "head");
    head.append(title, el("div", "tag", support || "!q"));
    const text = el("span", "text");
    const line = el("div", "line");
    line.append(el("span", "prompt", "❯ "), text);
    card.append(head, line);
    root.replaceChildren(card);
    const from = document.body.dataset.skin === "broadcast" ? "translateX(-48px)" : "translateY(24px)";
    card.animate([{ opacity: 0, transform: from }, { opacity: 1, transform: "none" }],
      { duration: 280, easing: "cubic-bezier(.2,.8,.2,1)" });
    // Types itself in: about a second whatever the length.
    const chars = [...(d.text || "")];
    const step = Math.min(35, Math.max(8, 1100 / Math.max(1, chars.length)));
    let n = 0;
    const type = () => {
      n = Math.min(chars.length, n + Math.max(1, Math.round(16 / step)));
      text.textContent = chars.slice(0, n).join("");
      if (n < chars.length) typing = setTimeout(type, step);
    };
    type();
};
ac.on("question", (m) => {
  if (m.action === "hide") return leave();
  draw(m.data || {});
});
