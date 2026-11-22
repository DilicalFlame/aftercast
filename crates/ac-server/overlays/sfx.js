// Sound is files: a sound skin is a folder in <home>/overlays/sounds/ and the alert plays the
// file named after its animation (dino.wav, pacman.wav, moonwalk.wav, coins.wav, neon.wav),
// else alert.wav, once when it starts. Nothing is made in code.
// window.aftercastSfx: alert(animation), volume(0..1), enabled(bool), unlock(), useSkin(folder).
(() => {
  let ctx = null;
  let master = null;
  let level = 0.7;
  let on = true;
  let skin = "";
  let samples = {};
  let loading = false;
  let chosen = null, chosenPath = "";
  const audio = () => {
    if (!ctx) {
      ctx = new (window.AudioContext || window.webkitAudioContext)();
      master = ctx.createGain();
      master.connect(ctx.destination);
    }
    master.gain.value = level;
    return ctx;
  };
  window.aftercastSfx = {
    // Plays the file of an alert animation (or alert.*), if the sound skin has one.
    alert(animation) {
      // The animation's own file, else alert.*, else the only file in the folder.
      const all = Object.values(samples);
      const buf = chosen || samples[animation] || samples.alert || (all.length === 1 ? all[0] : null);
      if (!on || !buf) return;
      try {
        const c = audio();
        const s = c.createBufferSource();
        s.buffer = buf;
        s.connect(master);
        s.start();
      } catch { /* no audio device */ }
    },
    // The one file chosen for the alert, as folder/file.mp3 ("none": go by the animation's name).
    async useFile(path) {
      if (!path || path === "none") { chosen = null; chosenPath = ""; return; }
      if (path === chosenPath) return;
      chosenPath = path;
      try {
        const bytes = await (await fetch(`/overlay/sounds/${path}`)).arrayBuffer();
        const buf = await audio().decodeAudioData(bytes);
        if (chosenPath === path) chosen = buf;
      } catch { chosenPath = ""; }
    },
    volume(v) { level = Math.min(1, Math.max(0, v)); if (master) master.gain.value = level; },
    enabled(v) { on = !!v; },
    unlock() { try { audio().resume(); } catch { /* no audio device */ } },
    // Loads a sound skin's files (decoded once).
    async useSkin(name) {
      // A folder that was empty (or missing) is looked at again: files may have been added.
      if (!name || (name === skin && (Object.keys(samples).length || loading))) return;
      skin = name;
      samples = {};
      loading = true;
      try {
        const files = await (await fetch(`/overlay/sounds/${encodeURIComponent(name)}/index.json`)).json();
        const loaded = {};
        await Promise.all(files.map(async (f) => {
          const bytes = await (await fetch(`/overlay/sounds/${encodeURIComponent(name)}/${encodeURIComponent(f)}`)).arrayBuffer();
          loaded[f.replace(/\.[^.]+$/, "").toLowerCase()] = await audio().decodeAudioData(bytes);
        }));
        if (skin === name) samples = loaded;
      } catch { /* no such folder: silence */ }
      loading = false;
    },
  };
})();
