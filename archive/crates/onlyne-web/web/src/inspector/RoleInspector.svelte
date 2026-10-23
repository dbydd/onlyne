<script lang="ts">
  // The board's own panel: the role's live state, the five things an operator
  // does to it, and the spec half of its entry.
  //
  // The spec is read lazily by whoever first needs a field of it, so this panel
  // asks for one read when it opens on a role the held read does not describe.
  // The effect is guarded on the value already held, because an unguarded ask
  // re-reads on every frame the stream pushes and one admin round trip per
  // frame is how a watch screen turns into a load generator.
  import ConfigEditor from './ConfigEditor.svelte';
  import Composer from './Composer.svelte';
  import Declare from './Declare.svelte';
  import Frame from '../shell/Frame.svelte';
  import RoleSessions from './RoleSessions.svelte';
  import RoutesEditor from './RoutesEditor.svelte';
  import Section from '../lib/ui/Section.svelte';
  import WorkList from './WorkList.svelte';
  import { clip } from '../lib/format';
  import { cluster } from '../lib/state/cluster.svelte';
  import { presenceTone } from '../lib/model';
  import { spec } from '../lib/state/spec.svelte';

  interface Props {
    role?: string;
    declaring?: boolean;
  }

  let { role = '', declaring = false }: Props = $props();

  const info = $derived(cluster.roleInfo(role));
  const board = $derived(cluster.boardByRole.get(role));
  const presence = $derived(board?.presence ?? 'offline');
  const drive = $derived(info?.runtime?.drive ?? 'plugin');
  /// The two depths the operator judges a board by, read off the board the
  /// reducer drew rather than off a second tally kept here.
  const queued = $derived(board?.queued ?? 0);
  const busy = $derived(cluster.countsOf(role).busy ?? 0);
  const client = $derived(spec.clientOf(role));
  /// One line is what a header can hold; the whole prose belongs to the config
  /// section, where there is room to edit it.
  const firstLine = $derived((client?.prose ?? '').split('\n').find((line) => line.trim() !== '') ?? '');
  const subtitle = $derived(clip(firstLine, 78));

  /// The cluster hash the held read was fetched for, kept locally so the ask is
  /// skipped on every later frame rather than repeated. A read that failed
  /// stays held: the error toast already reached the operator, and a panel that
  /// keeps asking cannot help.
  let readFor: string | null = null;
  $effect(() => {
    const hash = cluster.view.cluster?.spec_hash ?? '';
    if (!role || declaring || readFor === hash) return;
    if (!client && spec.error !== '') return;
    readFor = hash;
    void spec.refresh(cluster.token, hash);
  });
</script>

<Frame
  eyebrow={declaring ? 'declare' : 'role'}
  title={declaring ? 'Declare a role' : role}
  subtitle={declaring ? 'One entry in spec.toml, with a key pasted from the machine that will run it.' : subtitle}
>
  {#snippet actions()}
    {#if !declaring}
      <span class="chip line" data-tone={presenceTone(presence)}>{presence}</span>
      <span class="chip mono">{drive}</span>
      {#if info?.admin}
        <span class="chip line">admin</span>
      {/if}
      {#if queued > 0}
        <span class="chip line" data-tone="queue"><span class="num">{queued}</span> queued</span>
      {/if}
      {#if busy > 0}
        <span class="chip line" data-tone="run"><span class="num">{busy}</span> running</span>
      {/if}
    {/if}
  {/snippet}

  {#if declaring}
    <Declare />
  {:else}
    <Composer {role} />
    <Section title="Work" count={cluster.boardCards(role).length}>
      <WorkList {role} />
    </Section>
    <Section title="Sessions" count={cluster.sessionsByRole.get(role)?.length ?? 0}>
      <RoleSessions {role} />
    </Section>
    <Section title="Routes">
      <RoutesEditor {role} />
    </Section>
    <Section title="Config">
      <ConfigEditor {role} client={client} />
    </Section>
  {/if}
</Frame>