<script lang="ts">
  import { onMount } from "svelte";
  import { Plus } from "@lucide/svelte";

  import ProfileEditor from "$lib/components/profiles/ProfileEditor.svelte";
  import { profilesState } from "$lib/state/profiles";
  import type { Profile } from "$lib/types";
  import { Button } from "$lib/components/ui/button";

  let { onDone = () => {}, initialEdit = null } = $props<{
    onDone?: () => void;
    initialEdit?: Profile | null;
  }>();

  let editing = $state<Profile | null>(null);
  let creating = $state(false);

  onMount(async () => {
    // When asked to edit a specific profile (e.g. it has no API key), open its
    // editor directly instead of the profile list.
    if (initialEdit) {
      editing = initialEdit;
    }
    await profilesState.loadProfiles();
  });

  function beginAdd() {
    editing = null;
    creating = true;
  }

  function beginEdit(profile: Profile) {
    editing = profile;
    creating = false;
  }

  async function remove(profile: Profile) {
    if (!confirm(`Delete ${profile.display_name}?`)) {
      return;
    }
    await profilesState.deleteProfile(profile.id);
  }

  function closeEditor() {
    editing = null;
    creating = false;
    onDone();
  }
</script>

<div class="flex flex-col gap-4">
  {#if creating || editing}
    <ProfileEditor profile={editing} onSaved={closeEditor} onCancel={closeEditor} />
  {:else}
    <div class="flex items-center justify-between">
      <h3 class="text-sm font-medium text-foreground">Users</h3>
      <Button variant="outline" size="sm" onclick={beginAdd}>
        <Plus class="mr-2 h-4 w-4" /> Add user
      </Button>
    </div>

    <div class="flex flex-col gap-2">
      {#if $profilesState.profiles.length === 0}
        <p class="text-sm text-muted-foreground">No users configured.</p>
      {:else}
        {#each $profilesState.profiles as profile}
          <div class="flex items-center justify-between rounded-lg border border-border bg-card p-3">
            <div class="flex min-w-0 items-center gap-3">
              <div class="grid size-8 shrink-0 place-items-center rounded-full bg-primary/15 text-primary">
                {profile.display_name.charAt(0).toUpperCase()}
              </div>
              <div class="min-w-0">
                <strong class="text-sm font-medium text-foreground">{profile.display_name}</strong>
                <p class="mt-0.5 truncate text-xs text-muted-foreground" title={profile.server_url}>{profile.server_url}</p>
              </div>
            </div>
            <div class="flex items-center gap-1">
              <Button variant="ghost" size="sm" onclick={() => beginEdit(profile)}>Edit</Button>
              <Button variant="ghost" size="sm" class="text-destructive hover:text-destructive" onclick={() => remove(profile)}>Delete</Button>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>
