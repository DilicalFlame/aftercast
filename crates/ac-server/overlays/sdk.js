// aftercast overlay SDK, API v1 (specs/2026-11-23-overlay-plugins.md, section 6).
//
//   import ac from "/sdk/aftercast.js";
//   ac.params.subscribe((p) => ...);          // settings from the plugin's [[param]]s
//   ac.state("brb").subscribe((m) => ...);    // { action, data } | null
//   ac.on("question", (m) => ...);            // every message of a stream
//
// Stores follow the Svelte store contract: subscribe(fn) calls fn at once with the current
// value and returns an unsubscribe, so `$params` works in Svelte (`const params = ac.params`) and plain code can
// just subscribe. No dependencies.

const boot = (() => {
  try { return JSON.parse(document.getElementById("ac-boot").textContent); } catch { return {}; }
})();
const query = new URLSearchParams(location.search);
const demo = query.has("demo");
const thumb = demo && query.has("nodock");
const mode = thumb ? "thumb" : demo ? "demo" : "live";

// A store whose value only the SDK sets.
const store = (initial) => {
  let value = initial;
  const subs = new Set();
  return {
    get: () => value,
    subscribe(fn) { subs.add(fn); fn(value); return () => subs.delete(fn); },
    _set(next) {
      if (JSON.stringify(next) === JSON.stringify(value)) return;
      value = next;
      for (const fn of [...subs]) fn(value);
    },
  };
};
const view = (s) => ({ get: s.get, subscribe: s.subscribe });

const rootStyle = () => getComputedStyle(document.documentElement);
const bodyStyle = () => getComputedStyle(document.body);
const cssVar = (style, name, fallback = "") => style.getPropertyValue(name).trim() || fallback;

const paramsStore = store(boot.params || {});
const skinStore = store("");
const themeStore = store({});
const frameStore = store({ width: 1920, height: 1080, scale: 1 });
const volumeStore = store((boot.volume ?? 70) / 100);
// Whether sound may play: yes live; a sample (the editor) is silent until it is asked to.
const soundStore = store(mode === "live");
const skins = boot.skins || [];
const partIds = boot.parts || [];
// Positions of the movable parts the user has set, by skin then part: { nvim: { title: { x, y } } }.
let savedParts = boot.part_positions || {};
// What the editor is showing while a part is dragged (not saved yet).
let liveParts = null;
const partsStore = store({});
const dockedStore = store(false);
const suppressedStore = store(false);
const themeListeners = new Set();
const hooks = {};

// The skin in effect: ?skin=, else the plugin's own, else the pack (the theme's --skin); one
// the plugin does not have falls back to its first.
const pickSkin = () => {
  let s = query.get("skin") || cssVar(bodyStyle(), "--skin", "");
  if (skins.length && !skins.includes(s)) s = skins[0];
  document.body.dataset.skin = s;
  skinStore._set(s);
  // A custom skin is a stylesheet in <home>/overlays/skins/<id>/<skin>.css (built-in skins are
  // part of the plugin's own style sheet).
  if ((boot.custom_skins || []).includes(s)) {
    if (!skinLink) {
      skinLink = document.createElement("link");
      skinLink.rel = "stylesheet";
      document.head.append(skinLink);
    }
    const href = `/overlay/skins/${boot.id}/${s}.css`;
    if (skinLink.getAttribute("href") !== href) skinLink.setAttribute("href", href);
  } else if (skinLink) skinLink.removeAttribute("href");
};
let skinLink = null;
// ---- movable parts: any element with data-part="<id>" listed under [[part]] in the manifest ----
const frameEl = () => document.getElementById("ac-frame");
// Where an element is in the frame's own (unscaled) pixels.
const offsetIn = (el, frame) => {
  let x = 0, y = 0, e = el;
  while (e && e !== frame) { x += e.offsetLeft; y += e.offsetTop; e = e.offsetParent; }
  return { x, y, w: el.offsetWidth, h: el.offsetHeight };
};
const partEls = () => partIds.map((id) => [id, document.querySelector(`[data-part="${id}"]`)]).filter(([, e]) => e);
const positionsNow = () => ({ ...(savedParts[skinStore.get()] || {}), ...(liveParts || {}) });
let placing = false;
function placeParts() {
  const frame = frameEl();
  if (!frame || placing) return;
  placing = true;
  const pos = positionsNow();
  for (const [id, el] of partEls()) {
    const p = pos[id];
    if (!p || p.x == null || p.y == null) {
      // Back in the plugin's own layout.
      if (el.dataset.acFree) {
        for (const k of ["position", "left", "top", "margin", "gridArea", "width"]) el.style[k] = "";
        delete el.dataset.acFree;
      }
      continue;
    }
    el.dataset.acFree = "1";
    Object.assign(el.style, { position: "absolute", margin: "0", gridArea: "auto", left: `${p.x}px`, top: `${p.y}px` });
    if (p.w) el.style.width = `${p.w}px`;
    // A positioned ancestor of the part would shift it: put it where it was asked to be.
    const at = offsetIn(el, frame);
    if (at.x !== p.x) el.style.left = `${p.x - (at.x - p.x)}px`;
    if (at.y !== p.y) el.style.top = `${p.y - (at.y - p.y)}px`;
  }
  partsStore._set(pos);
  placing = false;
}
let placeQueued = false;
const schedulePlace = () => {
  if (placeQueued || !partIds.length) return;
  placeQueued = true;
  requestAnimationFrame(() => { placeQueued = false; placeParts(); });
};
// Every part's place as it is now, for the editor's first drag (so moving one leaves the others).
const measureParts = () => {
  const frame = frameEl();
  const out = {};
  if (!frame) return out;
  // A part that is not shown (no width, no height) has no place to keep.
  for (const [id, el] of partEls()) if (el.offsetWidth || el.offsetHeight) out[id] = offsetIn(el, frame);
  return out;
};

const cssList = (name) => {
  const t = cssVar(bodyStyle(), name, "");
  return !t || t === "none" ? [] : t.split(/\s+/);
};
const readTheme = () => {
  const r = rootStyle(), b = bodyStyle();
  // In the status bar instead of its own frame (a thumbnail shows the overlay alone).
  const docked = !thumb && boot.id !== "status" && cssList("--docked").includes(boot.id);
  dockedStore._set(docked);
  document.documentElement.classList.toggle("ac-docked", docked);
  themeStore._set({
    bg: cssVar(r, "--bg"), accent: cssVar(r, "--accent"), fg: cssVar(r, "--fg"), muted: cssVar(r, "--muted"),
    fontMono: cssVar(r, "--font-mono"), fontSans: cssVar(r, "--font-sans"),
  });
  frameStore._set({
    width: parseFloat(cssVar(b, "--w", "1920")), height: parseFloat(cssVar(b, "--h", "1080")),
    scale: parseFloat(cssVar(b, "--s", "1")),
  });
  const v = parseFloat(cssVar(r, "--volume", ""));
  if (Number.isFinite(v)) volumeStore._set(v / 100);
  pickSkin();
  schedulePlace();
  for (const fn of [...themeListeners]) fn();
};

// ---- the streams ----
const last = {};
const stores = {};
const listeners = {};
const stateStore = (name) => (stores[name] ||= store(last[name] || null));
const dispatch = (msg) => {
  if (msg.action !== "update") {
    last[msg.overlay] = msg;
    stateStore(msg.overlay)._set(msg);
  }
  for (const fn of [...(listeners[msg.overlay] || [])]) {
    try { fn(msg); } catch (e) { console.error(e); }
  }
};

// "$now", "$now-120000", "$now+5000" in a scenario become a time.
const fill = (v) => {
  if (typeof v === "string") {
    const m = /^\$now([+-]\d+)?$/.exec(v);
    return m ? Date.now() + Number(m[1] || 0) : v;
  }
  if (Array.isArray(v)) return v.map(fill);
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fill(x)]));
  return v;
};
const scenarios = boot.scenarios || [];
const applyScenario = (name) => {
  const sc = scenarios.find((s) => s.name === name) || scenarios[0];
  if (!sc) return false;
  for (const [overlay, st] of Object.entries(sc.state || {})) {
    dispatch({ overlay, action: st.action || "show", data: fill(st.data || {}) });
  }
  return true;
};

// ---- the host's messages ----
const setTheme = (data) => {
  const style = document.getElementById("ac-theme");
  if (style && typeof data.css === "string") {
    // A theme sent before this plugin was installed has no frame for it: keep the one we have.
    const rule = `[data-overlay="${boot.id}"]`;
    const own = style.textContent.split("\n").find((l) => l.startsWith(rule));
    style.textContent = data.css.includes(rule) || !own ? data.css : `${data.css}\n${own}`;
  }
  const mine = data.plugins && data.plugins[boot.id];
  if (mine) {
    paramsStore._set(mine.params || {});
    if (mine.parts) {
      savedParts = mine.parts;
      liveParts = null;
    }
  }
  readTheme();
};
const handle = (msg) => {
  if (msg.overlay === "__suppress") {
    // Hidden while an overlay it is set to hide for is on screen (not in the editor's samples).
    const on = mode === "live" && ((msg.data && msg.data.names) || []).includes(boot.id);
    suppressedStore._set(on);
    frameEl()?.classList.toggle("suppressed", on);
    return;
  }
  if (msg.overlay === "theme") return setTheme(msg.data || {});
  if (msg.overlay === "__plugin") { if (msg.data && msg.data.id === boot.id) location.reload(); return; }
  // A sample (the editor) is not driven by the stream, only by its theme and params.
  if (mode === "live") dispatch(msg);
};

let readyFns = [], isReady = false;
const becomeReady = () => {
  if (isReady) return;
  isReady = true;
  for (const fn of readyFns.splice(0)) fn();
};

let socket, retry = 500;
const connect = () => {
  socket = new WebSocket(`ws://${location.host}/ws?plugin=${encodeURIComponent(boot.id)}`);
  socket.onopen = () => { retry = 500; setTimeout(becomeReady, 120); };
  socket.onmessage = (e) => { try { handle(JSON.parse(e.data)); } catch (err) { console.error(err); } };
  socket.onclose = () => { setTimeout(connect, retry); retry = Math.min(retry * 2, 8000); };
  socket.onerror = () => socket.close();
};

const ac = {
  id: boot.id,
  api: boot.api,
  name: boot.name,
  version: boot.version,
  mode,
  features: ["state", "params", "scenarios", "hot-reload", "sound-gate", "avatar", "parts", "sandbox", "docked", "suppressed", "hooks"],
  params: view(paramsStore),
  skin: view(skinStore),
  theme: view(themeStore),
  frame: view(frameStore),
  volume: view(volumeStore),
  /** In the status bar instead of its own frame (the page is then empty). */
  docked: view(dockedStore),
  /** Hidden for now because an overlay it is set to hide for is on screen. */
  suppressed: view(suppressedStore),
  /** The URL's query (?demo, ?nodock, ?only=...). */
  query,
  /** A CSS variable of the page (the editor and the host set some), as a string. */
  css: (name) => cssVar(bodyStyle(), name, ""),
  /** A CSS variable holding a space separated list; "none" is the empty list. */
  cssList,
  /** Runs fn after the theme or the editor's variables change. */
  onTheme(fn) { themeListeners.add(fn); return () => themeListeners.delete(fn); },
  /** Answers an editor message (see the bridge): hook("play", (message) => ...). */
  hook(op, fn) { hooks[op] = fn; },
  /** Tells the editor something: send("barfit", { overflow: true }). */
  send(op, data = {}) { if (window.parent !== window) parent.postMessage({ aftercast: op, id: boot.id, ...data }, "*"); },
  /** true while sound may play (live, or a sample the editor switched on). */
  sound: view(soundStore),
  /** The picture file in <home>/overlays (`[overlay] avatar`), served at /overlay/assets/<file>. */
  avatar: boot.avatar || "logo.jpg",
  scenarios: scenarios.map((s) => s.name),
  state: (name) => view(stateStore(name)),
  on(name, fn) {
    (listeners[name] ||= new Set()).add(fn);
    if (last[name]) fn(last[name]);
    return () => listeners[name].delete(fn);
  },
  ready(fn) { if (isReady) fn(); else readyFns.push(fn); },
  // location.origin is "null" in a sandboxed page; the URL still names the host.
  url: (path) => new URL(path, location.href).href,
  /** The parts the user has moved, in effect for the current skin: { part: { x, y, w } }. */
  parts: view(partsStore),
  now: () => Date.now(),
  applyScenario,
  // The editor pushes a change to a sample page without waiting for the config to reload.
  _push(data) { setTheme(data); },
  _measure: measureParts,
};

window.aftercast = ac;
// Hooks the editor calls on every overlay page (a theme change, the selected overlay's sound).
window.aftercastSkin = pickSkin;
window.aftercastLive = (on) => { if (!thumb) soundStore._set(!!on); };
// The editor talks to a sandboxed plugin page through messages (it cannot reach into it):
//   { aftercast: "vars", vars }       CSS variables for <body> (its frame, colors)
//   { aftercast: "skin" }             wear the skin the variables name
//   { aftercast: "sound", on }        a sample's sound on or off
//   { aftercast: "push", data }       settings, as the host's theme message carries them
//   { aftercast: "scenario", name }   show a sample
//   { aftercast: "measure", id }      reply { aftercast: "rects", id, rects } with every part's box
//   { aftercast: "parts", parts }     show these part places now (null: the saved ones)
addEventListener("message", (e) => {
  const d = e.data;
  if (e.source !== window.parent || !d || typeof d.aftercast !== "string") return;
  if (d.aftercast === "vars") {
    for (const [k, v] of Object.entries(d.vars || {})) document.body.style.setProperty(k, String(v));
    readTheme();
  } else if (d.aftercast === "skin") pickSkin(), schedulePlace();
  else if (d.aftercast === "sound") { if (!thumb) soundStore._set(!!d.on); }
  else if (d.aftercast === "push") setTheme(d.data || {});
  else if (d.aftercast === "scenario") applyScenario(d.name);
  else if (d.aftercast === "measure") parent.postMessage({ aftercast: "rects", id: d.id, rects: measureParts(), scale: frameStore.get().scale }, "*");
  else if (d.aftercast === "parts") { liveParts = d.parts || null; placeParts(); }
  else if (hooks[d.aftercast]) hooks[d.aftercast](d);
});
if (partIds.length) {
  new MutationObserver(schedulePlace).observe(document.documentElement, { childList: true, subtree: true });
  skinStore.subscribe(schedulePlace);
}
const start = () => {
  readTheme();
  schedulePlace();
  if (demo) applyScenario(query.get("scenario") || undefined);
  if (thumb) becomeReady();
  else connect();
  if (demo) setTimeout(becomeReady, 200);
};
if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
else start();

export default ac;
