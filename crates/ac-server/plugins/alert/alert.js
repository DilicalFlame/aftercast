// The Super Chat alert: a short canvas animation per skin, drawn inside the alert's frame,
// (its sound is a file: sfx.js). window.aftercastAlert.play(host, data, skin) resolves when it
// is over. data: { author, amount, text, member }. Everything is drawn in code.
(() => {
  const css = (name, fallback) => getComputedStyle(document.body).getPropertyValue(name).trim() || fallback;
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const rand = (a, b) => a + Math.random() * (b - a);

  const headline = (d) => (d.member ? `${d.author} joined as a member` : `${d.author} donated ${d.amount}`);

  // Text with an outline, so it reads on any picture.
  const word = (g, text, x, y, fill, stroke = "#000", line = 6) => {
    g.lineJoin = "round";
    g.lineWidth = line;
    g.strokeStyle = stroke;
    g.strokeText(text, x, y);
    g.fillStyle = fill;
    g.fillText(text, x, y);
  };

  // The animation is this wide at most; a wider frame places it by its align (left, center, right).
  const MAXW = 960;
  const SCENES = {};

  // Chrome dino: the sentence stands on the ground in big letters, the dino runs in from the
  // left and jumps over each word. The amount is the last thing it clears: a coin.
  SCENES.dino = {
    init(g, w, h, d, ctx) {
      const fs = clamp(h * 0.34, 28, 64);
      g.font = `800 ${fs}px ${ctx.mono}`;
      const speed = 430, dx = w * 0.14, u = Math.max(2, Math.round(h / 52));
      const dw = 20 * u;
      const ground = h - 22;
      // Words in world coordinates: the first starts just past the right edge.
      let x = w + 40;
      const words = headline(d).split(" ").map((text) => {
        const width = g.measureText(text).width;
        const item = { text, x, width };
        x += width + g.measureText(" ").width * 1.6;
        return item;
      });
      // Jump windows: airborne from just before a word reaches the dino until it has passed.
      const wins = [];
      for (const k of words) {
        const a = (k.x - dx - dw) / speed - 0.08, b = (k.x + k.width - dx) / speed + 0.08;
        const last = wins[wins.length - 1];
        if (last && a < last.b + 0.12) last.b = b;
        else wins.push({ a, b, done: false, up: false });
      }
      const end = (words[words.length - 1].x + words[words.length - 1].width - dx) / speed;
      return { fs, speed, dx, u, dw, ground, words, wins, dur: end + 2.2, coin: false, clouds: [0.2, 0.55, 0.85].map((f) => ({ x: w * f, y: rand(6, h * 0.3) })) };
    },
    draw(g, w, h, s, t, ctx) {
      const ink = ctx.fg;
      // Sky details and ground.
      g.fillStyle = ctx.muted;
      for (const c of s.clouds) {
        const cx = ((c.x - t * 40) % (w + 120) + (w + 120)) % (w + 120) - 60;
        g.globalAlpha *= 0.5;
        g.fillRect(cx, c.y, 60, 8);
        g.fillRect(cx + 12, c.y - 8, 34, 8);
        g.globalAlpha /= 0.5;
      }
      g.fillStyle = ink;
      g.fillRect(0, s.ground + 2, w, 3);
      for (let i = 0; i < 22; i++) {
        const px = ((i * 97 - t * s.speed) % w + w) % w;
        g.fillRect(px, s.ground + 9 + (i % 3) * 5, 6 + (i % 4) * 4, 2);
      }
      // Letters.
      g.font = `800 ${s.fs}px ${ctx.mono}`;
      g.textBaseline = "alphabetic";
      for (const k of s.words) {
        const x = k.x - t * s.speed;
        if (x + k.width < -20 || x > w + 20) continue;
        word(g, k.text, x, s.ground, ctx.accent, "#0008", 5);
      }
      // The dino, up in the air inside a jump window.
      let lift = 0;
      let air = false;
      for (const j of s.wins) {
        if (t >= j.a && t <= j.b) {
          const p = (t - j.a) / (j.b - j.a);
          lift = 4 * p * (1 - p) * (s.fs * 0.75 + 26);
          air = true;
        }
      }
      const u = s.u, x0 = s.dx, y0 = s.ground - 14 * u - lift;
      const legs = air ? 0 : Math.floor(t * 9) % 2;
      g.fillStyle = ink;
      g.fillRect(x0 + 10 * u, y0, 10 * u, 7 * u); // head
      g.fillRect(x0 + 8 * u, y0 + 4 * u, 6 * u, 6 * u); // neck
      g.fillRect(x0 + 4 * u, y0 + 6 * u, 10 * u, 8 * u); // body
      g.fillRect(x0, y0 + 7 * u, 5 * u, 3 * u); // tail
      g.fillRect(x0 + 14 * u, y0 + 10 * u, 3 * u, u); // arm
      g.fillRect(x0 + 6 * u, y0 + 14 * u, 2 * u, (legs ? 2 : 4) * u);
      g.fillRect(x0 + 10 * u, y0 + 14 * u, 2 * u, (legs ? 4 : 2) * u);
      g.fillStyle = "#000";
      g.fillRect(x0 + 15 * u, y0 + u, u * 1.4, u * 1.4); // eye
      // The coin at the end.
      if (t > s.dur - 1.9) {
        const q = clamp((t - (s.dur - 1.9)) / 0.5, 0, 1);
        g.fillStyle = "#ffd23a";
        g.beginPath();
        g.arc(x0 + 12 * u, y0 - 10 - q * 30, 11, 0, 7);
        g.fill();
      }
    },
  };

  // Pac-Man eats the sentence from left to right, a ghost on its heels.
  SCENES.pacman = {
    init(g, w, h, d, ctx) {
      const fs = clamp(h * 0.3, 26, 58);
      g.font = `800 ${fs}px ${ctx.mono}`;
      const text = headline(d);
      const tw = g.measureText(text).width;
      return { fs, text, tw, x: (w - tw) / 2, y: h * 0.5 + fs * 0.35, r: clamp(h * 0.2, 14, 36), start: 0.9, dur: 6.2, wakas: 0 };
    },
    draw(g, w, h, s, t, ctx) {
      const run = clamp((t - s.start) / (s.dur - s.start - 0.8), 0, 1);
      const px = -40 + run * (w + 120);
      const cy = s.y - s.fs * 0.35;
      // Dots between the letters' height, and the letters, only right of Pac-Man.
      g.save();
      g.beginPath();
      g.rect(px, 0, w, h);
      g.clip();
      g.fillStyle = ctx.fg;
      for (let x = 20; x < w; x += 34) g.fillRect(x, s.y + 12, 6, 6);
      g.font = `800 ${s.fs}px ${ctx.mono}`;
      word(g, s.text, s.x, s.y, ctx.fg, "#000", 5);
      g.restore();
      if (t >= s.start && px < w + 30) {
        const mouth = (Math.abs(Math.sin(t * 13)) * 0.9 + 0.05) * 0.5;
        g.fillStyle = "#ffe033";
        g.beginPath();
        g.moveTo(px, cy);
        g.arc(px, cy, s.r, mouth, Math.PI * 2 - mouth);
        g.closePath();
        g.fill();
        // The ghost.
        const gx = px - s.r * 3.4, gr = s.r * 0.9;
        if (gx > -gr && t - s.start > 0.6) {
          g.fillStyle = ctx.accent;
          g.beginPath();
          g.arc(gx, cy - 2, gr, Math.PI, 0);
          g.lineTo(gx + gr, cy + gr);
          for (let i = 0; i < 4; i++) g.lineTo(gx + gr - (i + 0.5) * gr / 2, cy + gr - (i % 2 ? 0 : 6));
          g.lineTo(gx - gr, cy + gr);
          g.closePath();
          g.fill();
          g.fillStyle = "#fff";
          g.fillRect(gx - gr * 0.5, cy - gr * 0.5, gr * 0.4, gr * 0.5);
          g.fillRect(gx + gr * 0.1, cy - gr * 0.5, gr * 0.4, gr * 0.5);
          g.fillStyle = "#14213d";
          g.fillRect(gx - gr * 0.3, cy - gr * 0.3, gr * 0.2, gr * 0.3);
          g.fillRect(gx + gr * 0.3, cy - gr * 0.3, gr * 0.2, gr * 0.3);
        }
      }
    },
  };

  // A dancer moonwalks along the bottom edge of the frame (put that edge on whatever it
  // should walk on, e.g. the top of the facecam) under a neon headline.
  SCENES.moonwalk = {
    init(g, w, h, d, ctx) {
      return { fs: clamp(h * 0.26, 22, 50), text: headline(d), dur: 7, beat: -1, from: w * 0.92, to: w * 0.1 };
    },
    draw(g, w, h, s, t, ctx) {
      const hue = (t * 120) % 360;
      // Spotlight.
      const sp = g.createLinearGradient(0, 0, 0, h);
      sp.addColorStop(0, `hsl(${hue} 90% 60% / 0.30)`);
      sp.addColorStop(1, "hsl(0 0% 0% / 0)");
      g.fillStyle = sp;
      g.beginPath();
      g.moveTo(w * 0.4, 0);
      g.lineTo(w * 0.6, 0);
      g.lineTo(w * 0.8, h);
      g.lineTo(w * 0.2, h);
      g.fill();
      // Headline, flickering a little.
      g.font = `800 ${s.fs}px ${ctx.mono}`;
      g.textAlign = "center";
      g.shadowColor = `hsl(${hue} 100% 60%)`;
      g.shadowBlur = 18;
      word(g, s.text, w / 2, s.fs + 4, "#fff", `hsl(${hue} 100% 45%)`, 3);
      g.shadowBlur = 0;
      g.textAlign = "left";
      // The dancer slides left while facing right: the planted foot glides back, the other heel lifts.
      const p = clamp(t / (s.dur - 0.6), 0, 1);
      const x = s.from + (s.to - s.from) * p, floor = h - 4, k = (h - s.fs - 14) / 60;
      const step = (t * 2.2) % 2, a = step % 1, planted = step < 1 ? 0 : 1;
      const foot = (leg) => {
        const own = leg === planted;
        const slide = own ? (0.5 - a) * 26 * k : (a - 0.5) * 26 * k;
        return { x: x + slide + (leg ? 6 : -6) * k, lift: own ? 0 : Math.sin(a * Math.PI) * 10 * k, toe: own ? 0 : 1 };
      };
      const hip = { x, y: floor - 30 * k };
      g.lineCap = "round";
      for (const leg of [1, 0]) {
        const f = foot(leg);
        g.strokeStyle = "#4a4a5c";
        g.lineWidth = 7 * k;
        g.beginPath();
        g.moveTo(hip.x + (leg ? 3 : -3) * k, hip.y);
        g.lineTo(f.x, floor - 5 * k - f.lift);
        g.stroke();
        g.fillStyle = "#fff"; // white sock
        g.fillRect(f.x - 4 * k, floor - 9 * k - f.lift, 9 * k, 4 * k);
        g.fillStyle = "#000"; // loafer, heel up on the lifted leg
        g.fillRect(f.x - 5 * k, floor - 5 * k - f.lift, 14 * k, 5 * k);
      }
      g.fillStyle = "#4a4a5c"; // jacket
      g.fillRect(x - 9 * k, hip.y - 26 * k, 18 * k, 28 * k);
      g.fillStyle = "#fff"; // sequin glove
      g.fillRect(x + 8 * k, hip.y - 8 * k, 5 * k, 5 * k);
      g.fillStyle = "#e2b48a"; // face
      g.fillRect(x - 5 * k, hip.y - 40 * k, 11 * k, 13 * k);
      g.fillStyle = "#3a3a4a"; // fedora
      g.fillRect(x - 10 * k, hip.y - 43 * k, 21 * k, 4 * k);
      g.fillRect(x - 6 * k, hip.y - 52 * k, 13 * k, 10 * k);
      g.fillStyle = "#fff";
      g.fillRect(x - 6 * k, hip.y - 45 * k, 13 * k, 2 * k);
      g.fillStyle = "#000"; // shades
      g.fillRect(x + 1 * k, hip.y - 36 * k, 6 * k, 3 * k);
      // A glint on the glove on the beat.
      if (t % 0.46 < 0.12) {
        g.strokeStyle = "#fff";
        g.lineWidth = 2;
        g.beginPath();
        g.moveTo(x + 10 * k, hip.y - 14 * k);
        g.lineTo(x + 10 * k, hip.y + 2 * k);
        g.moveTo(x + 2 * k, hip.y - 5 * k);
        g.lineTo(x + 18 * k, hip.y - 5 * k);
        g.stroke();
      }
    },
  };

  // The amount bounces in and bursts into coins that fall and bounce off the frame's edges and
  // off each other.
  SCENES.coins = {
    init(g, w, h, d, ctx) {
      // One coin for every 10 of the amount (at least 5, at most 250), thrown over about three
      // seconds; with many, smaller ones so they still fit.
      const amount = parseFloat(String(d.amount).replace(/[^0-9.]/g, "")) || 100;
      const n = clamp(Math.round(amount / 10), 5, 250);
      const coins = Array.from({ length: n }, (_, i) => ({
        x: w / 2, y: h * 0.45, vx: rand(-w * 0.45, w * 0.45), vy: rand(-h * 3.2, -h * 1.2),
        spin: rand(0, 6), at: 0.15 + i * Math.min(0.035, 2.5 / n),
      }));
      return {
        coins, dur: 6, fs: clamp(h * 0.34, 26, 60), sym: (String(d.amount).match(/[^\d.,\s]+/) || ["$"])[0],
        floor: h - 14, r: n > 120 ? 8 : n > 60 ? 10 : 12,
      };
    },
    draw(g, w, h, s, t, ctx, d) {
      const dt = ctx.dt, R = s.r;
      const live = s.coins.filter((c) => t >= c.at);
      for (const c of live) {
        c.vy += 1500 * dt;
        c.x += c.vx * dt;
        c.y += c.vy * dt;
        c.spin += dt * 9;
        if (c.y > s.floor && c.vy > 0) {
          c.y = s.floor;
          c.vy = c.vy < 90 ? 0 : -c.vy * 0.5;
          c.vx *= 0.82;
        }
        if (c.x < R || c.x > w - R) {
          c.x = c.x < R ? R : w - R;
          c.vx *= -0.6;
        }
        if (c.y < R) {
          c.y = R;
          c.vy *= -0.6;
        }
      }
      // Coins hitting coins: pushed apart, and the approach speed along the line between them
      // is exchanged (a little lost).
      for (let i = 0; i < live.length; i++) {
        for (let j = i + 1; j < live.length; j++) {
          const a = live[i], b = live[j];
          const dx = b.x - a.x, dy = b.y - a.y, dist = Math.hypot(dx, dy);
          if (dist >= 2 * R || dist === 0) continue;
          const nx = dx / dist, ny = dy / dist, push = (2 * R - dist) / 2;
          a.x -= nx * push; a.y -= ny * push;
          b.x += nx * push; b.y += ny * push;
          const vn = (a.vx - b.vx) * nx + (a.vy - b.vy) * ny;
          if (vn > 0) {
            const jn = vn * 0.8;
            a.vx -= jn * nx; a.vy -= jn * ny;
            b.vx += jn * nx; b.vy += jn * ny;
          }
        }
      }
      for (const c of live) {
        if (c.x < -20 || c.x > w + 20) continue;
        const sq = Math.abs(Math.cos(c.spin));
        g.fillStyle = "#c58a00";
        g.beginPath();
        g.ellipse(c.x, c.y, (R + 1) * sq + 1, R + 1, 0, 0, 7);
        g.fill();
        g.fillStyle = "#ffd23a";
        g.beginPath();
        g.ellipse(c.x, c.y, (R - 2) * sq + 1, R - 2, 0, 0, 7);
        g.fill();
      }
      // The amount, bouncing in (a damped spring).
      const q = clamp(t / 0.9, 0, 1);
      const scale = 1 + Math.exp(-5 * q) * Math.cos(q * 14) * 0.9 - Math.exp(-5);
      g.save();
      g.translate(w / 2, h * 0.5);
      g.scale(scale, scale);
      g.textAlign = "center";
      g.font = `900 ${s.fs * 1.6}px ${ctx.mono}`;
      word(g, d.member ? "MEMBER" : d.amount, 0, s.fs * 0.5, "#ffd23a", "#3a2a00", 8);
      g.font = `700 ${s.fs * 0.55}px ${ctx.mono}`;
      word(g, d.author, 0, -s.fs * 0.85, ctx.fg, "#000", 4);
      g.restore();
      g.textAlign = "left";
    },
  };

  // A neon sign powers on letter by letter, buzzing, with sparks falling off it.
  SCENES.neon = {
    init(g, w, h, d, ctx) {
      const fs = clamp(h * 0.34, 28, 66);
      g.font = `800 ${fs}px ${ctx.mono}`;
      const text = headline(d);
      const chars = [...text].map((ch) => ({ ch, on: rand(0.3, 1.8), shown: false }));
      return { fs, chars, text, tw: g.measureText(text).width, dur: 6.4, sparks: [], next: 1, buzz: false };
    },
    draw(g, w, h, s, t, ctx) {
      g.font = `800 ${s.fs}px ${ctx.mono}`;
      let x = (w - s.tw) / 2;
      const y = h * 0.5 + s.fs * 0.3;
      const off = t > s.dur - 1;
      const hues = [330, 190];
      let n = 0;
      for (const c of s.chars) {
        const cw = g.measureText(c.ch).width;
        const lit = t > c.on && !off && (t > c.on + 0.25 || Math.sin(t * 90) > -0.2);
        const hue = hues[n++ % 2] + (c.ch === " " ? 0 : 0);
        g.shadowColor = `hsl(${hue} 100% 60%)`;
        g.fillStyle = lit ? `hsl(${hue} 100% 92%)` : `hsl(${hue} 30% 25%)`;
        g.shadowBlur = lit ? 26 + Math.sin(t * 40) * 4 : 0;
        g.fillText(c.ch, x, y);
        g.fillText(c.ch, x, y);
        if (lit && Math.random() < ctx.dt * 0.6) s.sparks.push({ x: x + cw / 2, y, vx: rand(-40, 40), vy: rand(-60, 0), life: 1 });
        x += cw;
      }
      g.shadowBlur = 0;
      // Sparks.
      for (const p of s.sparks) {
        p.vy += 500 * ctx.dt;
        p.x += p.vx * ctx.dt;
        p.y += p.vy * ctx.dt;
        p.life -= ctx.dt * 1.2;
        g.fillStyle = `hsl(45 100% 70% / ${Math.max(0, p.life)})`;
        g.fillRect(p.x, p.y, 3, 3);
      }
      s.sparks = s.sparks.filter((p) => p.life > 0);
      // A tube frame around it, once the sign has mostly lit.
      if (t > 2 && !off) {
        g.strokeStyle = `hsl(${hues[1]} 100% 80%)`;
        g.shadowColor = `hsl(${hues[1]} 100% 60%)`;
        g.shadowBlur = 16;
        g.lineWidth = 3;
        g.beginPath();
        g.roundRect((w - s.tw) / 2 - 28, y - s.fs - 6, s.tw + 56, s.fs * 1.5, 22);
        g.stroke();
        g.shadowBlur = 0;
      }
    },
  };

  window.aftercastAlert = {
    skins: Object.keys(SCENES),
    // One frame at second `at`, stepped from 0 in 1/60 s (for looking at a skin without
    // waiting for it).
    still(host, d, skin, at) {
      const scene = SCENES[skin] || SCENES.coins;
      const w = Math.min(host.clientWidth, MAXW), h = host.clientHeight;
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      host.replaceChildren(canvas);
      const g = canvas.getContext("2d");
      const mono = `"${css("--font-mono", "monospace").replace(/"/g, "")}", monospace`;
      const ctx = { mono, fg: css("--fg", "#fff"), accent: css("--accent", "#ff5555"), muted: css("--muted", "#9a9a9a"), dt: 1 / 60 };
      const s = scene.init(g, w, h, d, ctx);
      for (let t = 0; t <= at; t += 1 / 60) {
        g.clearRect(0, 0, w, h);
        g.save();
        g.globalAlpha = clamp(Math.min(t / 0.25, (s.dur - t) / 0.5), 0, 1);
        scene.draw(g, w, h, s, t, ctx, d);
        g.restore();
      }
    },
    play(host, d, skin) {
      return new Promise((done) => {
        const scene = SCENES[skin] || SCENES.coins;
        const w = Math.min(host.clientWidth || 1200, MAXW), h = host.clientHeight || 170;
        const canvas = document.createElement("canvas");
        canvas.width = w;
        canvas.height = h;
        canvas.className = "alert-canvas";
        host.replaceChildren(canvas);
        const g = canvas.getContext("2d");
        const mono = `"${css("--font-mono", "monospace").replace(/"/g, "")}", monospace`;
        const ctx = { mono, fg: css("--fg", "#fff"), accent: css("--accent", "#ff5555"), muted: css("--muted", "#9a9a9a") };
        const s = scene.init(g, w, h, d, ctx);
        const t0 = performance.now();
        let stop = false, prev = 0;
        const frame = (now) => {
          if (stop) return;
          const t = (now - t0) / 1000;
          ctx.dt = clamp(t - prev, 0, 0.05);
          prev = t;
          g.clearRect(0, 0, w, h);
          g.save();
          g.globalAlpha = clamp(Math.min(t / 0.25, (s.dur - t) / 0.5), 0, 1);
          scene.draw(g, w, h, s, t, ctx, d);
          g.restore();
          if (t >= s.dur) {
            canvas.remove();
            return done();
          }
          requestAnimationFrame(frame);
        };
        requestAnimationFrame(frame);
        // A hidden page does not run animation frames: a timer ends it anyway.
        setTimeout(() => { stop = true; canvas.remove(); done(); }, s.dur * 1000 + 3000);
      });
    },
  };
})();
