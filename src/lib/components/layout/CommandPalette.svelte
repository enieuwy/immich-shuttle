<script module lang="ts">
  export interface PaletteCommand {
    id: string;
    label: string;
    /** Extra terms matched by the fuzzy filter beyond the visible label. */
    keywords?: string[];
    run: () => void;
  }
</script>

<script lang="ts">
  import * as Command from "$lib/components/ui/command";

  let {
    open = $bindable(false),
    commands,
  }: { open?: boolean; commands: PaletteCommand[] } = $props();

  function select(cmd: PaletteCommand) {
    // Close before running so a command that opens another dialog isn't stacked
    // behind the palette.
    open = false;
    cmd.run();
  }
</script>

<Command.Dialog bind:open>
  <Command.Input placeholder="Type a command…" />
  <Command.List>
    <Command.Empty>No commands found.</Command.Empty>
    <Command.Group heading="Commands">
      {#each commands as cmd (cmd.id)}
        <Command.Item value={cmd.label} keywords={cmd.keywords} onSelect={() => select(cmd)}>
          {cmd.label}
        </Command.Item>
      {/each}
    </Command.Group>
  </Command.List>
</Command.Dialog>
