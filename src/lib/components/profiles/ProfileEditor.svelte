<script lang="ts">
  import { profilesState } from "$lib/state/profiles";
  import type { Profile } from "$lib/types";
  import { Input } from "$lib/components/ui/input";
  import { Label } from "$lib/components/ui/label";
  import { Button } from "$lib/components/ui/button";
  import { Alert, AlertDescription } from "$lib/components/ui/alert";
  import { CheckCircle2, XCircle, Loader2 } from "@lucide/svelte";

  let {
    profile = null,
    onSaved = () => {},
    onCancel = () => {},
  } = $props<{
    profile?: Profile | null;
    onSaved?: () => void;
    onCancel?: () => void;
  }>();

  let serverUrl = $state("");
  let lanServerUrl = $state("");
  let wanServerUrl = $state("");
  let apiKey = $state("");
  let validation = $state("");
  let saving = $state(false);
  let errorMessage = $state("");
  let validatedUserName = $state("");
  let validating = $derived(validation === "Validating...");

  $effect(() => {
    serverUrl = profile?.server_url ?? "";
    lanServerUrl = profile?.lan_server_url ?? "";
    wanServerUrl = profile?.wan_server_url ?? "";
  });

  async function testConnection() {
    errorMessage = "";
    validation = "Validating...";
    try {
      const result = await profilesState.validateProfile(serverUrl, apiKey);
      validatedUserName = result.user_name;
      validation = `Connected as ${result.user_name} (server ${result.server_version})`;
    } catch (error) {
      validation = "";
      validatedUserName = "";
      errorMessage = error instanceof Error ? error.message : String(error);
    }
  }

  async function saveProfile() {
    saving = true;
    errorMessage = "";
    try {
      await profilesState.saveProfile({
        id: profile?.id ?? null,
        display_name: validatedUserName || profile?.display_name || null,
        server_url: serverUrl,
        lan_server_url: lanServerUrl || null,
        wan_server_url: wanServerUrl || null,
        api_key: apiKey || null,
      });
      onSaved();
    } catch (error) {
      errorMessage = error instanceof Error ? error.message : String(error);
    } finally {
      saving = false;
    }
  }
</script>

<div class="grid gap-3">
  {#if profile}
    <h3 class="text-sm font-semibold text-foreground">Edit connection</h3>
  {/if}

  <div class="grid gap-1.5">
    <Label for="serverUrl">Primary Immich URL</Label>
    <Input id="serverUrl" bind:value={serverUrl} placeholder="https://immich.example.com" autocapitalize="off" autocorrect="off" spellcheck={false} />
  </div>

  <div class="grid gap-1.5">
    <Label for="apiKey">API key</Label>
    <Input
      id="apiKey"
      type="password"
      bind:value={apiKey}
      placeholder={profile ? "Leave blank to keep existing key" : "Paste your Immich API key"}
    />
    <p class="text-xs text-muted-foreground">Immich Web → Account Settings → API Keys</p>
  </div>

  <div class="mt-1 grid gap-3 rounded-lg border border-border/70 bg-muted/40 p-3">
    <p class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Advanced (optional)</p>
    <div class="grid gap-3 sm:grid-cols-2">
      <div class="grid gap-1.5">
        <Label for="lanServerUrl">LAN URL (optional)</Label>
        <Input id="lanServerUrl" bind:value={lanServerUrl} placeholder="http://192.168.1.10:2283" autocapitalize="off" autocorrect="off" spellcheck={false} />
      </div>
      <div class="grid gap-1.5">
        <Label for="wanServerUrl">WAN URL (optional)</Label>
        <Input id="wanServerUrl" bind:value={wanServerUrl} placeholder="https://immich.example.com" autocapitalize="off" autocorrect="off" spellcheck={false} />
      </div>
    </div>
  </div>

  {#if validatedUserName}
    <div class="flex items-center gap-2 rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-sm font-medium text-emerald-600 dark:text-emerald-400">
      <CheckCircle2 class="size-4 shrink-0" />
      <span>{validation}</span>
    </div>
  {:else if errorMessage}
    <Alert variant="destructive">
      <XCircle />
      <AlertDescription>{errorMessage}</AlertDescription>
    </Alert>
  {/if}

  <div class="mt-1 flex flex-wrap items-center justify-end gap-2">
    <Button variant="ghost" class="mr-auto" onclick={onCancel}>Cancel</Button>
    <Button variant="outline" onclick={testConnection} disabled={validating || !serverUrl.trim()}>
      {#if validating}
        <Loader2 class="size-4 animate-spin" />
        Testing…
      {:else}
        Test connection
      {/if}
    </Button>
    <Button onclick={saveProfile} disabled={saving || !serverUrl.trim()}>
      {saving ? "Saving…" : "Save"}
    </Button>
  </div>
</div>
