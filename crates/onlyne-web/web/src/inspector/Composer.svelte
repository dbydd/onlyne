<script lang="ts">
  // The surface's primary action: write a task to this board.
  //
  // The draft is cleared only when the op landed. A refused send keeps its
  // text, because that text is the operator's only copy of what they were
  // about to ask for, and retyping it to find out whether the button works is
  // the worse failure of the two.
  import PaperPlaneTilt from 'phosphor-svelte/lib/PaperPlaneTilt';
  import { OPERATOR_ROLE } from '../lib/model';
  import { sendTask } from '../lib/ops';

  interface Props {
    role: string;
  }

  let { role }: Props = $props();

  let draft = $state('');
  let sending = $state(false);
  let area: HTMLTextAreaElement | undefined = $state();

  const ready = $derived(draft.trim() !== '' && !sending);

  async function send() {
    if (!ready) return;
    const text = draft.trim();
    sending = true;
    try {
      if (await sendTask(role, text)) {
        draft = '';
        area?.focus();
      }
    } finally {
      sending = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    // Cmd or Ctrl with Enter is how a field is submitted without leaving the
    // keyboard; a plain Enter has to stay a newline, because the task text is
    // prose and the operator has no reason to write it on one line.
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void send();
    }
  }
</script>

<div class="composer">
  <label class="sr-only" for="task-draft-{role}">task for {role}</label>
  <textarea
    id="task-draft-{role}"
    class="textarea"
    bind:this={area}
    bind:value={draft}
    onkeydown={onKeydown}
    rows="3"
    placeholder="task for {role}"
    disabled={sending}
  ></textarea>
  <div class="foot">
    <span class="note">goes as {OPERATOR_ROLE}</span>
    <div class="keys"><span class="kbd">Cmd</span><span class="kbd">Enter</span></div>
    <button class="btn primary" onclick={() => void send()} disabled={!ready} aria-busy={sending}>
      {#if sending}<span class="working">sending</span>{:else}<PaperPlaneTilt size="12" weight="fill" /> Send{/if}
    </button>
  </div>
</div>

<style>
  .composer {
    display: grid;
    gap: var(--s-2);
    padding: var(--s-3);
    border-bottom: 1px solid var(--line-soft);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .note {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-11);
    color: var(--ink-4);
  }
  .keys {
    display: flex;
    gap: 3px;
    color: var(--ink-4);
  }
  .working {
    font-size: var(--fs-11);
  }
</style>