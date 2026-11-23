// __NAME__: an aftercast overlay. The host serves this page at /overlay/__ID__ and gives it
// the SDK below; everything else (placement, layers, scenes, sample data) is the host's.
import ac from "/sdk/aftercast.js";

const $ = (selector) => document.querySelector(selector);
const clock = (ms) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, r = s % 60;
  const pad = (n) => String(n).padStart(2, "0");
  return h ? `${h}:${pad(m)}:${pad(r)}` : `${m}:${pad(r)}`;
};

// Settings from overlay.toml's [[param]]s; they update live while you edit them.
ac.params.subscribe((p) => {
  $(".label").textContent = p.label;
  $(".card").style.setProperty("--tint", p.accent);
  $(".timer").hidden = !p.show_timer;
});

// A stream of the host: "now" is shown while you are live, with the quest and when it began.
let since = null;
ac.state("now").subscribe((message) => {
  const data = message && message.action === "show" ? message.data : null;
  $(".card").hidden = !data;
  $(".quest").textContent = data ? data.quest || "" : "";
  since = data ? data.live_since_ms : null;
});
setInterval(() => {
  $(".timer").textContent = since ? clock(ac.now() - since) : "";
}, 1000);
