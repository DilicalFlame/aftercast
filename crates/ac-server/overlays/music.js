// The BRB's music: plays a playlist (a folder in <home>/overlays/playlists/) in a shuffled
// loop, shows the track's album card and name, and draws bars that move with the sound.
// window.aftercastMusic.mount({ cover, title, artist, album, bars }, playlist) returns
// { setPlaying(bool), setVolume(0..1), stop() }. Without sound (muted, or the editor's sample)
// the card still shows and the bars breathe on their own.
(() => {
  const css = (name, fallback) => getComputedStyle(document.body).getPropertyValue(name).trim() || fallback;
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const shuffle = (list) => {
    const a = [...list];
    for (let i = a.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [a[i], a[j]] = [a[j], a[i]];
    }
    return a;
  };

  window.aftercastMusic = {
    mount(parts, playlist, shuffleOn = true) {
      const audio = new Audio();
      audio.preload = "auto";
      let tracks = [], index = 0, playing = false, stopped = false, failures = 0;
      let ctx = null, analyser = null, bins = null, raf = 0;
      const url = (kind, file) => `/overlay/${kind}/${encodeURIComponent(playlist)}/${encodeURIComponent(file)}`;

      const show = (i) => {
        if (!tracks.length) return;
        index = (i + tracks.length) % tracks.length;
        const t = tracks[index];
        parts.title.textContent = t.title || t.file;
        parts.artist.textContent = t.artist || "";
        parts.album.textContent = t.album || "";
        parts.cover.classList.remove("none");
        // A track without art gets a colored cover of its own (one color per title).
        let hue = 0;
        for (const c of t.title || t.file) hue = (hue * 31 + c.charCodeAt(0)) % 360;
        parts.cover.style.setProperty("--hue", hue);
        parts.cover.firstElementChild.onerror = () => parts.cover.classList.add("none");
        parts.cover.firstElementChild.src = url("cover", t.file);
        // The words slide in with each track.
        for (const e of [parts.title.parentElement]) {
          e.animate([{ opacity: 0, transform: "translateY(10px)" }, { opacity: 1, transform: "none" }], { duration: 450, easing: "ease-out" });
        }
        audio.src = `/overlay/music/${encodeURIComponent(playlist)}/${encodeURIComponent(t.file)}`;
        if (playing) audio.play().catch(() => {});
      };
      const next = () => show(index + 1);
      // A track that cannot be played is skipped; if none can, stop trying.
      audio.onended = () => { failures = 0; next(); };
      audio.onerror = () => { if (++failures < Math.max(2, tracks.length)) next(); };

      fetch(`/overlay/music/${encodeURIComponent(playlist)}/index.json`)
        .then((r) => r.json())
        .then((list) => {
          // No files in the folder: no music half at all.
          if (!list.length) { parts.cover.closest(".music").hidden = true; return; }
          tracks = shuffleOn ? shuffle(list) : list;
          show(0);
        })
        .catch(() => {});

      // Bars: drawn from the analyser when the sound plays, else a slow swell.
      const canvas = parts.bars;
      const g = canvas.getContext("2d");
      const N = 44;
      const level = new Array(N).fill(0.1);
      const draw = (now) => {
        if (stopped) return;
        raf = requestAnimationFrame(draw);
        const w = canvas.clientWidth, h = canvas.clientHeight;
        if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
        const t = now / 1000;
        if (playing && analyser) analyser.getByteFrequencyData(bins);
        const accent = css("--accent", "#ff5555"), fg = css("--fg", "#fff");
        g.clearRect(0, 0, w, h);
        const step = w / N, bw = step * 0.62;
        const grad = g.createLinearGradient(0, h, 0, 0);
        grad.addColorStop(0, accent);
        grad.addColorStop(1, fg);
        g.fillStyle = grad;
        let bass = 0;
        for (let b = 0; b < N; b++) {
          let v;
          if (playing && analyser) {
            // Low pitches get more bars than high ones, as they do on a spectrum.
            const i = Math.floor(Math.pow(b / N, 1.8) * bins.length * 0.8);
            v = (bins[i] / 255) ** 1.3;
          } else {
            v = 0.13 + 0.1 * Math.sin(t * 1.6 + b * 0.45) + 0.05 * Math.sin(t * 3.1 + b * 0.9);
          }
          level[b] += (v - level[b]) * (v > level[b] ? 0.5 : 0.14);
          if (b < 4) bass += level[b] / 4;
          const bh = Math.max(4, level[b] * h);
          g.beginPath();
          g.roundRect(b * step + (step - bw) / 2, (h - bh) / 2, bw, bh, bw / 2);
          g.fill();
        }
        // The album card breathes with the low end.
        parts.cover.style.transform = `scale(${1 + bass * 0.045})`;
      };
      raf = requestAnimationFrame(draw);

      const tap = () => {
        if (ctx) return;
        ctx = new (window.AudioContext || window.webkitAudioContext)();
        const src = ctx.createMediaElementSource(audio);
        analyser = ctx.createAnalyser();
        analyser.fftSize = 256;
        analyser.smoothingTimeConstant = 0.78;
        bins = new Uint8Array(analyser.frequencyBinCount);
        src.connect(analyser);
        analyser.connect(ctx.destination);
      };

      return {
        setPlaying(on) {
          if (stopped || on === playing) return;
          playing = on;
          if (on) {
            try { tap(); ctx.resume(); } catch { /* no audio device */ }
            audio.play().catch(() => {});
          } else audio.pause();
        },
        setVolume(v) { audio.volume = clamp(v, 0, 1); },
        stop() {
          stopped = true;
          cancelAnimationFrame(raf);
          audio.pause();
          audio.removeAttribute("src");
          if (ctx) ctx.close().catch(() => {});
        },
      };
    },
  };
})();
