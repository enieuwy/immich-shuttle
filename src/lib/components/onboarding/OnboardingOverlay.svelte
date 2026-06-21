<script lang="ts">
  import { Send, Plug, FolderOpen, Upload, Check, CheckCircle2 } from "@lucide/svelte";

  import ProfileEditor from "$lib/components/profiles/ProfileEditor.svelte";
  import { Button } from "$lib/components/ui/button";

  let { onDone = () => {} } = $props<{ onDone?: () => void }>();

  let step = $state(1);

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
      {#each steps as s, i (s.n)}
        {#if i > 0}
          <span class="select-none text-muted-foreground/40" aria-hidden="true">·</span>
        {/if}
        {@const Icon = s.icon}
        {@const done = s.n < step}
        {@const active = s.n === step}
        <div class="flex items-center gap-2 text-sm">
          <span
            class="grid size-5 shrink-0 place-items-center rounded-full text-[11px] font-semibold tabular-nums transition-colors {done
              ? 'bg-emerald-500 text-white'
              : active
                ? 'bg-primary text-primary-foreground'
                : 'bg-muted text-muted-foreground'}"
          >
            {#if done}
              <Check class="size-3" />
            {:else}
              {s.n}
            {/if}
          </span>
          <Icon class="size-3.5 {active ? 'text-primary' : 'text-muted-foreground'}" />
          <span class={active ? "font-medium text-foreground" : "text-muted-foreground"}>{s.label}</span>
        </div>
      {/each}
    </div>

    {#if step === 1}
      <ProfileEditor onSaved={() => (step = 2)} onCancel={() => {}} />
    {:else}
      <div class="flex flex-col items-center gap-5 py-4 text-center">
        <div class="grid size-16 place-items-center rounded-full bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
          <CheckCircle2 class="size-9" />
        </div>
        <div class="flex flex-col gap-1.5">
          <h3 class="text-xl font-semibold tracking-tight text-foreground">You're connected!</h3>
          <p class="max-w-md text-sm text-muted-foreground">
            Pick a source — a folder or a memory card — then choose albums and start importing.
          </p>
        </div>
        <div class="mt-1 flex w-full flex-col-reverse items-center gap-2 sm:flex-row sm:justify-center">
          <Button variant="ghost" onclick={() => (step = 1)}>Add another server</Button>
          <Button onclick={() => onDone()}>Get started</Button>
        </div>
      </div>
    {/if}
  </div>
</div>
