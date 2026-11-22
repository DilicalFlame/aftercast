// Going back in the recording: plays OBS's fragmented mp4 (served by /rehearse/dvr/) in a video
// element through a media source, from the fragment before the wanted time, and keeps feeding
// it as the file grows. window.aftercastDvr: index() -> { duration, ... } | null,
// open(video, seconds), close(). Seeking is the video element's own: a time outside what is
// buffered starts again from the fragment there.
(() => {
  const AHEAD = 25, BEHIND = 60;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  let video = null, source = null, buf = null, idx = null, pos = 0, token = 0, onSeek = null, autoplay = true;

  const getIndex = async () => {
    const r = await fetch("/rehearse/dvr/index.json", { cache: "no-store" });
    if (!r.ok) return null;
    const i = await r.json();
    const f = i.fragments, last = f[f.length - 1][0];
    // The length of the last fragment is not known yet: take the one before it.
    i.duration = (last + (f.length > 1 ? last - f[f.length - 2][0] : 4000)) / 1000;
    return i;
  };
  // Whole boxes from byte `a` (a box boundary), as many as one answer holds.
  const bytes = async (a) => {
    const r = await fetch("/rehearse/dvr/file", { headers: { Range: `bytes=${a}-` } });
    if (!r.ok) return new Uint8Array(0);
    return new Uint8Array(await r.arrayBuffer());
  };
  const idle = () => (buf.updating ? new Promise((r) => buf.addEventListener("updateend", r, { once: true })) : null);
  const append = async (data) => {
    await idle();
    await new Promise((res, rej) => {
      buf.addEventListener("updateend", res, { once: true });
      buf.addEventListener("error", () => rej(new Error("append failed")), { once: true });
      buf.appendBuffer(data);
    });
  };
  const remove = async (a, b) => {
    await idle();
    if (b <= a) return;
    await new Promise((res) => { buf.addEventListener("updateend", res, { once: true }); buf.remove(a, b); });
  };
  // The end of the buffered range holding `t`, or null.
  const rangeAt = (t) => {
    const r = video.buffered;
    for (let i = 0; i < r.length; i++) if (t >= r.start(i) - 0.05 && t <= r.end(i)) return [r.start(i), r.end(i)];
    return null;
  };
  // The media source's length follows the recording, so a seek anywhere in it is allowed.
  const grow = () => {
    try { if (!buf.updating && source.duration !== idx.duration) source.duration = Math.max(idx.duration, 0); } catch { /* shorter than the buffer */ }
  };
  // Where in the file the fragment at or before `sec` starts.
  const offsetFor = (sec) => {
    let at = idx.fragments[0];
    for (const f of idx.fragments) if (f[0] / 1000 <= sec) at = f;
    return at;
  };

  // (Re)start feeding from the fragment before `sec`.
  const startAt = async (sec) => {
    const my = ++token;
    idx = (await getIndex()) || idx;
    grow();
    // A fresh parser for a new place in the file.
    try { buf.abort(); } catch { /* not open */ }
    await idle();
    if (buf.buffered.length) await remove(buf.buffered.start(0), buf.buffered.end(buf.buffered.length - 1));
    await append((await bytes(0)).subarray(0, idx.init_end));
    const [t0, off] = offsetFor(sec);
    pos = off;
    let first = true;
    while (my === token) {
      const here = rangeAt(video.currentTime);
      const ahead = here ? here[1] - video.currentTime : 0;
      if (!first && ahead > AHEAD) { await sleep(300); continue; }
      if (pos >= idx.end) {
        // The recording has not grown yet: look again in a moment.
        await sleep(1500);
        idx = (await getIndex()) || idx;
        grow();
        continue;
      }
      const data = await bytes(pos);
      if (my !== token) return;
      if (!data.length) { await sleep(1000); continue; }
      await append(data);
      pos += data.length;
      if (first && rangeAt(Math.max(sec, t0 / 1000))) {
        first = false;
        video.currentTime = sec;
        if (autoplay) video.play().catch(() => {});
      }
      // Forget what is far behind.
      const b = rangeAt(video.currentTime);
      if (b && video.currentTime - b[0] > BEHIND) await remove(b[0], video.currentTime - BEHIND / 2);
    }
  };

  window.aftercastDvr = {
    index: getIndex,
    async open(el, sec, paused = false) {
      this.close();
      autoplay = !paused;
      video = el;
      source = new MediaSource();
      video.src = URL.createObjectURL(source);
      await new Promise((r) => source.addEventListener("sourceopen", r, { once: true }));
      idx = await getIndex();
      if (!idx) throw new Error("no fragmented mp4 recording");
      buf = source.addSourceBuffer(`video/mp4; codecs="${idx.codecs}"`);
      onSeek = () => {
        const t = video.currentTime;
        // A seek keeps playing or staying paused, as the player was.
        autoplay = !video.paused;
        if (!rangeAt(t)) startAt(t).catch((e) => console.warn("dvr:", e));
      };
      video.addEventListener("seeking", onSeek);
      startAt(sec).catch((e) => console.warn("dvr:", e));
    },
    close() {
      token++;
      if (video && onSeek) video.removeEventListener("seeking", onSeek);
      if (video) { video.pause(); video.removeAttribute("src"); video.load(); }
      video = source = buf = onSeek = null;
    },
  };
})();
