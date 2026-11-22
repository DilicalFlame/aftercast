// aftercast overlays: one page per OBS browser source (1920x1080, transparent).
// Messages: { overlay, action: "show" | "hide" | "update", data }; overlay "theme" restyles
// every page. `?demo` plays sample content instead (the rehearse page's overlay editor).
(() => {
  const name = document.body.dataset.overlay;
  const root = document.getElementById("root");
  let ticker = null;
  let keysIdle = null;
  let capDone = "";
  let shownQuestion = null;
  let musicCtl = null, brbKey = "", seenPlaylist = null, lastBrb = null;
  let typing = null;

  // The TUI's name palette (golden-angle hues from blue), picked by the name.
  const nameColor = (s) => {
    let h = 0;
    for (const c of s) h = (h * 31 + c.codePointAt(0)) >>> 0;
    return `hsl(${(210 + (h % 360) * 137.507764) % 360} 75% ${[70, 60, 80][h % 3]}%)`;
  };
  // Fades the shown card out, unless something replaced it meanwhile.
  const leave = () => {
    const card = root.firstElementChild;
    shownQuestion = null;
    if (!card) return clear();
    card.animate([{ opacity: 1 }, { opacity: 0, transform: "translateY(16px)" }],
      { duration: 220, easing: "ease-in", fill: "forwards" });
    setTimeout(() => { if (root.firstElementChild === card) clear(); }, 240);
  };

  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  };
  // A small card: a label (a float title, a pill or a block, per skin) beside its content.
  const row = (cls, label, ...kids) => {
    const c = el("div", `card row ${cls}`);
    c.append(el("span", "label", label), ...kids);
    return c;
  };
  const clock = (secs) => {
    secs = Math.max(0, Math.floor(secs));
    const h = Math.floor(secs / 3600), m = Math.floor(secs / 60) % 60, s = secs % 60;
    const pad = (n) => String(n).padStart(2, "0");
    return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
  };
  const tick = (fn, ms) => {
    clearInterval(ticker);
    fn();
    ticker = setInterval(fn, ms);
  };
  const clear = () => {
    clearInterval(ticker);
    if (musicCtl) musicCtl.stop();
    musicCtl = null;
    brbKey = "";
    root.replaceChildren();
  };

  // Toasts: a queue, 5 s each.
  const toasts = [];
  let toasting = false;
  const nextToast = () => {
    const text = toasts.shift();
    if (text === undefined) {
      toasting = false;
      return;
    }
    toasting = true;
    root.replaceChildren(row("toast", "Notice", el("span", "text", text)));
    setTimeout(() => {
      root.replaceChildren();
      setTimeout(nextToast, 300);
    }, 5000);
  };

  // Keys: bubbles flowing upward in a frame (--w by --h, bottom center at the anchor,
  // lined up by --align), newest at the bottom; a bubble rising past the frame's top fades.
  // Letters typed in one burst share a bubble (with a block cursor while it grows), spaces
  // shown as ␣; shortcuts and keys that type nothing get their own, in Vim notation
  // (<C-s>, <Esc>, <CR>; D is the Windows key, as Vim writes Super/Cmd). Text wider than
  // the frame wraps like an editor: the word being typed moves to a new bubble.
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
  if (name === "keys") frame();
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

  // Behaviour set per overlay in the editor. `--docked` (on every page): the overlays shown
  // in the status bar instead of their own frame. `--hide` (this overlay): hidden while any
  // of these is on screen. A page only hears the messages, so "on screen" is: shown and not
  // hidden since, or for overlays that never send a hide (toast, captions, keys) lately.
  const list = (v) => {
    const t = getComputedStyle(document.body).getPropertyValue(v).trim();
    return !t || t === "none" ? [] : t.split(/\s+/);
  };
  let docked = [], rules = [], redock = () => {};
  const active = {};
  const LATELY = { toast: 5500, captions: 4500, keys: 3000, alert: 7000 };
  const track = (m) => {
    if (m.action === "hide") delete active[m.overlay];
    else active[m.overlay] = LATELY[m.overlay] ? Date.now() + LATELY[m.overlay] : Infinity;
  };
  setInterval(() => root.classList.toggle("suppressed", rules.some((o) => (active[o] || 0) > Date.now())), 200);
  const params = new URLSearchParams(location.search);
  // The editor lets only the selected overlay play its sound.
  let soundDemo = false;
  // The BRB plays the folder sounds/brb; its Play sound switch turns the whole playlist off
  // (and with it the cover art and the bars). Changing shuffle starts the playlist over.
  const brbMusic = () => (list("--mute")[0] === "1" ? "" : "on") + list("--shuffle")[0];
  const refresh = () => {
    const was = docked.join(" ");
    const wasDocked = docked.includes(name);
    // A skin thumbnail shows the overlay alone (the status bar with sample segments).
    docked = params.has("nodock")
      ? name === "status" ? (params.get("only") ? [params.get("only")] : ["now", "latency", "chapters", "toast"]) : []
      : list("--docked");
    rules = params.has("nodock") ? [] : list("--hide");
    if (name !== "status" && docked.includes(name)) clear();
    // Undocked in the editor: start over so the sample plays again.
    if (wasDocked && !docked.includes(name) && params.has("demo")) return location.reload();
    if (docked.join(" ") !== was) redock();
    if (name === "status" && bar && JSON.stringify(barOpts()) !== barKey) drawStatus();
    // Sound is on unless switched off (everywhere, or for this overlay); the editor's samples
    // are silent until the overlay is selected or a Play button asks.
    const loud = list("--sound")[0] !== "0" && list("--mute")[0] !== "1" && (!params.has("demo") || soundDemo) && !params.has("nodock");
    const volume = (Number(list("--volume")[0]) || 0) / 100;
    if (window.aftercastSfx) {
      window.aftercastSfx.useSkin(list("--sound-skin")[0]);
      window.aftercastSfx.useFile(list("--sound-file")[0]);
      window.aftercastSfx.volume(volume);
      window.aftercastSfx.enabled(loud);
    }
    if (musicCtl) {
      musicCtl.setVolume(volume);
      musicCtl.setPlaying(loud && (params.has("demo") || name !== "brb" || (brbCard && brbCard.live)));
    }
    // The playlist switched off or shuffled differently while the BRB shows: start over.
    if (name === "brb") {
      const pl = brbMusic();
      const changed = seenPlaylist !== null && pl !== seenPlaylist;
      seenPlaylist = pl;
      if (changed && lastBrb && root.firstElementChild) {
        brbKey = "";
        render.brb(lastBrb);
      }
    }
  };
  window.aftercastRefresh = refresh;
  // The editor's samples of the BRB and starting screens are always there; their clocks start
  // when the editor goes into that mode (true) and are back to 0 when it leaves (false).
  let demoSamples = null, timerOn = null, brbCard = null;
  window.aftercastTimer = (on) => {
    if (!params.has("demo") || !demoSamples || on === timerOn || !(name === "brb" || name === "starting")) return;
    timerOn = on;
    const d = demoSamples[name];
    d.since_ms = on ? Date.now() : null;
    clear();
    brbKey = "";
    render[name](d);
  };
  // The editor changed the BRB screen's options: draw it again.
  // Parts the author moved by hand stand where they were put (top left in the frame).
  let lastScr = {};
  const applyFree = (card, pos) => {
    for (const e of card.querySelectorAll("[data-el]")) {
      const x = pos[`${e.dataset.el}_x`], y = pos[`${e.dataset.el}_y`];
      const free = x != null && y != null;
      e.classList.toggle("free", free);
      e.style.left = free ? x + "px" : "";
      e.style.top = free ? y + "px" : "";
    }
    const side = (sel, v) => {
      const e = card.querySelector(sel);
      if (e && v) e.style.width = e.style.height = Math.min(800, Math.max(80, Number(v))) + "px";
    };
    const music = card.querySelector(".music");
    if (music) music.style.width = music.classList.contains("free") && pos.music_w ? Math.min(1800, Math.max(300, Number(pos.music_w))) + "px" : "";
    side(".pfp", pos.pfp_size);
    side(".qr", pos.qr_size);
  };
  window.aftercastBrbPos = (pos) => {
    lastScr = { ...lastScr, ...pos };
    const card = root.querySelector(".card.brb");
    if (card) applyFree(card, lastScr);
  };
  window.aftercastBrbReload = () => { brbKey = ""; if (lastBrb) render.brb(lastBrb); };

  // Status bar: a thin bar the docked overlays (now, brb, latency, chapters, toast) share as
  // segments, each a label and a value. Its height is its frame's height; text follows it.
  // Options (`[overlay.bar]`): the order of the segments, whether they animate, and whether
  // they scroll left forever like a ticker.
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
    skins: Object.fromEntries(list("--seg-skins").map((x) => x.split("="))),
    aligns: Object.fromEntries(list("--seg-aligns").map((x) => x.split("="))),
    pins: Object.fromEntries(list("--seg-pins").map((x) => x.split("="))),
    extras: list("--bar-extras"),
    opacities: Object.fromEntries(list("--seg-opacities").map((x) => x.split("="))),
    hidden: list("--bar-hidden"),
    order: list("--bar-order"),
    animate: list("--bar-animate")[0] !== "0",
    marquee: list("--bar-marquee")[0] === "1",
    speed: Number(list("--bar-speed")[0]) || 60,
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
      const skin = params.has("only") ? params.get("skin") || "nvim" : o.skins[EXTRA[n] ? "now" : n] || "nvim";
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
    if (!bar || window.parent === window) return;
    let need = 0;
    if (!barOpts().marquee) for (const s of bar.querySelectorAll(".seg")) need += s.getBoundingClientRect().width;
    const have = bar.getBoundingClientRect().width;
    window.parent.aftercastBarFit?.({ overflow: need > have + 1, need: Math.round(need), have: Math.round(have) });
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
    if (n === "toast" && !params.has("demo")) {
      clearTimeout(flash);
      flash = setTimeout(() => statusPut("toast", "hide"), 5000);
    }
    drawStatus();
  };
  setInterval(() => ticks.forEach((f) => f()), 250);

  // Super Chat alerts: one at a time, each an animation in the frame (see alert.js).
  const ALERT_SKINS = ["dino", "pacman", "moonwalk", "coins", "neon"];
  const alerts = [];
  let alerting = false;
  const nextAlert = async (force) => {
    const d = alerts.shift();
    if (!d) { alerting = false; return; }
    alerting = true;
    const skin = ALERT_SKINS.includes(document.body.dataset.skin) ? document.body.dataset.skin : "dino";
    const host = el("div", "alert");
    root.replaceChildren(host);
    if (force) window.aftercastSfx.enabled(true);
    window.aftercastSfx.alert(skin);
    await window.aftercastAlert.play(host, d, skin);
    if (params.has("demo") && !soundDemo) window.aftercastSfx.enabled(false);
    root.replaceChildren();
    setTimeout(() => nextAlert(), 400);
  };
  // The editor's Play buttons: the alert at once, with sound, whatever the settings.
  window.aftercastPlay = (what) => {
    window.aftercastSfx.unlock();
    window.aftercastSfx.volume(Number(what.volume) / 100 || 0.7);
    if (what.sound) {
      window.aftercastSfx.enabled(true);
      return window.aftercastSfx.alert(ALERT_SKINS.includes(document.body.dataset.skin) ? document.body.dataset.skin : "dino");
    }
    alerts.length = 0;
    alerts.push(SAMPLE_ALERT);
    if (!alerting) nextAlert(true);
  };
  const SAMPLE_ALERT = { author: "viewer42", amount: "\u20b9500.00", text: "Keep the streams coming!" };

  const render = {
    alert(d) {
      alerts.push(d);
      if (!alerting) nextAlert();
    },
    now(d) {
      const quest = el("span", "quest", d.quest || "");
      const time = el("span", "timer");
      const card = row("now", "Now", quest, time);
      root.replaceChildren(card);
      if (d.live_since_ms) tick(() => (time.textContent = clock((Date.now() - d.live_since_ms) / 1000)), 1000);
    },
    // Shown again with the same question (state is resent often): left as it is.
    question(d) {
      const key = `${d.author}\u0000${d.text}`;
      if (shownQuestion === key && root.firstElementChild) return;
      shownQuestion = key;
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
    },
    // Be right back: the title and the time away, the playlist playing (the track's album card,
    // its name and bars that move with the sound) and the UPI QR code. A playlist or a UPI id
    // that is not there leaves its half out.
    brb(d) {
      // Shown again with the same start (state is resent): left as it is.
      lastBrb = d;
      const key = String(d.since_ms || "");
      if (brbKey === key && root.firstElementChild) return;
      brbKey = key;
      // The screen is always there, waiting (its clock at 0, or stopped from last time): this
      // only starts the clock, so nothing flashes as the scene changes in.
      if (brbCard && brbCard.time.isConnected && d.since_ms && brbCard.text === (d.text || "Be right back")) {
        brbCard.live = true;
        tick(() => (brbCard.time.textContent = clock((Date.now() - d.since_ms) / 1000)), 1000);
        return refresh();
      }
      // The screen's options (picture, QR, texts) come from the config.
      fetch(`/overlay/brb.json?skin=${encodeURIComponent(document.body.dataset.skin || "")}`, { cache: "no-store" }).then((r) => r.json()).catch(() => ({ screen: {} }))
        .then((o) => { if (brbKey === key && lastBrb === d) render.brbScreen(d, o.screen || {}, o.avatar || "logo.jpg"); });
    },
    brbScreen(d, scr, avatar) {
      lastScr = scr;
      const px = (v, lo, hi, dflt) => Math.min(hi, Math.max(lo, Number(v) || dflt)) + "px";
      clear();
      const card = el("div", "card brb");
      // The backdrop is the starting screen's: its sky, in the skin worn.
      const scene = el("div", "starting bgscene");
      const sky = el("div", "sky");
      for (const c of ["b1", "b2", "b3", "b4", "b5", "b6", "p1", "p2", "p3", "d1", "d2", "d3", "d4", "h1", "h2"]) sky.append(el("i", c));
      scene.append(sky);
      card.append(scene);
      const time = el("div", "timer");
      const top = el("div", "brb-top");
      top.append(el("div", "title", d.text || "Be right back"), time);
      top.dataset.el = "title";
      const lines = el("div", "brb-text");
      lines.dataset.el = "text";
      for (const t of scr.text || []) lines.append(el("div", "line", t));
      const main = el("div", "brb-main");
      if (scr.pfp !== false) {
        const pic = el("img", `pfp ${scr.pfp_shape || "circle"}`);
        pic.dataset.el = "pfp";
        pic.alt = "";
        pic.style.width = pic.style.height = px(scr.pfp_size, 80, 700, 260);
        pic.onerror = () => pic.remove();
        pic.src = `/overlay/assets/${encodeURIComponent(avatar)}`;
        main.append(pic);
      }
      const playlist = brbMusic() ? "brb" : "";
      let parts = null;
      if (playlist) {
        const cover = el("div", "cover");
        cover.append(el("img"));
        const track = el("div", "track");
        parts = { cover, title: track, artist: el("div", "artist"), album: el("div", "album"), bars: el("canvas", "bars") };
        const words = el("div", "words");
        words.append(el("div", "now", "Now playing"), track, parts.artist, parts.album);
        const meta = el("div", "meta");
        meta.append(words, parts.bars);
        const music = el("div", "music");
        music.dataset.el = "music";
        music.append(cover, meta);
        main.append(music);
      }
      if (scr.qr !== false) {
        const pay = el("div", "pay");
        pay.dataset.el = "qr";
        const qr = el("img", "qr");
        qr.alt = "";
        qr.style.width = qr.style.height = px(scr.qr_size, 120, 800, 420);
        qr.onerror = () => pay.remove();
        qr.src = `/overlay/upi.svg?v=${Date.now()}`;
        pay.append(qr);
        if (scr.qr_caption) pay.append(el("div", "note", scr.qr_caption));
        main.append(pay);
      }
      card.append(top);
      if (lines.children.length) card.append(lines);
      card.append(main);
      applyFree(card, scr);
      root.replaceChildren(card);
      if (parts) {
        musicCtl = window.aftercastMusic.mount(parts, playlist, list("--shuffle")[0] !== "0");
        refresh();
      }
      brbCard = { text: d.text || "Be right back", time, live: !!d.since_ms };
      if (d.since_ms) tick(() => (time.textContent = clock((Date.now() - d.since_ms) / 1000)), 1000);
      else time.textContent = clock(0);
      refresh();
    },
    // The starting screen, after the channel's starting video: a purple sky of drifting bars,
    // the picture under an arc of "STREAM", "Starting" in brush script, and a clock counting
    // up since the scene came on (the video counted down) that pops every second.
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
        img.src = `/overlay/assets/${encodeURIComponent(d.avatar || "logo.jpg")}`;
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
    latency(d) {
      const num = el("span", "num");
      root.replaceChildren(row("latency", "Lat", num));
      tick(() => (num.textContent = Math.floor((Date.now() - d.start_ms) / 100)), 50);
    },
    chapters(d) {
      root.replaceChildren(row("chapters", "Chapter", el("span", "title", d.title || "")));
    },
    // Password boxes and denied apps never reach the overlay.
    keys(d) {
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
    },
    // Live captions: the last final line, then the partial Whisper is still revising.
    // Cleared after 4 s without speech.
    captions(d) {
      clearTimeout(keysIdle);
      const partial = d.final ? "" : d.text || "";
      if (d.final) capDone = `${capDone} ${d.text || ""}`.slice(-120);
      const shown = `${capDone} ${partial}`.trim().slice(-160);
      const card = el("div", "card captions");
      // The words still being revised are dimmer.
      const tail = partial ? shown.slice(-partial.length) : "";
      card.append(el("span", "done", shown.slice(0, shown.length - tail.length)), el("span", "partial", tail));
      root.replaceChildren(card);
      keysIdle = setTimeout(() => {
        capDone = "";
        clear();
      }, 4000);
    },
  };

  // The BRB and starting screens live in scenes of their own, so they stay drawn when their mode
  // ends: the scene change (a stinger, say) takes them away, and nothing underneath shows
  // through meanwhile. Only the clock stops (the music a moment later).
  const rest = () => {
    clearInterval(ticker);
    brbKey = "";
    if (brbCard) {
      brbCard.live = false;
      setTimeout(() => { if (!brbCard.live) refresh(); }, 2500);
    }
  };
  const handle = (msg) => {
    if (msg.overlay === "theme") {
      document.getElementById("theme").textContent = msg.data.css;
      refresh();
      return skin();
    }
    track(msg);
    if (name === "status" && msg.overlay === "status") {
      barHidden = (msg.data && msg.data.hidden) || [];
      if (bar) drawStatus();
      return;
    }
    if (name === "status") {
      if (docked.includes(msg.overlay)) statusPut(msg.overlay, msg.action, msg.data || {});
      return;
    }
    if (msg.overlay !== name || docked.includes(name)) return;
    if (name === "toast") {
      if (msg.data && msg.data.text) toasts.push(msg.data.text);
      if (!toasting) nextToast();
      return;
    }
    if (msg.action === "hide" && (name === "brb" || name === "starting")) return rest();
    if (msg.action === "hide") return name === "question" ? leave() : clear();
    const draw = render[name];
    if (draw) draw(msg.data || {});
  };

  // Sample content for the overlay editor, looping.
  const demo = () => {
    const ago = (s) => Date.now() - s * 1000;
    const samples = demoSamples = {
      now: { quest: "Writing a parser in Rust", live_since_ms: ago(3723) },
      question: [
        { author: "viewer42", text: "Why Neovim over VS Code? And which plugin manager do you use?" },
        { author: "ana_codes", text: "How do you test the tokenizer?", support: { label: "₹500.00" } },
      ],
      // Their clocks stand at 0 until the editor goes into their mode (see aftercastTimer).
      brb: { text: "Be right back", since_ms: null },
      starting: { since_ms: null, avatar: "logo.jpg" },
      latency: { start_ms: Date.now() },
      chapters: { title: "Tokenizer edge cases" },
      captions: { text: "so this is where the parser gets interesting", final: false },
    };
    if (name === "status") {
      statusBar();
      redock = () => {
        for (const n of Object.keys(segs)) statusPut(n, "hide");
        for (const n of docked) if (samples[n]) statusPut(n, "show", samples[n]);
        if (docked.includes("toast")) statusPut("toast", "show", { text: "Thanks for the Super Chat, viewer42!" });
      };
      redock();
      setInterval(reportFit, 800);
      return drawStatus();
    }
    if (docked.includes(name)) return;
    if (name === "alert") {
      // A skin thumbnail: one still from the middle of the animation.
      if (params.has("nodock")) {
        // After the editor has set this page's frame size.
        return setTimeout(() => {
          const host = el("div", "alert");
          root.replaceChildren(host);
          const skin = document.body.dataset.skin;
          window.aftercastAlert.still(host, SAMPLE_ALERT, ALERT_SKINS.includes(skin) ? skin : "dino", 3.3);
        }, 300);
      }
      const loop = () => { alerts.push(SAMPLE_ALERT); if (!alerting) nextAlert(); setTimeout(loop, 11000); };
      return setTimeout(loop, 500);
    }
    if (name === "toast") return root.replaceChildren(row("toast", "Notice", el("span", "text", "Thanks for the Super Chat, viewer42!")));
    if (name === "question") {
      let i = 0;
      const next = () => {
        render.question(samples.question[i++ % samples.question.length]);
        setTimeout(leave, 5200);
        setTimeout(next, 5800);
      };
      return next();
    }
    if (name !== "keys") {
      render[name](samples[name]);
      if (name === "captions") setInterval(() => render.captions(samples.captions), 3000);
      return;
    }
    const steps = ["c", "i", "w", 200, "Ctrl+S", "Esc", 400, ":", "w", "Enter", 500, "Ctrl+d", 300, "d", "d", "p", 600,
      "Ctrl+Shift+p", 400, "f", "n", " ", "m", "a", "i", "n", 900];
    let i = 0;
    const next = () => {
      const s = steps[i++ % steps.length];
      if (typeof s === "number") return setTimeout(next, s);
      if (s.length > 1) render.keys({ combo: s, ch: s === "Enter" ? "\n" : undefined });
      else render.keys({ ch: s });
      setTimeout(next, 110);
    };
    next();
  };

  const connect = () => {
    const ws = new WebSocket(`ws://${location.host}/ws`);
    ws.onmessage = (e) => handle(JSON.parse(e.data));
    // aftercast restarted or not running yet: keep trying. The server resends state.
    ws.onclose = () => setTimeout(connect, 1000);
  };
  // The skin: `?skin=` (editor thumbnails), else the theme's --skin. A custom skin's
  // stylesheet loads from <home>/overlays/skins/<overlay>/; a built-in one is empty there.
  const skin = () => {
    const s = new URLSearchParams(location.search).get("skin")
      || getComputedStyle(document.body).getPropertyValue("--skin").trim() || "nvim";
    // Each skin has its own arrangement of the BRB screen.
    if (name === "brb" && document.body.dataset.skin && document.body.dataset.skin !== s) {
      setTimeout(() => window.aftercastBrbReload?.());
    }
    document.body.dataset.skin = s;
    const href = `/overlay/skins/${name}/${s}.css`;
    const link = document.getElementById("skin");
    if (link.getAttribute("href") !== href) link.setAttribute("href", href);
  };
  window.aftercastLive = (on) => {
    if (on === soundDemo) return;
    soundDemo = on;
    if (on) window.aftercastSfx.unlock();
    refresh();
  };
  window.aftercastSkin = skin;
  skin();
  refresh();
  if (new URLSearchParams(location.search).has("demo")) demo();
  else {
    // The BRB and starting screens are drawn at once, clocks at 0, and wait for their mode.
    if (!docked.includes(name)) {
      if (name === "brb") render.brb({ text: "Be right back" });
      if (name === "starting") {
        fetch("/overlay/brb.json").then((r) => r.json()).catch(() => ({}))
          .then((o) => render.starting({ avatar: o.avatar || "logo.jpg", idle: true }));
      }
    }
    connect();
  }
})();
