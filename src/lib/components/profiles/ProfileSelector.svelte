<script lang="ts">
  import { ChevronsUpDown, Users } from "@lucide/svelte";
  import { activeProfile, profilesState } from "$lib/state/profiles";

  import { Button } from "$lib/components/ui/button";
  import {
    DropdownMenu,
    DropdownMenuContent,
    DropdownMenuItem,
    DropdownMenuRadioGroup,
    DropdownMenuRadioItem,
    DropdownMenuSeparator,
    DropdownMenuTrigger,
  } from "$lib/components/ui/dropdown-menu";

  let { onManage = () => {} } = $props<{ onManage?: () => void }>();

  let selected = $derived($activeProfile?.id ?? "");

  function profileHost(serverUrl: string) {
    try {
      return new URL(serverUrl).host;
    } catch {
      return serverUrl;
    }
  }

  function handleProfileChange(profileId: string) {
    if (!profileId) {
      return;
    }
    profilesState.setActiveProfile(profileId);
  }
</script>

<DropdownMenu>
  <DropdownMenuTrigger>
    {#snippet child({ props })}
      <Button variant="outline" size="sm" class="min-w-0 justify-between gap-2" {...props} aria-label={$activeProfile ? `Switch profile (current: ${$activeProfile.display_name})` : "Select profile"}>
        {#if $activeProfile}
          <span class="grid size-5 shrink-0 place-items-center rounded-full bg-primary text-[10px] text-primary-foreground">
            {$activeProfile.display_name.charAt(0).toUpperCase()}
          </span>
          <span class="flex min-w-0 flex-col items-start">
            <span class="max-w-[180px] truncate font-medium">{$activeProfile.display_name}</span>
            <span class="text-xs text-muted-foreground">{profileHost($activeProfile.server_url)}</span>
          </span>
        {:else}
          <span class="font-medium">Select profile</span>
        {/if}
        <ChevronsUpDown class="size-4 shrink-0 text-muted-foreground" />
      </Button>
    {/snippet}
  </DropdownMenuTrigger>
  <DropdownMenuContent align="end" class="w-64">
    <DropdownMenuRadioGroup value={selected} onValueChange={handleProfileChange}>
      {#each $profilesState.profiles as profile}
        <DropdownMenuRadioItem value={profile.id} class="items-start py-2">
          <span class="flex min-w-0 flex-col">
            <span class="truncate font-medium">{profile.display_name}</span>
            <span class="truncate text-xs text-muted-foreground">{profileHost(profile.server_url)}</span>
          </span>
        </DropdownMenuRadioItem>
      {/each}
    </DropdownMenuRadioGroup>
    <DropdownMenuSeparator />
    <DropdownMenuItem onclick={onManage} class="gap-2">
      <Users class="size-4 text-muted-foreground" />
      <span>Manage users…</span>
    </DropdownMenuItem>
  </DropdownMenuContent>
</DropdownMenu>
