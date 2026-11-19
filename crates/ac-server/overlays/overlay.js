// aftercast overlays: one page per OBS browser source (1920x1080, transparent).
// Messages: { overlay, action: "show" | "hide" | "update", data }.
(() => {
  const name = document.body.dataset.overlay;
  const root = document.getElementById("root");
  let ticker = null;
  let keysIdle = null;
  let keysText = "";

  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
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
    root.replaceChildren(el("div", "card toast", text));
    setTimeout(() => {
      root.replaceChildren();
      setTimeout(nextToast, 300);
    }, 5000);
  };

  const render = {
    now(d) {
      const card = el("div", "card now");
      const quest = el("span", "quest", d.quest || "");
      const time = el("span", "timer");
      card.append(quest, time);
      root.replaceChildren(card);
      if (d.live_since_ms) tick(() => (time.textContent = clock((Date.now() - d.live_since_ms) / 1000)), 1000);
    },
    question(d) {
      const card = el("div", "card question");
      card.append(el("div", "author", d.author), el("div", "text", d.text));
      root.replaceChildren(card);
    },
    brb(d) {
      const card = el("div", "card brb");
      const time = el("div", "timer");
      card.append(el("div", "title", d.text || "Be right back"), time);
      root.replaceChildren(card);
      if (d.since_ms) tick(() => (time.textContent = clock((Date.now() - d.since_ms) / 1000)), 1000);
    },
    latency(d) {
      const card = el("div", "card latency");
      root.replaceChildren(card);
      tick(() => (card.textContent = `LAT ${Math.floor((Date.now() - d.start_ms) / 100)}`), 50);
    },
    chapters(d) {
      root.replaceChildren(el("div", "card chapters", d.title || ""));
    },
    // Typed text as a rolling line (last 24 characters) and shortcuts as key chips.
    // Password boxes and denied apps never reach the overlay. Cleared after 1.5 s idle.
    keys(d) {
      clearTimeout(keysIdle);
      if (d.ch === "\n") keysText = "";
      else if (d.ch) keysText = (keysText + (d.ch === "\t" ? " " : d.ch)).slice(-24);
      else if (d.back) keysText = keysText.slice(0, -1);
      const card = el("div", "card keys");
      if (keysText) card.append(el("span", "text", keysText));
      if (d.combo) card.append(...d.combo.split("+").map((k) => el("kbd", null, k)));
      if (card.childElementCount) root.replaceChildren(card);
      keysIdle = setTimeout(() => {
        keysText = "";
        clear();
      }, 1500);
    },
  };

  const handle = (msg) => {
    if (msg.overlay !== name) return;
    if (name === "toast") {
      if (msg.data && msg.data.text) toasts.push(msg.data.text);
      if (!toasting) nextToast();
      return;
    }
    if (msg.action === "hide") return clear();
    const draw = render[name];
    if (draw) draw(msg.data || {});
  };

  const connect = () => {
    const ws = new WebSocket(`ws://${location.host}/ws`);
    ws.onmessage = (e) => handle(JSON.parse(e.data));
    // aftercast restarted or not running yet: keep trying. The server resends state.
    ws.onclose = () => setTimeout(connect, 1000);
  };
  connect();
})();
