<script lang="ts">
  // The spec half of one role's entry, one control per field group.
  //
  // Every control writes its own group and carries its own pending state,
  // because `upsert_role` moves only the keys an edit names: one combined save
  // would be one confirmation over four independent facts, and a half-written
  // pair of groups would leave the operator unable to say which half moved.
  //
  // A field this component holds is authoritative only while the field is
  // focused. A field keeps what the operator typed across the reload that
  // follows an edit, which is what makes editing prose possible at all: that
  // reload arrives in under a second and would otherwise overwrite the sentence
  // under their cursor. So a field adopts the spec's value when the field is
  // not focused, and is left alone from the moment it is.
  import ConfirmButton from '../lib/ui/ConfirmButton.svelte';
  import Minus from 'phosphor-svelte/lib/Minus';
  import Plus from 'phosphor-svelte/lib/Plus';
  import type { Drive } from '../gen/WebOp';
  import type { SpecBuilder } from '../lib/ops';
  import type { SpecClient } from '../lib/state/spec.svelte';
  import { editSpec } from '../lib/ops';
  import { ui } from '../lib/state/ui.svelte';

  interface Props {
    role: string;
    /// The spec's own reading of this role. Undefined while the read is in
    /// flight or after one that failed, and every control says so rather than
    /// showing values it cannot see.
    client?: SpecClient;
  }

  let { role, client }: Props = $props();

  const DRIVES: Drive[] = ['plugin', 'acp', 'exec'];

  /// The four numbers one `set_session` carries, in the order they are typed:
  /// the two timeouts, the attempt count, then the backoff ladder.
  const POLICY_MIN = 3;

  function policyOf(entry?: SpecClient): string {
    if (!entry) return '';
    return [entry.readyMs, entry.idleMs, entry.attempts, ...entry.backoffMs].join(', ');
  }

  function argvOf(entry?: SpecClient): string {
    return (entry?.command ?? []).join('\n');
  }

  let sessions = $state(1);
  let prose = $state('');
  let drive = $state<Drive>('plugin');
  let argv = $state('');
  let policy = $state('');
  /// The field under the cursor, named. One holder for five fields, because a
  /// reload moves all of them and each has to decide for itself whether it is
  /// being typed into.
  let focused: string | null = $state(null);
  let saving = $state<string | null>(null);
  let dropping = $state(false);
  let policyError = $state('');

  $effect(() => {
    const entry = client;
    if (focused !== 'sessions') sessions = entry?.maxSessions ?? 1;
    if (focused !== 'prose') prose = entry?.prose ?? '';
    if (focused !== 'runtime') drive = entry?.drive ?? 'plugin';
    if (focused !== 'runtime') argv = argvOf(entry);
    if (focused !== 'policy') policy = policyOf(entry);
  });

  /// One control at a time. A spec write is a file rewrite and a reload, and
  /// two of them racing would leave the second landing against the first one's
  /// outcome with no way to say which was which.
  const quiet = $derived(saving !== null || dropping);

  async function save(label: string, build: SpecBuilder) {
    if (quiet) return;
    saving = label;
    try {
      await editSpec(label, build);
    } finally {
      saving = null;
    }
  }

  function step(delta: number) {
    sessions = Math.max(0, sessions + delta);
  }

  /// Whole non-negative numbers only, because these are milliseconds and counts
  /// and a decimal in either is a value the server will refuse after a round
  /// trip. Rejecting it here keeps the answer on the field that caused it.
  function parsePolicy(text: string): number[] | null {
    if (text.trim() === '') return null;
    const parts = text.split(',').map((part) => part.trim());
    if (parts.some((part) => part === '')) return null;
    const values = parts.map(Number);
    if (!values.every((value) => Number.isInteger(value) && value >= 0)) return null;
    return values.length >= POLICY_MIN ? values : null;
  }

  function savePolicy() {
    const values = parsePolicy(policy);
    if (!values) {
      policyError = `ready_ms, idle_ms, attempts, then the backoff ladder. whole numbers only, at least ${POLICY_MIN} of them.`;
      return;
    }
    policyError = '';
    void save('policy', () => [
      {
        edit: 'set_session',
        args: {
          role,
          ready_ms: values[0],
          idle_ms: values[1],
          attempts: values[2],
          backoff_ms: values.slice(3),
        },
      },
    ]);
  }

  function saveRuntime() {
    const command = argv
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '');
    void save('runtime', () => [{ edit: 'set_runtime', args: { role, runtime: { drive, command } } }]);
  }

  async function removeRole() {
    if (quiet) return;
    dropping = true;
    try {
      // A remove that landed takes this panel's own subject with it, so the
      // selection is cleared rather than left on a role that is gone.
      if (await editSpec(`remove ${role}`, () => [{ edit: 'remove_role', args: { role } }])) ui.clear();
    } finally {
      dropping = false;
    }
  }
</script>

{#if !client}
  <p class="absent">the spec entry for this role has not been read. the controls below appear once it is.</p>
{:else}
  <div class="config">
    <div class="field">
      <span class="label">max_sessions</span>
      <div class="stepper">
        <button class="btn icon sm" aria-label="fewer sessions" disabled={quiet || sessions <= 0} onclick={() => step(-1)}>
          <Minus size="12" weight="bold" />
        </button>
        <span class="num value">{sessions}</span>
        <button class="btn icon sm" aria-label="more sessions" disabled={quiet} onclick={() => step(1)}>
          <Plus size="12" weight="bold" />
        </button>
        <button
          class="btn sm"
          aria-busy={saving === 'max sessions'}
          disabled={quiet}
          onclick={() => void save('max sessions', () => [{ edit: 'upsert_role', args: { role, max_sessions: sessions } }])}
        >
          {saving === 'max sessions' ? 'saving' : 'save'}
        </button>
      </div>
      <span class="hint">sessions this role may run at once. it reaches a client at its next hello.</span>
    </div>

    <div class="field">
      <label class="label" for="prose-{role}">prose</label>
      <textarea
        id="prose-{role}"
        class="textarea"
        rows="3"
        bind:value={prose}
        onfocus={() => (focused = 'prose')}
        onblur={() => (focused = null)}
      ></textarea>
      <span class="hint">what this role is for. the next session that opens reads it; a session already running keeps the prose it opened with.</span>
      <div class="save-row">
        <button
          class="btn sm"
          aria-busy={saving === 'prose'}
          disabled={quiet || prose === ''}
          onclick={() => void save('prose', () => [{ edit: 'set_prose', args: { role, prose } }])}
        >
          {saving === 'prose' ? 'saving' : 'save prose'}
        </button>
      </div>
    </div>

    <div class="field">
      <label class="label" for="policy-{role}">timeouts and retries</label>
      <input
        id="policy-{role}"
        class="input"
        inputmode="numeric"
        autocomplete="off"
        spellcheck="false"
        placeholder="ready_ms, idle_ms, attempts, backoff_ms"
        bind:value={policy}
        onfocus={() => (focused = 'policy')}
        onblur={() => (focused = null)}
        aria-invalid={policyError !== '' ? 'true' : 'false'}
      />
      {#if policyError}
        <span class="error">{policyError}</span>
      {:else}
        <span class="hint">ready_ms, idle_ms, attempts, then the backoff ladder. whole numbers, comma separated. these reach sessions opened after the reload.</span>
      {/if}
      <div class="save-row">
        <button class="btn sm" aria-busy={saving === 'policy'} disabled={quiet} onclick={() => savePolicy()}>
          {saving === 'policy' ? 'saving' : 'save policy'}
        </button>
      </div>
    </div>

    <div class="field">
      <label class="label" for="drive-{role}">drive</label>
      <select
        id="drive-{role}"
        class="select"
        bind:value={drive}
        onfocus={() => (focused = 'runtime')}
        onblur={() => (focused = null)}
        disabled={quiet}
      >
        {#each DRIVES as option (option)}
          <option value={option}>{option}</option>
        {/each}
      </select>
    </div>

    <div class="field">
      <label class="label" for="argv-{role}">argv</label>
      <textarea
        id="argv-{role}"
        class="textarea mono"
        rows="3"
        spellcheck="false"
        placeholder={'one argument per line\n{session} and {task} are substituted by the client'}
        bind:value={argv}
        onfocus={() => (focused = 'argv')}
        onblur={() => (focused = null)}
      ></textarea>
      <span class="hint">acp requires a headless placement, which is a property of the machine that runs the client, so plugin is the safe default.</span>
      <div class="save-row">
        <button class="btn sm" aria-busy={saving === 'runtime'} disabled={quiet} onclick={() => saveRuntime()}>
          {saving === 'runtime' ? 'saving' : 'save runtime'}
        </button>
      </div>
    </div>

    <div class="field inline">
      <span class="label">admin</span>
      {#if client.admin}
        <span class="chip line">admin</span>
      {:else}
        <span class="hint">off. it is read from spec.toml and is not set from here.</span>
      {/if}
    </div>

    <div class="field danger">
      <span class="label">remove role</span>
      <span class="hint">drops the entry from spec.toml. the workspace on that role's machine is untouched, and deliveries already queued stay queued.</span>
      <div class="save-row">
        <ConfirmButton
          label="Remove role"
          confirm="Remove {role}"
          busy={dropping}
          title="remove {role} from spec.toml"
          disabled={quiet}
          onconfirm={() => void removeRole()}
        />
      </div>
    </div>
  </div>
{/if}

<style>
  .config {
    display: grid;
    gap: var(--s-3);
  }
  .absent {
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .stepper {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .value {
    min-width: 3ch;
    text-align: center;
    color: var(--ink);
  }
  .save-row {
    display: flex;
    justify-content: flex-end;
  }
  .inline {
    display: flex;
    align-items: center;
    gap: var(--s-2);
  }
  .danger {
    padding-top: var(--s-2);
    border-top: 1px solid var(--line-soft);
  }
</style>