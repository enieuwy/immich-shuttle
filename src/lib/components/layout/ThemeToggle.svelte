<script lang="ts">
  import { Sun, Moon, Monitor, Palette } from "@lucide/svelte";
  import { Button } from "$lib/components/ui/button";
  import {
    DropdownMenu,
    DropdownMenuContent,
    DropdownMenuLabel,
    DropdownMenuRadioGroup,
    DropdownMenuRadioItem,
    DropdownMenuSeparator,
    DropdownMenuTrigger,
  } from "$lib/components/ui/dropdown-menu";
  import {
    Tooltip,
    TooltipContent,
    TooltipProvider,
    TooltipTrigger,
  } from "$lib/components/ui/tooltip";
  import {
    avatarDisplayState,
    paletteState,
    themeState,
    type AvatarDisplay,
    type ThemePalette,
  } from "$lib/state/theme";

  const nextLabel = $derived(
    $themeState === "light"
      ? "Switch to dark"
      : $themeState === "dark"
        ? "Switch to system"
        : "Switch to light",
  );

  const palettes: { id: ThemePalette; label: string; hint: string }[] = [
    { id: "darkroom", label: "Darkroom", hint: "Near-black, teal accent" },
    { id: "indigo", label: "Indigo", hint: "Classic blue" },
    { id: "ember", label: "Ember", hint: "Warm charcoal, amber accent" },
  ];
</script>

<TooltipProvider delayDuration={200}>
  <div class="flex items-center gap-0.5">
    <DropdownMenu>
      <Tooltip>
        <TooltipTrigger>
          {#snippet child({ props: tooltipProps })}
            <DropdownMenuTrigger {...tooltipProps}>
              {#snippet child({ props })}
                <Button variant="ghost" size="icon-sm" {...props} aria-label="Appearance">
                  <Palette class="size-4" />
                </Button>
              {/snippet}
            </DropdownMenuTrigger>
          {/snippet}
        </TooltipTrigger>
        <TooltipContent>Appearance</TooltipContent>
      </Tooltip>
      <DropdownMenuContent align="end" class="w-56">
        <DropdownMenuLabel>Dark palette</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={$paletteState}
          onValueChange={(value) => paletteState.setPalette(value as ThemePalette)}
        >
          {#each palettes as palette (palette.id)}
            <DropdownMenuRadioItem value={palette.id} class="items-start py-2">
              <span class="flex min-w-0 flex-col">
                <span class="font-medium">{palette.label}</span>
                <span class="text-xs text-muted-foreground">{palette.hint}</span>
              </span>
            </DropdownMenuRadioItem>
          {/each}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuLabel>Shared-album badges</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={$avatarDisplayState}
          onValueChange={(value) => avatarDisplayState.setDisplay(value as AvatarDisplay)}
        >
          <DropdownMenuRadioItem value="initials" class="items-start py-2">
            <span class="flex min-w-0 flex-col">
              <span class="font-medium">Colored initials</span>
              <span class="text-xs text-muted-foreground">Readable at badge size</span>
            </span>
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem value="photos" class="items-start py-2">
            <span class="flex min-w-0 flex-col">
              <span class="font-medium">Profile photos</span>
              <span class="text-xs text-muted-foreground">Picture with a colored ring</span>
            </span>
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>

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
  </div>
</TooltipProvider>
