<script lang="ts">
  import {
    Tooltip,
    TooltipContent,
    TooltipProvider,
    TooltipTrigger,
  } from "$lib/components/ui/tooltip";
  import { avatarStyle } from "$lib/users";
  import { avatarKey, avatarsState } from "$lib/state/avatars";
  import { avatarDisplayState } from "$lib/state/theme";
  import type { AlbumUser } from "$lib/types";

  let {
    user,
    profileId,
    class: className = "",
  }: {
    user: AlbumUser;
    /** Profile whose server the avatar image is fetched from. */
    profileId: string | null | undefined;
    /** Sizing/typography, e.g. "size-4.5 text-[9px]". */
    class?: string;
  } = $props();

  const style = $derived(avatarStyle(user));
  // Photo badges are opt-in: initials read better at badge size, so the
  // image only renders when the appearance setting asks for it.
  const image = $derived(
    profileId && $avatarDisplayState === "photos"
      ? ($avatarsState.images.get(avatarKey(profileId, user.id)) ?? null)
      : null,
  );
</script>

<TooltipProvider delayDuration={200}>
  <Tooltip>
    <TooltipTrigger>
      {#snippet child({ props })}
        {#if image}
          <img
            {...props}
            src={image}
            alt=""
            class="rounded-full object-cover {className}"
            style="box-shadow: 0 0 0 1.5px {style.bg};"
          />
        {:else}
          <span
            {...props}
            class="grid place-items-center rounded-full font-semibold ring-1 ring-card {className}"
            style="background-color: {style.bg}; color: {style.fg};"
          >
            {user.name.charAt(0).toUpperCase()}
          </span>
        {/if}
      {/snippet}
    </TooltipTrigger>
    <TooltipContent>{user.name}</TooltipContent>
  </Tooltip>
</TooltipProvider>
