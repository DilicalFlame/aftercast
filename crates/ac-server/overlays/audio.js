// aftercast audio service (plugin API v1): a playlist player and a spectrum, shared by plugins.
//
//   import { mount } from "/sdk/audio.js";
//   const player = mount(parts, "brb", true, { fade: () => 5, clip: () => 30 });
//   player.setVolume(0.7); player.setPlaying(true); ... player.stop();
//
// `playlist` is a folder in <home>/overlays/sounds/ ("" for none: only the spectrum runs).
// `parts` are the elements to drive; leave out what the page does not have, but give a canvas:
// bars (the small spectrum) and/or viz (a big one for the page's skin, read from
// document.body.dataset.skin: clean, broadcast, glass, minimal; anything else gets the bars
// look); cover (a div with an img; with no picture it gets the class "none", plus data-art 0-11
// and --hue to draw one from in CSS), title, artist, album (text), fill (the progress bar's fill),
// t1 and t2 (elapsed and length text). Put them in an element with class "music": it is hidden
// when the playlist folder is empty.
// Each track plays `clip()` seconds from a random place (0: all of it) with `fade()` seconds of
// fade at both ends; setPlaying fades in and out the same way. Without sound (a sample) a drum
// machine stand-in moves the spectrum. Colors come from the CSS variables --acc --acc2 --acc3.

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const lerp = (a, b, t) => a + (b - a) * t;
const say = (el, text) => { if (el) el.textContent = text; };
const shuffle = (list) => {
  const a = [...list];
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
};
const ARTS = 12;
const artSeed = Math.floor(Math.random() * ARTS), hueSeed = Math.floor(Math.random() * 360);
const clock = (sec) => `${Math.floor(sec / 60)}:${String(Math.floor(sec % 60)).padStart(2, "0")}`;
const BANDS = 64;

// ---- the stand-in for sound: 88 bpm, sixteenth steps, a few random ghost notes ----
const stand = () => {
  const walk = new Float32Array(BANDS).map(() => Math.random());
  let step = -1, kick = 0, snare = 0, hat = 0;
  return (t, dt, out) => {
    const s = Math.floor((t * 88 * 4) / 60);
    if (s !== step) {
      step = s;
      const i = s % 16, r = () => 0.75 + Math.random() * 0.25;
      if (i === 0 || i === 6 || i === 10 || (i === 14 && Math.random() < 0.4)) kick = r();
      if (i === 4 || i === 12) snare = r();
      if (i % 2 === 0 || Math.random() < 0.3) hat = r() * 0.8;
    }
    kick *= Math.exp(-dt * 7);
    snare *= Math.exp(-dt * 9);
    hat *= Math.exp(-dt * 14);
    for (let b = 0; b < BANDS; b++) {
      const p = b / (BANDS - 1);
      walk[b] = clamp(walk[b] + (Math.random() - 0.5) * 0.16, 0, 1);
      const pad = (0.22 + 0.34 * walk[b]) * (1 - 0.45 * p);
      const k = kick * Math.max(0, 1 - p * 3.4);
      const sn = snare * Math.exp(-(((p - 0.45) / 0.22) ** 2));
      const h = hat * Math.max(0, (p - 0.55) / 0.45) * (0.6 + 0.4 * walk[b]);
      out[b] = clamp(pad + k * 0.9 + sn * 0.8 + h * 0.9, 0, 1);
    }
  };
};

const api = {
  mount(parts, playlist, shuffleOn = true, opts = {}) {
    // A skin thumbnail (`nodock`) only looks: no sound, a small slow canvas.
    const thumb = new URLSearchParams(location.search).has("nodock");
    const audio = thumb
      ? { paused: true, duration: NaN, currentTime: 0, volume: 0, play: () => Promise.resolve(), pause() {}, removeAttribute() {} }
      : new Audio();
    audio.preload = "auto";
    // A sandboxed page is cross-origin to the host: the spectrum needs the audio to be CORS clean.
    audio.crossOrigin = "anonymous";
    let tracks = [], index = 0, playing = false, stopped = false, failures = 0, master = 1, seg = null;
    let ctx = null, analyser = null, bins = null, raf = 0, edges = null;
    const url = (kind, file) => `/overlay/${kind}/${encodeURIComponent(playlist)}/${encodeURIComponent(file)}`;

    const show = (i) => {
      // A player that was let go never starts a track again (its list may arrive late).
      if (stopped || closing || !tracks.length) return;
      index = (i + tracks.length) % tracks.length;
      const t = tracks[index];
      say(parts.title, t.title || t.file);
      say(parts.artist, t.artist || "");
      say(parts.album, t.album || "");
      if (parts.cover) {
        parts.cover.classList.remove("none");
        // A track without art gets one drawn in CSS: one of a dozen designs and a color, picked
        // at random for the stream and kept for the track (the same title, the same cover).
        let h = 0;
        for (const c of t.title || t.file) h = (h * 31 + c.charCodeAt(0)) % 100003;
        parts.cover.dataset.art = (h + artSeed) % ARTS;
        parts.cover.style.setProperty("--hue", (h * 7 + hueSeed) % 360);
        parts.cover.firstElementChild.onerror = () => parts.cover.classList.add("none");
        parts.cover.firstElementChild.src = url("cover", t.file);
      }
      // The words fade in with each track.
      if (parts.title) parts.title.parentElement.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 500, easing: "ease-out" });
      seg = null;
      audio.src = `/overlay/music/${encodeURIComponent(playlist)}/${encodeURIComponent(t.file)}`;
      if (playing) audio.play().catch(() => {});
    };
    const next = () => show(index + 1);
    // Each track plays a clip of its own: `--brb-clip` seconds from a random place (all of a
    // shorter track), fading in and out over `--brb-fade` seconds at its two ends.
    const num = (name, dflt) => {
      const v = parseFloat(getComputedStyle(document.body).getPropertyValue(name));
      return Number.isFinite(v) ? v : dflt;
    };
    audio.onloadedmetadata = () => {
      const whole = audio.duration, clip = opts.clip ? opts.clip() : 30, known = Number.isFinite(whole);
      const len = known ? (clip > 0 ? Math.min(clip, whole) : whole) : clip > 0 ? clip : Infinity;
      const start = known && whole > len ? Math.random() * (whole - len) : 0;
      seg = { start, len, fade: Math.min(opts.fade ? opts.fade() : 5, len / 2) };
      if (start > 0) audio.currentTime = start;
    };
    // Volume = master x the clip's own fades x `ramp`, which rises from silence whenever the
    // music is asked to play (the BRB starts) and falls to silence, then pauses, when it is not
    // (we are back): neither start nor stop is ever sudden.
    let want = false, ramp = 0, closing = false, lastTick = performance.now();
    const finish = () => {
      stopped = true;
      playing = false;
      clearInterval(gain);
      cancelAnimationFrame(raf);
      audio.pause();
      audio.removeAttribute("src");
      if (ctx) ctx.close().catch(() => {});
    };
    const gain = setInterval(() => {
      if (stopped) return;
      const now = performance.now(), dt = (now - lastTick) / 1000;
      lastTick = now;
      const fade = closing ? 0.25 : opts.fade ? opts.fade() : 5;
      ramp = fade > 0 ? clamp(ramp + (want ? dt : -dt) / fade, 0, 1) : want ? 1 : 0;
      if (!want && ramp === 0) {
        if (!audio.paused) audio.pause();
        playing = false;
        if (closing) finish();
        return;
      }
      if (!seg) { audio.volume = 0; return; }
      const pos = audio.currentTime - seg.start;
      if (playing && !audio.paused && pos >= seg.len - 0.05) { next(); return; }
      const g = seg.fade > 0 ? Math.min(1, pos / seg.fade, (seg.len - pos) / seg.fade) : 1;
      audio.volume = clamp(master * clamp(g, 0, 1) * ramp, 0, 1);
    }, 60);
    // A track that cannot be played is skipped; if none can, stop trying.
    audio.onended = () => { failures = 0; next(); };
    audio.onerror = () => { if (++failures < Math.max(2, tracks.length)) next(); };

    if (playlist) {
      fetch(`/overlay/music/${encodeURIComponent(playlist)}/index.json`)
        .then((r) => r.json())
        .then((list) => {
          // No files in the folder: no music half at all.
          if (!list.length) {
            const box = parts.cover && parts.cover.closest(".music");
            if (box) box.hidden = true;
            return;
          }
          tracks = shuffleOn ? shuffle(list) : list;
          show(0);
        })
        .catch(() => {});
    }

    // ---- the spectrum: 64 log-spaced bands, auto-gained, fast attack and slow release ----
    const level = new Float32Array(BANDS).fill(0.05), raw = new Float32Array(BANDS), peak = new Float32Array(BANDS).fill(0.05);
    const faked = stand();
    let agc = 0.3, bassAvg = 0, pulse = 0, lastBeat = 0, last = 0, tint = null, tintAt = -1e9, frame = 0;
    const sample = (arr, p) => {
      const x = clamp(p, 0, 1) * (BANDS - 1), i = Math.floor(x), f = x - i;
      return lerp(arr[i], arr[Math.min(BANDS - 1, i + 1)], f);
    };
    // A band's value smoothed over its neighbours (the big bars read as one shape).
    const soft = (arr, p, r) => {
      let s = 0, n = 0;
      for (let k = -r; k <= r; k++) { s += sample(arr, p + k / (BANDS - 1)); n++; }
      return s / n;
    };
    const colors = (canvas, now) => {
      if (!tint || now - tintAt > 500) {
        const cs = getComputedStyle(canvas);
        const v = (n, d) => cs.getPropertyValue(n).trim() || d;
        tint = { a: v("--acc", "#a78bfa"), b: v("--acc2", "#f0abfc"), c: v("--acc3", "#7dd3fc") };
        tintAt = now;
      }
      return tint;
    };

    const analyse = (t, dt) => {
      const real = playing && tracks.length && !audio.paused && analyser && ctx && ctx.state === "running";
      if (real) {
        analyser.getByteFrequencyData(bins);
        let top = 0;
        for (let b = 0; b < BANDS; b++) {
          let s = 0;
          for (let i = edges[b]; i < edges[b + 1]; i++) s += bins[i];
          const v = s / (edges[b + 1] - edges[b]) / 255;
          raw[b] = v * (0.8 + 0.7 * (b / BANDS));
          if (raw[b] > top) top = raw[b];
        }
        // Gain follows the loudest band so quiet and loud tracks both fill the bars.
        agc = Math.max(top, agc * 0.995, 0.22);
        for (let b = 0; b < BANDS; b++) raw[b] = clamp((raw[b] / agc) ** 1.5, 0, 1);
      } else faked(t, dt, raw);
      let bass = 0;
      for (let b = 0; b < BANDS; b++) {
        level[b] += (raw[b] - level[b]) * (raw[b] > level[b] ? 0.7 : 0.17);
        peak[b] = Math.max(level[b], peak[b] - dt * 0.45);
        if (b < 6) bass += level[b] / 6;
      }
      bassAvg = bassAvg * 0.96 + bass * 0.04;
      if (bass > bassAvg * 1.25 + 0.06 && t - lastBeat > 0.18) { pulse = 1; lastBeat = t; }
      pulse *= Math.exp(-dt * 6);
      return bass;
    };

    // ---- small bars in the player: rounded, glowing, with a reflection and peak dots ----
    const smallBars = (canvas, c) => {
      const g = canvas.getContext("2d");
      const w = canvas.clientWidth, h = canvas.clientHeight;
      if (!w || !h) return;
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      const gap = 6, bw = 8, N = Math.max(8, Math.floor((w + gap) / (bw + gap)));
      g.clearRect(0, 0, w, h);
      const base = h * 0.76, room = base - 4, x0 = (w - (N * (bw + gap) - gap)) / 2;
      const up = g.createLinearGradient(0, base, 0, 0);
      up.addColorStop(0, c.a);
      up.addColorStop(1, c.b);
      const down = g.createLinearGradient(0, base, 0, h);
      down.addColorStop(0, c.a);
      down.addColorStop(1, "transparent");
      for (let b = 0; b < N; b++) {
        const p = b / (N - 1), v = soft(level, p, 1), bh = Math.max(bw, v * room), x = x0 + b * (bw + gap);
        g.fillStyle = up;
        g.beginPath();
        g.roundRect(x, base - bh, bw, bh, bw / 2);
        g.fill();
        g.globalAlpha = 0.2;
        g.fillStyle = down;
        g.beginPath();
        g.roundRect(x, base + 3, bw, Math.min(bh * 0.4, h - base - 3), bw / 2);
        g.fill();
        g.globalAlpha = 0.9;
        g.fillStyle = "#fff";
        g.beginPath();
        g.arc(x + bw / 2, base - soft(peak, p, 1) * room - bw, bw / 2 - 1, 0, Math.PI * 2);
        g.fill();
        g.globalAlpha = 1;
      }
    };

    // ---- the big one behind the screen, by skin ----
    const SCALE = 0.75;
    const big = (canvas, c, t) => {
      const mode = document.body.dataset.skin || "nvim";
      const W = canvas.clientWidth, H = canvas.clientHeight;
      if (!W || !H) return;
      const sc = thumb ? 0.2 : SCALE, cw = Math.round(W * sc), ch = Math.round(H * sc);
      if (canvas.width !== cw || canvas.height !== ch) { canvas.width = cw; canvas.height = ch; }
      const g = canvas.getContext("2d");
      g.setTransform(sc, 0, 0, sc, 0, 0);
      g.clearRect(0, 0, W, H);
      if (mode === "clean") {
        // Wide soft bars standing on the bottom edge, as tall as the sound is loud.
        const N = 36, step = W / N, bw = step * 0.78, r = bw * 0.4;
        const grad = g.createLinearGradient(0, H, 0, H * 0.1);
        grad.addColorStop(0, c.a + "8c");
        grad.addColorStop(0.6, c.b + "44");
        grad.addColorStop(1, c.b + "0c");
        g.fillStyle = grad;
        for (let b = 0; b < N; b++) {
          const v = soft(level, b / (N - 1), 1), bh = H * (0.05 + v * 0.64);
          g.beginPath();
          g.roundRect(b * step + (step - bw) / 2, H - bh, bw, bh + r, [r, r, 0, 0]);
          g.fill();
        }
      } else if (mode === "broadcast") {
        // A level meter: stacks of lit blocks.
        const N = 30, step = W / N, bw = step - 10, blk = 28, gap = 8, rows = Math.floor((H * 0.86) / (blk + gap));
        for (let b = 0; b < N; b++) {
          const v = soft(level, b / (N - 1), 1), lit = Math.round(v * rows * 0.8);
          for (let r = 0; r < rows; r++) {
            const on = r < lit, mix = r / rows;
            g.globalAlpha = on ? 0.8 - mix * 0.3 : 0.035;
            g.fillStyle = on ? (mix > 0.72 ? c.c : mix > 0.4 ? c.b : c.a) : "#fff";
            g.fillRect(b * step + 5, H - (r + 1) * (blk + gap), bw, blk);
          }
        }
        g.globalAlpha = 1;
      } else if (mode === "glass") {
        // Three soft waves, one behind the other, each a little slower than the one in front.
        const M = 28;
        for (const [k, col, a, rad] of [[2, c.c, 0.3, 4], [1, c.b, 0.4, 3], [0, c.a, 0.5, 2]]) {
          const pts = [];
          for (let i = 0; i <= M; i++) pts.push([(i / M) * W, H - (0.1 + soft(level, i / M, rad) * 0.7 + 0.025 * Math.sin(t * 0.8 + i * 0.4 + k)) * H * (1 - k * 0.08)]);
          g.beginPath();
          g.moveTo(0, H);
          g.lineTo(pts[0][0], pts[0][1]);
          for (let i = 1; i < pts.length; i++) {
            const [x1, y1] = pts[i - 1], [x2, y2] = pts[i];
            g.quadraticCurveTo(x1, y1, (x1 + x2) / 2, (y1 + y2) / 2);
          }
          g.lineTo(W, pts[M][1]);
          g.lineTo(W, H);
          g.closePath();
          const grad = g.createLinearGradient(0, H * 0.2, 0, H);
          grad.addColorStop(0, col + Math.round(a * 255).toString(16).padStart(2, "0"));
          grad.addColorStop(1, col + "08");
          g.fillStyle = grad;
          g.fill();
        }
      } else if (mode === "minimal") {
        // Hairlines across the whole screen, standing on the bottom edge.
        const N = 110, step = W / N;
        for (let b = 0; b < N; b++) {
          const p = b / (N - 1), v = soft(level, p, 0), bh = H * (0.03 + v * 0.9), x = b * step + step / 2 - 1.5;
          g.globalAlpha = 0.34;
          g.fillStyle = c.a;
          g.fillRect(x, H - bh, 3, bh);
          g.globalAlpha = 0.9;
          g.fillRect(x, H - bh, 3, 7);
        }
        g.globalAlpha = 1;
      }
    };

    let lastDraw = 0;
    const draw = (now) => {
      if (stopped) return;
      raf = requestAnimationFrame(draw);
      if (now - lastDraw < (thumb ? 90 : 30)) return;
      lastDraw = now;
      const t = now / 1000, dt = clamp(t - last, 0.001, 0.1);
      last = t;
      const bass = analyse(t, dt);
      frame++;
      const probe = parts.viz || parts.bars;
      if (!probe) return;
      const c = colors(probe, now);
      if (!thumb && parts.bars && parts.bars.clientWidth) smallBars(parts.bars, c);
      if (parts.viz) big(parts.viz, c, t);
      // The album card kicks with the beat.
      if (parts.cover) parts.cover.style.transform = `scale(${1 + bass * 0.03 + pulse * 0.04})`;
      parts.viz && parts.viz.style.setProperty("--pulse", pulse.toFixed(3));
      // Progress: the track's own, or a slow sample while there is no sound.
      if (parts.fill) {
        const live = seg && playing && !audio.paused;
        const dur = seg && Number.isFinite(seg.len) ? seg.len : 200;
        const at = live ? clamp(audio.currentTime - seg.start, 0, dur) : (t * 2) % dur;
        parts.fill.style.width = (at / dur) * 100 + "%";
        say(parts.t1, clock(at));
        say(parts.t2, clock(dur));
      }
    };
    raf = requestAnimationFrame(draw);

    const tap = () => {
      if (ctx) return;
      ctx = new (window.AudioContext || window.webkitAudioContext)();
      const src = ctx.createMediaElementSource(audio);
      analyser = ctx.createAnalyser();
      analyser.fftSize = 1024;
      analyser.smoothingTimeConstant = 0.5;
      analyser.minDecibels = -85;
      analyser.maxDecibels = -18;
      bins = new Uint8Array(analyser.frequencyBinCount);
      // Band edges, log-spaced from about 90 Hz to 14 kHz, each at least one bin wide.
      edges = new Uint16Array(BANDS + 1);
      const lo = 2, hi = Math.min(bins.length - 2, 330);
      edges[0] = lo;
      for (let b = 1; b <= BANDS; b++) edges[b] = Math.max(edges[b - 1] + 1, Math.round(lo * Math.pow(hi / lo, b / BANDS)));
      src.connect(analyser);
      analyser.connect(ctx.destination);
    };

    return {
      setPlaying(on) {
        if (stopped || closing) return;
        want = on;
        if (on) {
          playing = true;
          try { tap(); ctx.resume(); } catch { /* no audio device */ }
          if (tracks.length && audio.paused) audio.play().catch(() => {});
        }
      },
      setVolume(v) { master = clamp(v, 0, 1); },
      // The screen is drawn again or taken away: a quick fade (a quarter second), then
      // everything is let go, so old players never pile up. (Leaving the BRB itself is
      // setPlaying(false), which fades over `--brb-fade`.)
      stop() {
        if (stopped || closing) return;
        closing = true;
        want = false;
        if (audio.paused || !playing) finish();
        else setTimeout(() => { if (!stopped) finish(); }, 600);
      },
    };
  },
};
export const mount = api.mount;
