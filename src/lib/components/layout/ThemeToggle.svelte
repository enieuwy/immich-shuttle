<script lang="ts">
  import { Sun, Moon, Monitor } from "@lucide/svelte";
  import { Button } from "$lib/components/ui/button";
  import {
    Tooltip,
    TooltipContent,
    TooltipProvider,
    TooltipTrigger,
  } from "$lib/components/ui/tooltip";
  import { themeState } from "$lib/state/theme";

  const nextLabel = $derived(
    $themeState === "light"
      ? "Switch to dark"
      : $themeState === "dark"
        ? "Switch to system"
        : "Switch to light",
  );
</script>

<TooltipProvider delayDuration={200}>
  <Tooltip>
    <TooltipTrigger>
      {#snippet child({ props })}
        <Button variant="ghost" size="icon-sm" {...props} aria-label={nextLabel} onclick={() => themeState.cycle()}>
          {#if $themeState === "light"}
            <Sun class="size-4" />
          {:else if $themeState === "dark"}
            <Moon class="size-4" />
          {:else}
            <Monitor class="size-4" />
          {/if}
        </Button>
      {/snippet}
    </TooltipTrigger>
    <TooltipContent>{nextLabel}</TooltipContent>
  </Tooltip>
</TooltipProvider>
