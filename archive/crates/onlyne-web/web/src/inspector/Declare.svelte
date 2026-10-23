<script lang="ts">
  // The one form that declares a role.
  //
  // A role entry needs a `key`, and the key is minted by `onlyne-client init`
  // on the machine that will run the client, so the operator pastes it here.
  // That is why this form exists at all, and why nothing on the surface can
  // derive one for the operator.
  //
  // Everything is validated in the browser before it is sent: the four names
  // are cheap to check, and a refusal that comes back over the admin link after
  // a round trip is a worse place to learn that a role name holds an upper-case
  // letter. The server still validates, and still owns the refusal when it
  // disagrees; these checks are about not asking a question the form already
  // knows the answer to.
  import Eye from 'phosphor-svelte/lib/Eye';
  import type { Drive } from '../gen/WebOp';
  import { OPERATOR_ROLE } from '../lib/model';
  import { editSpec } from '../lib/ops';
  import { cluster } from '../lib/state/cluster.svelte';
  import { spec } from '../lib/state/spec.svelte';
  import { ui } from '../lib/state/ui.svelte';

  interface Props {
    /// Lets the form refuse a name that is already on the board, which the
    /// read is the only source for.
    role?: string;
  }

  let { role = '' }: Props = $props();

  const DRIVES: Drive[] = ['plugin', 'acp', 'exec'];
  const NAME = /^[a-z0-9_-]+$/;
  const KEY = /^ed25519\/(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
  /// The declared and the registered names both count as taken: the first is
  /// what this edit would overwrite, the second is what the machine is already
  /// running as.
  const taken = $derived(new Set([...cluster.canvasRoles, ...spec.clients.map((client) => client.role)]));

  let name = $state('');
  let key = $state('');
  let prose = $state('');
  let sessions = $state(1);
  let drive = $state<Drive>('plugin');
  let argv = $state('');
  let reveal = $state(false);
  let saving = $state(false);

  const nameError = $derived.by(() => {
    const value = name.trim();
    if (value === '') return 'a role needs a name.';
    if (value.length > 32) return 'at most 32 characters.';
    if (!NAME.test(value)) return 'lower case letters, digits, - and _ only.';
    if (value === OPERATOR_ROLE) return `${OPERATOR_ROLE} is reserved for the operator.`;
    if (taken.has(value)) return 'a role by that name already exists.';
    return '';
  });

  /// The shape only. The server decodes it, checks it against 32 bytes and
  /// refuses a key it does not hold; nothing here decides that it is right, and
  /// nothing here keeps the string once the save has landed.
  const keyError = $derived.by(() => {
    const value = key.trim();
    if (value === '') return 'the key `onlyne-client init` prints for this role.';
    if (!value.startsWith('ed25519/')) return 'the key starts with ed25519/.';
    if (!KEY.test(value)) return 'the part after the prefix is base64.';
    return '';
  });

  const sessionsError = $derived(sessions < 1 || !Number.isInteger(sessions) ? 'at least one session.' : '');
  const blocking = $derived(nameError !== '' || keyError !== '' || sessionsError !== '' || saving);

  /// A key is only ever read back while the operator is looking at it, and the
  /// reveal is a button rather than a default because this is the one field on
  /// the surface that carries a credential.
  const masked = $derived(key.length > 0 && !reveal);

  function wipe() {
    name = '';
    key = '';
    prose = '';
    sessions = 1;
    drive = 'plugin';
    argv = '';
    reveal = false;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (blocking) return;
    const role = name.trim();
    const command = argv
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line !== '');
    saving = true;
    try {
      // `runtime` never rides inside `upsert_role`: the wire type carries only
      // role, key, prose, admin and max_sessions, so a `runtime` key nested in
      // those args is dropped on the way in and would be a save that looks like
      // it worked and writes nothing. The second edit is what puts the argv in
      // the file.
      const ok = await editSpec(`declare ${role}`, () => [
        { edit: 'upsert_role', args: { role, key: key.trim(), prose: prose.trim(), max_sessions: sessions } },
        { edit: 'set_runtime', args: { role, runtime: { drive, command } } },
      ]);
      if (!ok) return;
      // The reload that follows the write is what creates the board, so the new
      // role is selected here and the panel switches to it on the next frame.
      ui.select({ kind: 'role', role });
      wipe();
    } finally {
      saving = false;
    }
  }
</script>

<form class="declare" autocomplete="off" novalidate onsubmit={save}>
  <div class="field">
    <label class="label" for="decl-role">role</label>
    <input
      id="decl-role"
      class="input mono"
      spellcheck="false"
      autocapitalize="none"
      placeholder="builder"
      bind:value={name}
      aria-invalid={name !== '' && nameError !== '' ? 'true' : 'false'}
      aria-describedby="decl-role-msg"
    />
    <span class="hint" id="decl-role-msg">
      {#if nameError && name !== ''}
        <span class="error">{nameError}</span>
      {:else}
        lower case letters, digits, - and _. at most 32 characters. not {OPERATOR_ROLE}.
      {/if}
    </span>
  </div>

  <div class="field">
    <label class="label" for="decl-key">key</label>
    <div class="keyline">
      <input
        id="decl-key"
        class="input mono"
        type={masked ? 'password' : 'text'}
        spellcheck="false"
        autocapitalize="none"
        placeholder="ed25519/..."
        bind:value={key}
        aria-invalid={key !== '' && keyError !== '' ? 'true' : 'false'}
        aria-describedby="decl-key-msg"
      />
      <button
        class="btn icon"
        type="button"
        aria-label={reveal ? 'hide the key' : 'show the key'}
        aria-pressed={reveal}
        title={reveal ? 'hide the key' : 'show the key'}
        onclick={() => (reveal = !reveal)}
      >
        <Eye size="12" />
      </button>
    </div>
    <span class="hint" id="decl-key-msg">
      {#if keyError && key !== ''}
        <span class="error">{keyError}</span>
      {:else}
        the public key `onlyne-client init` prints for this role. it is the role's credential and it is never held here.
      {/if}
    </span>
  </div>

  <div class="field">
    <label class="label" for="decl-prose">prose</label>
    <textarea
      id="decl-prose"
      class="textarea"
      rows="3"
      placeholder="what this role is for. the first session that opens reads it."
      bind:value={prose}
    ></textarea>
    <span class="hint">optional. the next session that opens on this role reads it.</span>
  </div>

  <div class="field">
    <label class="label" for="decl-sessions">max_sessions</label>
    <input
      id="decl-sessions"
      class="input num"
      type="number"
      min="1"
      step="1"
      bind:value={sessions}
      aria-invalid={sessionsError !== '' ? 'true' : 'false'}
      aria-describedby="decl-sessions-msg"
    />
    <span class="hint" id="decl-sessions-msg">
      {#if sessionsError}<span class="error">{sessionsError}</span>{:else}sessions this role may run at once.{/if}
    </span>
  </div>

  <div class="field">
    <label class="label" for="decl-drive">drive</label>
    <select id="decl-drive" class="select" bind:value={drive}>
      {#each DRIVES as option (option)}
        <option value={option}>{option}</option>
      {/each}
    </select>
    <span class="hint">acp needs a headless placement on the machine that runs the client, so plugin is the safe default.</span>
  </div>

  <div class="field">
    <label class="label" for="decl-argv">argv</label>
    <textarea
      id="decl-argv"
      class="textarea mono"
      rows="2"
      spellcheck="false"
      placeholder={'optional, one argument per line\n{session} and {task} are substituted by the client'}
      bind:value={argv}
    ></textarea>
  </div>

  <div class="foot">
    <button class="btn primary" type="submit" disabled={blocking} aria-busy={saving}>
      {saving ? 'declaring' : 'declare role'}
    </button>
  </div>

  <p class="next">
    the client and its workspace are created on that role's machine, with <code>onlyne-client init</code>. onlyne
    runs no process for you. until that client attaches, the board reads <em>declared, no client attached</em>.
  </p>
</form>

<style>
  .declare {
    display: grid;
    gap: var(--s-3);
    padding: var(--s-3);
  }
  .keyline {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .foot {
    display: flex;
    justify-content: flex-end;
  }
  .next {
    padding-top: var(--s-2);
    border-top: 1px solid var(--line-soft);
    font-size: var(--fs-11);
    color: var(--ink-3);
    text-wrap: pretty;
  }
  .next code,
  .next em {
    font-family: var(--font-mono);
    font-size: 10.5px;
    font-style: normal;
    color: var(--ink-2);
  }
</style>