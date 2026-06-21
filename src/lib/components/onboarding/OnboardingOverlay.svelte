<script lang="ts">
  import { Send, Plug, FolderOpen, Upload } from "@lucide/svelte";

  import ProfileEditor from "$lib/components/profiles/ProfileEditor.svelte";
  import { profilesState } from "$lib/state/profiles";

  let { onDone = () => {} } = $props<{ onDone?: () => void }>();

  $effect(() => {
    if ($profilesState.profiles.length > 0) {
      onDone();
    }
  });

  const steps = [
    { n: 1, label: "Connect server", icon: Plug },
    { n: 2, label: "Choose a source", icon: FolderOpen },
    { n: 3, label: "Import", icon: Upload },
  ];
</script>

<div class="fixed inset-0 z-50 grid place-items-center bg-background/80 p-4 backdrop-blur-sm">
  <div class="grid w-full max-w-[760px] gap-6 rounded-xl border border-border bg-card p-6 shadow-lg sm:p-8">
    <div class="flex items-start gap-4">
      <div class="grid size-10 shrink-0 place-items-center rounded-xl brand-gradient shadow-sm">
        <Send class="size-5 text-white" />
      </div>
      <div class="flex flex-col gap-1">
        <h2 class="text-2xl font-semibold tracking-tight brand-text-gradient">Welcome to Immich Shuttle</h2>
        <p class="text-sm text-muted-foreground">Connect your Immich server to start importing photos and videos.</p>
      </div>
    </div>

    <div class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-border/70 bg-muted/40 px-4 py-3">
      {#each steps as step, i (step.n)}
        {#if i > 0}
          <span class="select-none text-muted-foreground/40" aria-hidden="true">·</span>
        {/if}
        {@const Icon = step.icon}
        <div class="flex items-center gap-2 text-sm">
          <span class="grid size-5 shrink-0 place-items-center rounded-full bg-primary text-[11px] font-semibold tabular-nums text-primary-foreground">{step.n}</span>
          <Icon class="size-3.5 text-muted-foreground" />
          <span class="text-muted-foreground">{step.label}</span>
        </div>
      {/each}
    </div>

    <ProfileEditor onSaved={onDone} onCancel={() => {}} />
  </div>
</div>
