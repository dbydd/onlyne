<script lang="ts">
  // The palette: one field, one list, three keys. It is mounted always and
  // renders nothing until asked for, so the surface it sits over never pays
  // for it.
  //
  // Ranking is deliberately the dullest rule that works: within a group a match
  // at the start of the row outranks a match anywhere in it, ties keep the
  // order the group was built in, and each group is cut at a fixed size.
  // Nothing is scored on recency or frequency, so a result never moves because
  // the cluster happened to push something.
  import CaretDown from 'phosphor-svelte/lib/CaretDown';
  import CaretRight from 'phosphor-svelte/lib/CaretRight';
  import X from 'phosphor-svelte/lib/X';
  import { clip, plural, short } from '../lib/format';
  import { OPERATOR_ROLE, principalName, presenceTone, workOf, workTitle, type Tone } from '../lib/model';
  import { cluster } from '../lib/state/cluster.svelte';
  import { ui } from '../lib/state/ui.svelte';

  type Group = 'Actions' | 'Roles' | 'Tasks';

  interface Result {
    group: Group;
    key: string;
    text: string;
    tone?: Tone;
    note?: string;
    /// Where this row sits in the one highlight that walks all three groups.
    at: number;
    run: () => void;
  }

  interface GroupRows {
    name: Group;
    rows: Result[];
  }

  interface FlatRow extends Result {
    groupname: Group;
  }

  const MAX_GROUPS = 3;
  const MAX_TASKS = 8;
  const LIST_ID = 'palette-results';

  let query = $state('');
  let cursor = $state(0);
  let input: HTMLInputElement | null = $state(null);
  let list: HTMLElement | null = $state(null);

  const results = $derived.by<GroupRows[]>(() => {
    const needle = query.trim().toLowerCase();
    const groups: GroupRows[] =
      needle === ''
        ? [{ name: 'Actions', rows: actions() }]
        : [
            { name: 'Actions', rows: rank(actions(), needle).slice(0, MAX_GROUPS) },
            { name: 'Roles', rows: rank(roles(), needle).slice(0, MAX_GROUPS) },
            { name: 'Tasks', rows: rank(tasks(), needle).slice(0, MAX_TASKS) },
          ];
    let at = 0;
    const numbered = groups.map((group) => ({
      ...group,
      rows: group.rows.map((row) => ({ ...row, at: at++ })),
    }));
    return numbered.filter((group) => group.rows.length > 0);
  });

  /// One highlight over three groups, so the list is walked the way it is read.
  const flat = $derived<FlatRow[]>(results.flatMap((group) => group.rows.map((row) => ({ ...row, groupname: group.name }))));
  const activeId = $derived(flat.length > 0 ? `palette-row-${flat[cursor]?.key}` : undefined);

  /// The query and the highlight cannot disagree: whatever the last keystroke
  /// left highlighted is what Enter runs, and a list that got shorter cannot
  /// leave the highlight past its end.
  $effect(() => {
    const total = flat.length;
    if (total === 0) cursor = 0;
    else if (cursor >= total) cursor = total - 1;
  });

  // A field that was focused a moment ago is not the palette's any more.
  $effect(() => {
    if (ui.paletteOpen && input) input.focus();
  });

  function actions(): Result[] {
    const rows: Result[] = [];
    for (const board of cluster.canvasBoards) {
      if (board.presence === 'offline') continue;
      const role = board.role;
      rows.push({
        group: 'Actions',
        key: `send:${role}`,
        text: `Send a task to ${role}`,
        at: 0,
        run: () => ui.select({ kind: 'role', role }),
      });
    }
    const faults = cluster.openFaults.length;
    rows.push({
      group: 'Actions',
      key: 'dock:faults',
      text: 'Open faults',
      at: 0,
      run: () => ui.setDock({ tab: 'faults', open: true }),
    });
    if (faults > 0) {
      rows.push({
        group: 'Actions',
        key: 'dock:faults:open',
        text: `Open the ${plural(faults, 'open fault')} in the dock`,
        at: 0,
        run: () => ui.setDock({ tab: 'faults', open: true }),
      });
    }
    rows.push({
      group: 'Actions',
      key: 'dock:ledger',
      text: 'Open ledger',
      at: 0,
      run: () => ui.setDock({ tab: 'ledger', open: true }),
    });
    // The operator's own board is not one of the canvas boards, so it has no
    // task action here; an entry that names it keeps it findable.
    rows.push({
      group: 'Actions',
      key: `send:${OPERATOR_ROLE}`,
      text: `Send a task to ${OPERATOR_ROLE}`,
      at: 0,
      run: () => ui.select({ kind: 'role', role: OPERATOR_ROLE }),
    });
    rows.push({ group: 'Actions', key: 'layout:reset', text: 'Reset layout', at: 0, run: () => ui.resetLayout() });
    rows.push({ group: 'Actions', key: 'select:declare', text: 'Declare a role', at: 0, run: () => ui.select({ kind: 'declare' }) });
    return rows;
  }

  function roles(): Result[] {
    return cluster.canvasBoards.map((board) => {
      const role = board.role;
      return {
        group: 'Roles' as const,
        key: `role:${role}`,
        text: role,
        tone: presenceTone(board.presence),
        note: `${plural(board.queued ?? 0, 'queued task')} · ${plural(board.cards?.length ?? 0, 'card')}`,
        at: 0,
        run: () => ui.select({ kind: 'role', role }, true),
      };
    });
  }

  function tasks(): Result[] {
    return cluster.deliveries.map((delivery) => ({
      group: 'Tasks' as const,
      key: `task:${delivery.msg_id}`,
      text: clip(workTitle({ out_head: delivery.out_head, kind: delivery.kind }), 64),
      tone: workOf({ state: delivery.state, outcome: delivery.outcome, column: cluster.columnById.get(delivery.msg_id) }).tone,
      note: `${principalName(delivery.to)} · ${short(delivery.msg_id)}`,
      at: 0,
      run: () => ui.select({ kind: 'task', msgId: delivery.msg_id, role: principalName(delivery.to) }, true),
    }));
  }

  /// Prefix first, then substring, and a miss dropped. Equal ranks keep the
  /// order they arrived in, which is the order the group was built in.
  function rank(rows: Result[], needle: string): Result[] {
    const prefix: Result[] = [];
    const inner: Result[] = [];
    for (const row of rows) {
      const at = haystack(row).indexOf(needle);
      if (at === 0) prefix.push(row);
      else if (at > 0) inner.push(row);
    }
    return [...prefix, ...inner];
  }

  /// What a row is matched on: a role by its name, an action by its sentence,
  /// a task by its title and the id an operator pastes from a log.
  function haystack(row: Result): string {
    return row.note === undefined ? row.text.toLowerCase() : `${row.text} ${row.note}`.toLowerCase();
  }

  function close() {
    ui.paletteOpen = false;
  }

  function runAt(index: number) {
    const row = flat[index];
    if (!row) return;
    close();
    row.run();
  }

  function onKeydown(event: KeyboardEvent) {
    switch (event.key) {
      case 'Escape':
        // Stops here on purpose: the surface handler defers to the palette
        // while it is open, and a key that closed the palette and then cleared
        // the selection behind it would take two presses to undo.
        event.preventDefault();
        event.stopPropagation();
        close();
        break;
      case 'ArrowDown':
        event.preventDefault();
        if (flat.length > 0) cursor = (cursor + 1) % flat.length;
        scrollToCursor();
        break;
      case 'ArrowUp':
        event.preventDefault();
        if (flat.length > 0) cursor = (cursor - 1 + flat.length) % flat.length;
        scrollToCursor();
        break;
      case 'Home':
        event.preventDefault();
        cursor = 0;
        scrollToCursor();
        break;
      case 'End':
        event.preventDefault();
        cursor = Math.max(0, flat.length - 1);
        scrollToCursor();
        break;
      case 'Enter':
        event.preventDefault();
        runAt(cursor);
        break;
    }
  }

  function scrollToCursor() {
    // The browser scrolls the list itself: it knows how tall the rows turned
    // out to be, and this is asked for after a keystroke, so the layout it
    // reads is the settled one.
    queueMicrotask(() => {
      const row = flat[cursor];
      if (row) list?.querySelector(`#${CSS.escape(`palette-row-${row.key}`)}`)?.scrollIntoView({ block: 'nearest' });
    });
  }
</script>

{#if ui.paletteOpen}
  <div class="scrim" role="presentation" onclick={close}></div>
  <div class="palette" role="dialog" aria-modal="true" aria-label="Command palette">
    <input
      bind:this={input}
      bind:value={query}
      oninput={() => {
        cursor = 0;
      }}
      onkeydown={onKeydown}
      class="input query"
      type="text"
      role="combobox"
      autocomplete="off"
      autocapitalize="off"
      spellcheck="false"
      placeholder="Jump to a board, a task or an action"
      aria-label="Search boards, tasks and actions"
      aria-expanded={flat.length > 0}
      aria-controls={LIST_ID}
      aria-autocomplete="list"
      aria-activedescendant={activeId}
    />
    <div class="scroll">
      <div class="results" role="listbox" id={LIST_ID} tabindex="-1" bind:this={list} onkeydown={onKeydown} aria-label="Results">
        {#each flat as row (row.key)}
          <div
            class="row"
            class:on={row.at === cursor}
            id={`palette-row-${row.key}`}
            role="option"
            tabindex="-1"
            aria-selected={row.at === cursor}
            onmouseenter={() => (cursor = row.at)}
            onclick={() => runAt(row.at)}
            onkeydown={onKeydown}
          >
            <span class="groupname">{row.groupname}</span>
            {#if row.tone}<span class="dot" data-tone={row.tone} aria-hidden="true"></span>{/if}
            <span class="text trunc">{row.text}</span>
            {#if row.note}<span class="note trunc">{row.note}</span>{/if}
          </div>
        {/each}
        {#if results.length === 0}
          <p class="none">Nothing here is called that.</p>
        {/if}
      </div>
      {#if results.length > 0}
        <p class="keys">
          <span class="kbd up"><CaretDown /></span>
          <span class="kbd"><CaretDown /></span>
          <span>to move</span>
          <span class="kbd"><CaretRight /></span>
          <span>to run</span>
          <span class="kbd"><X /></span>
          <span>to close</span>
        </p>
      {/if}
    </div>
    {#if query.trim() === ''}
      <p class="hint">
        <span class="key">Jump to a board</span>
        <span class="key">Send a task</span>
      </p>
    {/if}
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: var(--z-palette);
    background: var(--scrim);
    animation: fade var(--t-fast) var(--ease);
  }
  .palette {
    position: fixed;
    top: 14vh;
    left: 50%;
    z-index: calc(var(--z-palette) + 1);
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - var(--s-5)));
    transform: translateX(-50%);
    border-radius: var(--r-3);
    background: var(--raised);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
    animation: open var(--t-med) var(--ease);
  }
  .query {
    height: 40px;
    border: 0;
    border-bottom: 1px solid var(--line-soft);
    border-radius: 0;
    background: transparent;
    box-shadow: none;
    font-size: var(--fs-13);
  }
  .query:hover {
    box-shadow: none;
  }
  .query:focus-visible {
    box-shadow: inset 0 -2px 0 var(--focus);
  }
  .scroll {
    max-height: 52vh;
    overflow-y: auto;
  }
  .results {
    padding: var(--s-2) 0;
    outline: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    height: 26px;
    padding: 0 var(--s-3);
    cursor: pointer;
  }
  .row.on {
    background: var(--hover);
    box-shadow: inset 2px 0 0 var(--bone);
  }
  /* The group rides each row rather than heading a block: one list box, with
   * the rows still options of it, and a constant gutter so the labels do not
   * shift the text when the results change. */
  .groupname {
    flex: none;
    width: 52px;
    font-size: var(--fs-11);
    font-weight: 500;
    color: var(--ink-4);
  }
  .row.on .groupname {
    color: var(--ink-3);
  }
  .text {
    flex: 1 1 auto;
    font-size: var(--fs-12);
    color: var(--ink-2);
  }
  .row.on .text {
    color: var(--ink);
  }
  .note {
    flex: 0 1 auto;
    max-width: 55%;
    font-size: var(--fs-11);
    color: var(--ink-4);
    text-align: right;
  }
  .keys {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: var(--s-2) var(--s-3) 2px;
    font-size: var(--fs-11);
    color: var(--ink-4);
  }
  .keys .kbd {
    min-width: 16px;
    height: 16px;
    font-size: 9.5px;
    color: var(--ink-3);
  }
  /* No up arrow ships in the family; the same glyph turned over is the same
   * glyph. */
  .keys .up {
    transform: rotate(180deg);
  }
  .none {
    padding: var(--s-2) var(--s-3) var(--s-3);
    font-size: var(--fs-12);
    color: var(--ink-3);
  }
  .hint {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-3) var(--s-3);
    border-top: 1px solid var(--line-soft);
    font-size: var(--fs-11);
    color: var(--ink-3);
  }
  .key {
    padding: 1px 6px;
    border-radius: var(--r-1);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--ink-3);
  }
  @keyframes open {
    from {
      opacity: 0;
      transform: translateX(-50%) translateY(-6px);
    }
    to {
      opacity: 1;
      transform: translateX(-50%) translateY(0);
    }
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
</style>