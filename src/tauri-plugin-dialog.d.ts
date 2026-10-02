declare module "@tauri-apps/plugin-dialog" {
  export type DialogFilter = {
    name: string;
    extensions: string[];
  };

  export type OpenDialogOptions = {
    title?: string;
    directory?: boolean;
    multiple?: boolean;
    filters?: DialogFilter[];
  };

  export function open(options?: OpenDialogOptions): Promise<string | string[] | null>;

  export type SaveDialogOptions = {
    title?: string;
    defaultPath?: string;
    filters?: DialogFilter[];
  };

  export function save(options?: SaveDialogOptions): Promise<string | null>;

  export type ConfirmDialogOptions = {
    title?: string;
    kind?: "info" | "warning" | "error";
    okLabel?: string;
    cancelLabel?: string;
  };

  export function confirm(message: string, options?: string | ConfirmDialogOptions): Promise<boolean>;
}
