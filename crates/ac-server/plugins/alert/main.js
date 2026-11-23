import ac from "/sdk/aftercast.js";
import { el, row, clock, nameColor } from "/sdk/ui.js";

const root = document.getElementById("ac-frame");
const query = ac.query;
const params = { has: (k) => (k === "demo" ? ac.mode !== "live" : query.has(k)) };
const list = (v) => ac.cssList(v);
let soundDemo = false;
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

// Sound is on unless switched off (everywhere, or for this overlay); the editor's samples are
// silent until this overlay is selected or a Play button asks.
const refresh = () => {
  const loud = list("--sound")[0] !== "0" && list("--mute")[0] !== "1" && ac.sound.get() && ac.mode !== "thumb";
  const volume = (Number(list("--volume")[0]) || 0) / 100;
  window.aftercastSfx.useSkin(list("--sound-skin")[0]);
  window.aftercastSfx.useFile(list("--sound-file")[0]);
  window.aftercastSfx.volume(volume);
  window.aftercastSfx.enabled(loud);
  if (ac.sound.get()) window.aftercastSfx.unlock();
  soundDemo = ac.sound.get();
};
ac.onTheme(refresh);
ac.sound.subscribe(refresh);
ac.hook("play", (m) => window.aftercastPlay({ sound: m.sound, volume: m.volume }));
ac.on("alert", (m) => {
  if (m.action === "hide") return;
  alerts.push(m.data || {});
  if (!alerting) nextAlert();
});
if (ac.mode === "thumb") {
  // A skin thumbnail: one still from the middle of the animation.
  setTimeout(() => {
    const host = el("div", "alert");
    root.replaceChildren(host);
    const skin = document.body.dataset.skin;
    window.aftercastAlert.still(host, SAMPLE_ALERT, ALERT_SKINS.includes(skin) ? skin : "dino", 3.3);
  }, 300);
} else if (ac.mode === "demo") {
  const loop = () => { alerts.push(SAMPLE_ALERT); if (!alerting) nextAlert(); setTimeout(loop, 11000); };
  setTimeout(loop, 500);
}
