import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
const query = ac.query;
let docked = [];
let redock = () => {};
  const DOCK = { now: "Now", brb: "BRB", latency: "Lat", chapters: "Chapter", toast: "Notice" };
  const segs = {}; // name -> { d: data, fresh: just arrived, leaving }
  const ticks = [];
  let bar = null, lane = null, pinL = null, pinR = null, flash = null, barKey = "";
  // Segments of the bar that are not overlays. The viewer count is a sample: nothing here
  // knows the real one yet.
  const EXTRA = { viewers: "Viewers", clock: "Time" };
  let viewers = 90 + Math.floor(Math.random() * 60);
  setInterval(() => { viewers = Math.max(1, viewers + Math.round((Math.random() - 0.42) * 4)); }, 2500);
  // Docked overlays the scene hides (their source is switched off): left out. The editor sets
  // `--bar-hidden`; on stream the controller sends the list in a message to the bar.
  let barHidden = [];
  const barOpts = () => ({
    skins: Object.fromEntries(ac.cssList("--seg-skins").map((x) => x.split("="))),
    aligns: Object.fromEntries(ac.cssList("--seg-aligns").map((x) => x.split("="))),
    pins: Object.fromEntries(ac.cssList("--seg-pins").map((x) => x.split("="))),
    extras: ac.cssList("--bar-extras"),
    opacities: Object.fromEntries(ac.cssList("--seg-opacities").map((x) => x.split("="))),
    hidden: ac.cssList("--bar-hidden"),
    order: ac.cssList("--bar-order"),
    animate: ac.cssList("--bar-animate")[0] !== "0",
    marquee: ac.cssList("--bar-marquee")[0] === "1",
    speed: Number(ac.cssList("--bar-speed")[0]) || 60,
  });
  const statusBar = () => {
    if (!bar || !bar.isConnected) {
      bar = el("div", "status");
      const clip = el("div", "track");
      lane = el("div", "belt");
      pinL = el("div", "pin");
      pinR = el("div", "pin");
      clip.append(lane);
      bar.append(pinL, clip, pinR);
      root.replaceChildren(bar);
    }
    return bar;
  };
  // One segment: a label and its value; its `tick` keeps clocks running.
  const makeSeg = (n, d, skin) => {
    const seg = el("span", `seg ${n}`);
    seg.dataset.s = skin;
    const val = el("span", "val");
    seg.append(el("span", "label", DOCK[n] || EXTRA[n]), val);
    const timer = el("span", "timer");
    let tick = null;
    if (n === "viewers") {
      val.classList.add("num");
      tick = () => (val.textContent = viewers.toLocaleString());
    } else if (n === "clock") {
      val.classList.add("num");
      tick = () => (val.textContent = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }));
    } else if (n === "now") {
      val.append(el("span", "text", d.quest || ""), timer);
      if (d.live_since_ms) tick = () => (timer.textContent = clock((Date.now() - d.live_since_ms) / 1000));
    } else if (n === "brb") {
      val.append(el("span", "text", d.text || "Be right back"), timer);
      if (d.since_ms) tick = () => (timer.textContent = clock((Date.now() - d.since_ms) / 1000));
      else timer.textContent = clock(0);
    } else if (n === "latency") {
      val.classList.add("num");
      tick = () => (val.textContent = Math.floor((Date.now() - d.start_ms) / 100));
    } else {
      val.textContent = n === "chapters" ? d.title || "" : d.text || "";
    }
    if (tick) { tick(); ticks.push(tick); }
    return seg;
  };
  // Draws every segment again: in the configured order, once, or as many copies as a
  // seamless ticker needs.
  // How a segment arrives, per skin.
  const ENTER = {
    nvim: [{ opacity: 0, transform: "translateX(-24px)" }, { opacity: 1, transform: "none" }],
    clean: [{ opacity: 0, transform: "scale(0.7)" }, { opacity: 1, transform: "none" }],
    broadcast: [{ opacity: 0, clipPath: "inset(0 100% 0 0)" }, { opacity: 1, clipPath: "inset(0 0 0 0)" }],
    glass: [{ opacity: 0, transform: "translateY(60%)", filter: "blur(6px)" }, { opacity: 1, transform: "none", filter: "none" }],
    minimal: [{ opacity: 0 }, { opacity: 1 }],
  };
  // Draws every segment again. Pinned segments (when the bar scrolls) stand at its ends; the
  // rest sit in left, center and right zones, or scroll as a ticker between the pins.
  const drawStatus = () => {
    const b = statusBar();
    const o = barOpts();
    barKey = JSON.stringify(o);
    const names = Object.keys(segs).filter((n) => ![...o.hidden, ...barHidden].includes(n)).sort((x, y) => {
      const i = (n) => (o.order.includes(n) ? o.order.indexOf(n) : 99);
      return i(x) - i(y);
    });
    const extras = o.extras.filter((n) => EXTRA[n]);
    ticks.length = 0;
    b.classList.toggle("marquee", o.marquee);
    b.classList.toggle("animate", o.animate);
    const pinned = (n) => o.marquee && o.pins[n] === "1";
    const seg = (n) => {
      // A skin thumbnail names the look to show; otherwise each overlay's own.
      const skin = query.has("only") ? query.get("skin") || "nvim" : o.skins[EXTRA[n] ? "now" : n] || "nvim";
      const s = makeSeg(n, segs[n] ? segs[n].d : {}, skin);
      const e = segs[n] || {};
      const op = o.opacities[n];
      if (op !== undefined && op !== "100") s.style.opacity = op / 100;
      if (o.animate && e.leaving) s.animate([{ opacity: 1 }, { opacity: 0, transform: "translateY(60%)" }], { duration: 220, fill: "forwards" });
      else if (o.animate && e.fresh) s.animate(ENTER[skin] || ENTER.nvim, { duration: 380, easing: "cubic-bezier(.2,.8,.2,1)" });
      else if (o.animate && e.changed) s.animate([{ filter: "brightness(2)" }, { filter: "none" }], { duration: 600 });
      return s;
    };
    const zone = (cls, list) => { const z = el("div", cls); z.append(...list.map(seg)); return z; };
    const at = (list, a) => list.filter((n) => (o.aligns[n] || "left") === a);
    pinL.replaceChildren();
    pinR.replaceChildren();
    lane.getAnimations().forEach((x) => x.cancel());
    if (!o.marquee) {
      const g = el("div", "group");
      g.append(zone("zone left", at(names, "left")), zone("zone center", at(names, "center")),
        zone("zone right", [...at(names, "right"), ...extras]));
      lane.replaceChildren(g);
    } else {
      // Pins: left and center ones at the left end, right ones and the extras at the right.
      const pins = names.filter(pinned);
      pinL.append(...pins.filter((n) => (o.aligns[n] || "left") !== "right").map(seg));
      pinR.append(...pins.filter((n) => o.aligns[n] === "right").map(seg), ...extras.map(seg));
      const loose = names.filter((n) => !pinned(n));
      const group = () => { const g = el("div", "group"); g.append(...loose.map(seg)); return g; };
      lane.replaceChildren(group());
      for (const e of Object.values(segs)) e.fresh = e.changed = false;
      if (!loose.length) return;
      // Copies until they cover the lane, then slide by one copy's width forever.
      const w = lane.firstChild.offsetWidth;
      if (!w) return;
      for (let i = 0; i < Math.ceil(lane.parentElement.clientWidth / w) + 1; i++) lane.append(group());
      lane.animate([{ transform: "translateX(0)" }, { transform: `translateX(-${w}px)` }],
        { duration: (w / o.speed) * 1000, iterations: Infinity });
      return;
    }
    for (const e of Object.values(segs)) e.fresh = e.changed = false;
  };
  // Tells the editor when the segments are wider than the bar (without the ticker).
  const reportFit = () => {
    if (!bar) return;
    let need = 0;
    if (!barOpts().marquee) for (const s of bar.querySelectorAll(".seg")) need += s.getBoundingClientRect().width;
    const have = bar.getBoundingClientRect().width;
    ac.send("barfit", { overflow: need > have + 1, need: Math.round(need), have: Math.round(have) });
  };
  const statusPut = (n, action, d) => {
    if (!DOCK[n]) return;
    statusBar();
    const old = segs[n];
    if (action === "hide") {
      if (!old) return;
      if (!barOpts().animate) { delete segs[n]; return drawStatus(); }
      old.leaving = true;
      drawStatus();
      setTimeout(() => { if (segs[n] === old) { delete segs[n]; drawStatus(); } }, 230);
      return;
    }
    const same = old && !old.leaving && JSON.stringify(old.d) === JSON.stringify(d);
    if (same) return;
    segs[n] = { d, fresh: !old || old.leaving, changed: !!old, leaving: false };
    // The editor keeps the toast up so it can be placed; on stream it goes after 5 s.
    if (n === "toast" && ac.mode === "live") {
      clearTimeout(flash);
      flash = setTimeout(() => statusPut("toast", "hide"), 5000);
    }
    drawStatus();
  };
  setInterval(() => ticks.forEach((f) => f()), 250);

// Which overlays are in the bar: from the page's variables (the editor and the host set them); a
// skin thumbnail names one (?only=) or shows the usual four.
const dockedNow = () =>
  query.has("nodock")
    ? (query.get("only") ? [query.get("only")] : ["now", "latency", "chapters", "toast"])
    : ac.cssList("--docked");
const sample = {
  now: { quest: "Writing a parser in Rust", live_since_ms: Date.now() - 3723000 },
  brb: { text: "Be right back", since_ms: null },
  latency: { start_ms: Date.now() },
  chapters: { title: "Tokenizer edge cases" },
  toast: { text: "Thanks for the Super Chat, viewer42!" },
};
const refresh = () => {
  const was = docked.join(" ");
  docked = dockedNow();
  if (docked.join(" ") !== was) redock();
  if (bar && JSON.stringify(barOpts()) !== barKey) drawStatus();
};
redock = () => {
  for (const n of Object.keys(segs)) statusPut(n, "hide");
  for (const n of docked) {
    if (ac.mode !== "live") { if (sample[n]) statusPut(n, "show", sample[n]); continue; }
    const m = ac.state(n).get();
    if (m && m.action === "show") statusPut(n, "show", m.data || {});
  }
};
ac.onTheme(refresh);
docked = dockedNow();
statusBar();
if (ac.mode !== "live") redock();
for (const n of Object.keys(DOCK)) {
  ac.on(n, (m) => { if (ac.mode === "live" && docked.includes(n)) statusPut(n, m.action, m.data || {}); });
}
// On stream the controller sends the list of docked overlays the scene hides.
ac.on("status", (m) => { barHidden = (m.data && m.data.hidden) || []; if (bar) drawStatus(); });
setInterval(reportFit, 800);
drawStatus();
