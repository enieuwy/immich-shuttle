<script lang="ts">
  import { avatarStyle } from "$lib/users";
  import { avatarKey, avatarsState } from "$lib/state/avatars";
  import type { AlbumUser } from "$lib/types";

  let {
    user,
    profileId,
    class: className = "",
  }: {
    user: AlbumUser;
    /** Profile whose server the avatar image is fetched from. */
    profileId: string | null | undefined;
    /** Sizing/typography, e.g. "size-4 text-[8px]". */
    class?: string;
  } = $props();

  const style = $derived(avatarStyle(user));
  const image = $derived(
    profileId ? ($avatarsState.images.get(avatarKey(profileId, user.id)) ?? null) : null,
  );
</script>

{#if image}
  <img
    src={image}
    alt=""
    title={user.name}
    class="rounded-full object-cover ring-1 ring-card {className}"
  />
{:else}
  <span
    title={user.name}
    class="grid place-items-center rounded-full font-semibold ring-1 ring-card {className}"
    style="background-color: {style.bg}; color: {style.fg};"
  >
    {user.name.charAt(0).toUpperCase()}
  </span>
{/if}
