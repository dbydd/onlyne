<script lang="ts">
  // A two-step button for the ops that take something away. The first click
  // arms it and renames it, the second fires; a blur or four seconds of
  // hesitation disarms. It is deliberately not a modal: nothing on this
  // surface should stop the world to ask a question a button can ask.
  interface Props {
    label: string;
    confirm?: string;
    onconfirm: () => void;
    variant?: 'danger' | 'primary' | 'ghost' | 'default';
    size?: '' | 'sm';
    disabled?: boolean;
    busy?: boolean;
    title?: string;
  }

  let {
    label,
    confirm = 'confirm',
    onconfirm,
    variant = 'danger',
    size = '',
    disabled = false,
    busy = false,
    title = '',
  }: Props = $props();

  let armed = $state(false);
  let timer: number | null = null;

  function disarm() {
    if (timer !== null) window.clearTimeout(timer);
    timer = null;
    armed = false;
  }

  function fire(event: MouseEvent) {
    event.stopPropagation();
    if (!armed) {
      armed = true;
      timer = window.setTimeout(disarm, 4000);
      return;
    }
    disarm();
    onconfirm();
  }
</script>

<button
  class="btn {variant} {size}"
  class:armed
  aria-busy={busy}
  disabled={disabled || busy}
  {title}
  onclick={fire}
  onblur={disarm}
>
  {armed ? confirm : label}
</button>
