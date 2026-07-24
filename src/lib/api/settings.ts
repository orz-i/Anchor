import { invoke } from "@tauri-apps/api/core";
import { invokeRead } from "$lib/api/invoke";

export interface FrpProfileDto {
  id: string;
  name: string;
  server: string;
  serverPort: number;
  hasToken: boolean;
}

export interface FrpProfileInput {
  id: string;
  name: string;
  server: string;
  serverPort: number;
}

export async function listFrpProfiles(): Promise<FrpProfileDto[]> {
  return invokeRead<FrpProfileDto[]>("list_frp_profiles");
}

export async function saveFrpProfile(
  profile: FrpProfileInput,
  token?: string,
): Promise<FrpProfileDto> {
  return invoke<FrpProfileDto>("save_frp_profile", { profile, token });
}

export async function getLastWorkspaceId(): Promise<string> {
  return invokeRead<string>("get_last_workspace_id");
}

export async function setLastWorkspace(id: string): Promise<void> {
  return invoke("set_last_workspace", { id });
}

export async function deleteFrpProfile(id: string): Promise<void> {
  return invoke("delete_frp_profile", { id });
}

export interface ProxyConfigDto {
  mode: string;
  url: string;
}

export async function getProxy(): Promise<ProxyConfigDto> {
  return invokeRead<ProxyConfigDto>("get_proxy");
}

export async function setProxy(proxy: ProxyConfigDto): Promise<void> {
  return invoke("set_proxy", { proxy });
}
