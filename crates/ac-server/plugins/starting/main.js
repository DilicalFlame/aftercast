import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
let ticker = null;
const tick = (fn, ms) => { clearInterval(ticker); fn(); ticker = setInterval(fn, ms); };
const query = ac.query;
const params = { has: (k) => (k === "demo" ? ac.mode !== "live" : query.has(k)) };
const render = {
    starting(d) {
      let scene = root.querySelector(".starting");
      if (!scene) {
        if (!document.getElementById("starting-fonts")) {
          const fonts = el("link");
          fonts.id = "starting-fonts";
          fonts.rel = "stylesheet";
          fonts.href = "https://fonts.googleapis.com/css2?family=Caveat+Brush&family=Quicksand:wght@600;700&display=swap";
          document.head.append(fonts);
        }
        scene = el("div", "starting");
        const sky = el("div", "sky");
        for (const k of ["b1", "b2", "b3", "b4", "b5", "b6", "p1", "p2", "p3", "d1", "d2", "d3", "d4", "h1", "h2"]) {
          sky.append(el("i", k));
        }
        const who = el("div", "who");
        who.innerHTML = '<svg class="arc" viewBox="0 0 640 640" aria-hidden="true"><path id="arc" d="M 70 330 A 250 250 0 0 1 570 330" fill="none"/>'
          + '<text><textPath href="#arc" startOffset="50%" text-anchor="middle">STREAM</textPath></text></svg>';
        const pic = el("div", "pic");
        const img = el("img");
        img.alt = "";
        img.onerror = () => {
          pic.classList.add("mark");
          pic.innerHTML = '<svg viewBox="0 0 512 512"><path d="M 360.85 182.58 A 128 128 0 1 0 360.85 329.42" fill="none" stroke="currentColor" stroke-width="54" stroke-linecap="round"/><circle cx="256" cy="256" r="38" fill="currentColor"/></svg>';
        };
        img.src = ac.url(`/overlay/assets/${encodeURIComponent(d.avatar || ac.avatar)}`);
        pic.append(img);
        who.append(pic, el("div", "label", "Starting"));
        const count = el("div", "count");
        count.append(el("span", "num"));
        scene.append(sky, who, count, el("div", "sub", "subscribe"));
        root.replaceChildren(scene);
        scene.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 600, easing: "ease-out" });
      }
      const num = scene.querySelector(".num");
      // (The waiting screen, and the editor's sample, stand at 0 until the mode starts them.)
      const since = d.since_ms || (d.idle || params.has("demo") ? null : Date.now());
      let shown = "";
      tick(() => {
        const secs = since ? Math.max(0, Math.floor((Date.now() - since) / 1000)) : 0;
        const h = Math.floor(secs / 3600), m = Math.floor(secs / 60) % 60, s = secs % 60;
        const pad = (n) => String(n).padStart(2, "0");
        const text = h ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
        if (text === shown) return;
        shown = text;
        num.textContent = text;
        num.animate([{ transform: "scale(1.16)", opacity: 0.55 }, { transform: "scale(1)", opacity: 1 }],
          { duration: 420, easing: "cubic-bezier(.2,.8,.2,1)" });
      }, 200);
    },
};
// The waiting screen is drawn at once (its clock at 0) and waits for the stream; it stays drawn
// when the mode ends, only the clock stops. The editor's sample stands at 0 until its mode starts.
render.starting({ avatar: ac.avatar, idle: true });
ac.on("starting", (m) => {
  if (m.action === "hide") { clearInterval(ticker); return; }
  if (ac.mode === "live") render.starting({ ...(m.data || {}), avatar: ac.avatar });
});
ac.hook("timer", (m) => render.starting({ avatar: ac.avatar, since_ms: m.on ? Date.now() : null }));
