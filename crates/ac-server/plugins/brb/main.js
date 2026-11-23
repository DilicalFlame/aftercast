// Be right back, as an aftercast overlay plugin. The page is built once; settings change it in
// place (nothing rebuilds while you drag a slider, so the music keeps playing), and the stream's
// "brb" state starts the timer and the music.
import ac from "/sdk/aftercast.js";
import { mount } from "/sdk/audio.js";

const frame = document.getElementById("ac-frame");
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

// ---- the page ----
const card = el("div", "card brb");

// The backdrop: stars, shooting stars, clouds and a sleeping cat (the skins that have them),
// and the big spectrum canvas behind everything.
const scene = el("div", "brbbg");
const viz = el("canvas", "bgviz");
scene.append(el("div", "aurora"), viz);
const stars = el("div", "stars");
[[6, 33, 34], [4, 70, 28], [39, 83, 22], [97, 6, 30], [95, 17, 20], [52, 12, 16], [23, 8, 14], [72, 58, 14], [88, 90, 18], [31, 50, 12]]
  .forEach(([x, y, s], i) => {
    const star = el("i");
    star.style.cssText = `left:${x}%;top:${y}%;width:${s}px;height:${s}px;animation-delay:${(i * 0.7) % 4}s`;
    stars.append(star);
  });
scene.append(stars);
for (const c of ["a", "b", "c"]) scene.append(el("i", `shoot ${c}`));
scene.append(el("div", "cloud back"), el("div", "cloud front"));
const cat = el("div", "cat");
// A cat asleep on the clouds: round head on its paws, ears that twitch, a tail that sways.
cat.innerHTML = '<span class="z z1">z</span><span class="z z2">z</span><span class="z z3">Z</span>'
  + '<svg viewBox="0 0 340 170" aria-hidden="true">'
  + '<path d="M6 170C-6 118 44 92 112 90C200 82 296 92 326 170Z" fill="currentColor"/>'
  + '<path class="tail" d="M312 142C352 134 344 92 306 98" fill="none" stroke="currentColor" stroke-width="17" stroke-linecap="round"/>'
  + '<ellipse cx="190" cy="118" rx="92" ry="40" fill="#fff" opacity=".06"/>'
  + '<path d="M168 92q10 20 0 44M204 90q10 22 0 46M238 94q8 18 0 38" fill="none" stroke="var(--bgc)" stroke-opacity=".28" stroke-width="5" stroke-linecap="round"/>'
  + '<circle cx="84" cy="112" r="46" fill="currentColor"/>'
  + '<path class="ear l" d="M46 94L42 46Q44 38 52 42L82 70Z" fill="currentColor" stroke="currentColor" stroke-width="6" stroke-linejoin="round"/>'
  + '<path class="ear r" d="M122 86L134 42Q136 34 144 40L136 100Z" fill="currentColor" stroke="currentColor" stroke-width="6" stroke-linejoin="round"/>'
  + '<path d="M52 82L52 58L70 74ZM128 80L136 56L134 86Z" fill="var(--acc)" opacity=".35"/>'
  + '<ellipse cx="74" cy="152" rx="24" ry="13" fill="currentColor"/><ellipse cx="118" cy="154" rx="24" ry="13" fill="currentColor"/>'
  + '<ellipse cx="74" cy="152" rx="24" ry="13" fill="#fff" opacity=".07"/>'
  + '<path d="M60 112q9 9 18 0M96 112q9 9 18 0" fill="none" stroke="var(--bgc)" stroke-width="4" stroke-linecap="round"/>'
  + '<path d="M86 122l8 0l-4 5z" fill="var(--acc)" opacity=".7"/>'
  + '<path d="M44 126l-26-4M44 134l-24 6M128 126l24-4M128 134l22 6" fill="none" stroke="var(--acc)" stroke-opacity=".45" stroke-width="2.5" stroke-linecap="round"/>'
  + '</svg>';
scene.append(cat);
card.append(scene);

// Title, timer, lines
const timer = el("div", "timer");
const title = el("div", "title");
const top = el("div", "brb-top");
top.dataset.part = "title";
top.append(timer, title);
const lines = el("div", "brb-text");
lines.dataset.part = "lines";
const main = el("div", "brb-main");

// Your picture
const pic = el("img", "pfp circle");
pic.dataset.part = "pfp";
pic.alt = "";
pic.src = ac.url(`/overlay/assets/${encodeURIComponent(ac.avatar)}`);
pic.onerror = () => { pic.dataset.broken = "1"; applyParams(ac.params.get()); };

// The player
const cover = el("div", "cover");
cover.append(el("img"));
const track = el("div", "track");
const fill = el("i");
const t1 = el("span", "", "0:00"), t2 = el("span", "", "0:00");
const prog = el("div", "prog");
prog.append(fill);
const times = el("div", "times");
times.append(t1, t2);
const parts = { viz, cover, title: track, artist: el("div", "artist"), album: el("div", "album"), bars: el("canvas", "bars"), fill, t1, t2 };
const np = el("div", "np");
np.append(el("span", "eq"), "now playing");
const words = el("div", "words");
words.append(np, track, parts.artist, parts.album);
const meta = el("div", "meta");
meta.append(words, parts.bars, prog, times);
const music = el("div", "music");
music.dataset.part = "music";
music.append(cover, meta);

// The support QR code
const pay = el("div", "pay");
pay.dataset.part = "qr";
const qr = el("img", "qr");
qr.alt = "";
qr.onerror = () => { pay.dataset.broken = "1"; applyParams(ac.params.get()); };
const note = el("div", "note");
pay.append(el("div", "paytop", "♥ support the stream"), qr, note);

main.append(pic, music, pay);
card.append(top, lines, main);
frame.append(card);

// ---- settings: applied in place ----
let player = null, playerKey = "";
const opts = { fade: () => Number(ac.params.get().fade) || 0, clip: () => Number(ac.params.get().clip) || 0 };
let heading = "Be right back";

function setTitle() {
  const text = (ac.params.get().title || "").trim() || heading;
  const words = text.split(" ");
  title.replaceChildren();
  if (words.length > 1) title.append(words.slice(0, -1).join(" ") + " ", Object.assign(el("span", "hl"), { textContent: words[words.length - 1] }));
  else title.append(Object.assign(el("span", "hl"), { textContent: words[0] }));
}

function applyParams(p) {
  setTitle();
  lines.replaceChildren(...(p.lines || []).map((t) => el("span", "line", t)));
  lines.hidden = !(p.lines || []).length;
  pic.hidden = !p.pfp || pic.dataset.broken === "1";
  pic.className = `pfp ${p.pfp_shape}`;
  pic.style.width = pic.style.height = `${p.pfp_size}px`;
  pay.hidden = !p.qr || pay.dataset.broken === "1";
  qr.style.width = qr.style.height = `${p.qr_size}px`;
  note.textContent = p.qr_caption || "";
  note.hidden = !p.qr_caption;
  const th = ac.theme.get();
  const src = `/overlay/upi.svg?link=${encodeURIComponent(p.qr_link || "")}&accent=${encodeURIComponent(th.accent || "")}`;
  if (qr.dataset.src !== src) { qr.dataset.src = src; qr.src = ac.url(src); }
  // The player is rebuilt only when its source changes; it keeps playing through everything else.
  const key = `${p.music}|${p.playlist}|${p.shuffle}`;
  music.hidden = !p.music;
  if (key !== playerKey) {
    playerKey = key;
    if (player) player.stop();
    player = p.music && p.playlist ? mount(parts, p.playlist, p.shuffle, opts) : mount({ viz }, "", true, opts);
    syncSound();
  }
}

// ---- the stream: the timer, the title, the music ----
let since = null, away = false;
function syncSound() {
  if (!player) return;
  player.setVolume(ac.volume.get());
  player.setPlaying(away && ac.sound.get() && !!ac.params.get().music);
}
ac.state("brb").subscribe((m) => {
  away = !!m && m.action === "show";
  since = away && m.data ? m.data.since_ms || null : null;
  heading = (away && m.data && m.data.text) || "Be right back";
  setTitle();
  syncSound();
});
ac.sound.subscribe(syncSound);
ac.volume.subscribe(syncSound);
ac.params.subscribe(applyParams);
setInterval(() => { timer.textContent = clock(since ? (ac.now() - since) / 1000 : 0); }, 500);
timer.textContent = clock(0);
