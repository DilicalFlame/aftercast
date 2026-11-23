<script>
  // The SDK is loaded by the host before this bundle; its stores work with $ in Svelte.
  const ac = window.aftercast;
  const params = ac.params;
  const now = ac.state("now");

  let tick = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (tick = ac.now()), 1000);
    return () => clearInterval(t);
  });

  const clock = (ms) => {
    const s = Math.max(0, Math.floor(ms / 1000));
    const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, r = s % 60;
    const pad = (n) => String(n).padStart(2, "0");
    return h ? `${h}:${pad(m)}:${pad(r)}` : `${m}:${pad(r)}`;
  };

  const data = $derived($now && $now.action === "show" ? $now.data : null);
</script>

{#if data}
  <div class="card" style:--tint={$params.accent}>
    <span class="label">{$params.label}</span>
    <span class="quest">{data.quest}</span>
    {#if $params.show_timer && data.live_since_ms}
      <span class="timer">{clock(tick - data.live_since_ms)}</span>
    {/if}
  </div>
{/if}

<style>
  .card {
    --tint: var(--accent);
    display: flex;
    align-items: center;
    gap: 18px;
    padding: 18px 26px;
    border-radius: 18px;
    background: var(--bg);
    box-shadow: inset 0 0 0 2px var(--tint);
    font: 600 30px/1.2 var(--font-sans), sans-serif;
    color: var(--fg);
  }
  .label { padding: 4px 14px; border-radius: 10px; background: var(--tint); color: #111; font-size: 22px; font-weight: 800; }
  .timer { margin-left: auto; font: 700 28px/1 var(--font-mono), monospace; color: var(--tint); }
  :global([data-skin="light"]) .card { background: #fff; color: #111; box-shadow: 0 10px 30px rgb(0 0 0 / 25%); }
</style>
