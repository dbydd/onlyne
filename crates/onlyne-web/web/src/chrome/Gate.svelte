<script lang="ts">
  // The two ways a launch ends before this surface can run: the tab carries no
  // startup token, or the server refused the one it carries. Neither is a
  // crash, and neither is worth a red page: both are the normal end of a
  // process that mints its token per launch, so the page stays quiet and says
  // exactly what to do next. Nothing here touches the cluster.
  import Key from 'phosphor-svelte/lib/Key';

  interface Props {
    mode: 'no-token' | 'refused';
    detail: string;
  }

  let { mode, detail }: Props = $props();

  const refused = $derived(mode === 'refused');
  /// The guard writes its sentence with the binary's own name in front of it.
  /// The heading already says whose sentence this is, so the prefix is cut and
  /// the rest is passed through untouched.
  const said = $derived(refused ? detail.replace(/^onlyne-web:\s*/, '') : '');
</script>

<div class="gate" data-tone={refused ? 'fail' : 'plain'}>
  <div class="card">
    <div class="brand">
      <span class="mark" aria-hidden="true"></span>
      <span class="wordmark">onlyne</span>
    </div>
    {#if refused}
      <span class="mark-icon" data-tone="fail" aria-hidden="true"><Key /></span>
      <h1 class="title">The token was refused</h1>
      <p class="body">The server answered this request with:</p>
      <code class="said">{said}</code>
      <p class="body">A fresh <code class="cmd">onlyne-web</code> mints a fresh token, so retrying this tab cannot help.</p>
    {:else}
      <span class="mark-icon" aria-hidden="true"><Key /></span>
      <h1 class="title">This surface needs its startup token</h1>
      <p class="body">The URL this process printed looks like:</p>
      <code class="said">http://127.0.0.1:&lt;port&gt;/?token=…</code>
      <p class="body">
        Start it with <code class="cmd">onlyne-web --open</code> and it opens that page for you. Without
        <code class="cmd">--open</code> the line it printed carries the same URL.
      </p>
      <p class="body">The token is minted per launch, so a link from an earlier launch has stopped working.</p>
    {/if}
  </div>
</div>

<style>
  .gate {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: var(--s-5);
    overflow-y: auto;
    background: var(--bg);
  }
  .card {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--s-3);
    width: min(520px, 100%);
    padding: var(--s-5);
    border-radius: var(--r-3);
    background: var(--panel);
    box-shadow: var(--shadow-pop);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .mark {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--bone);
  }
  .wordmark {
    font-size: var(--fs-13);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--ink-2);
  }
  /* The glyph reads as the thing that is missing, not as a fault: nothing here
   * wears a warning. */
  .mark-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--r-2);
    background: var(--tone-wash);
    color: var(--tone, var(--ink-3));
  }
  .title {
    font-size: var(--fs-18);
    font-weight: 600;
    color: var(--ink);
    text-wrap: balance;
  }
  .body {
    font-size: var(--fs-12);
    color: var(--ink-3);
    text-wrap: pretty;
  }
  .said {
    display: block;
    padding: 8px 10px;
    border-radius: var(--r-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line-soft);
    color: var(--ink-2);
    font-size: var(--fs-11);
    text-wrap: pretty;
    overflow-wrap: anywhere;
  }
  .cmd {
    padding: 1px 4px;
    border-radius: var(--r-1);
    background: var(--raised);
    color: var(--ink-2);
    font-size: var(--fs-11);
  }
</style>