import { writable } from "svelte/store";
import type { RuntimeState, WorkspaceProfile } from "$lib/types";

export const workspaces = writable<WorkspaceProfile[]>([]);
export const mcpRuntimeStates = writable<Record<string, RuntimeState>>({});
export const actionsRuntimeStates = writable<Record<string, RuntimeState>>({});
