<script lang="ts">
  // The surface's keys. Three, and each one is an escape hatch rather than a
  // shortcut to learn: the palette, and two ways out.
  //
  // The Enter guard matters: a screenshot of an empty panel is not the only
  // cost of a key that fires while someone is typing. `Escape` in a field
  // blurs the field and stops there, so a composer draft is never thrown away
  // by the key that means "leave this input".
  import { ui } from '../lib/state/ui.svelte';

  function isTyping(target: EventTarget | null): boolean {
    return (
      target instanceof HTMLInputElement ||
      target instanceof HTMLTextAreaElement ||
      (target instanceof HTMLElement && target.isContentEditable)
    );
  }

  function onKeydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      ui.paletteOpen = !ui.paletteOpen;
      return;
    }
    if (event.key === 'Escape') {
      if (ui.paletteOpen) return;
      if (isTyping(event.target)) {
        (event.target as HTMLElement).blur();
        return;
      }
      if (ui.selection) ui.selection = null;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />
