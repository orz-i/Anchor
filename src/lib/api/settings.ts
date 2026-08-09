import { invoke } from "@tauri-apps/api/core";
import { invokeRead } from "$lib/api/invoke";
import type { GatewayControlStatus } from "$lib/types";

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

export interface McpGatewayConfigDto {
  urlModelVersion: number;
  enabled: boolean;
  localPort: number;
  ownerWorkspaceId: string;
  publicUrl: string;
  observedPublicUrl: string;
  observedOwnerWorkspaceId: string;
  observedTunnelSignature: string;
}

export type McpGatewayStatusDto = GatewayControlStatus;

export async function getMcpGateway(): Promise<McpGatewayConfigDto> {
  return invokeRead<McpGatewayConfigDto>("get_mcp_gateway");
}

export async function getMcpGatewayStatus(): Promise<McpGatewayStatusDto> {
  return invokeRead<McpGatewayStatusDto>("get_mcp_gateway_status");
}

export async function setMcpGateway(
  config: McpGatewayConfigDto,
): Promise<McpGatewayStatusDto> {
  return invoke<McpGatewayStatusDto>("set_mcp_gateway", { config });
}

export interface WindowsWorkspaceAutostartDto {
  workspaceId: string;
  service: "mcp" | "actions" | "all";
  tunnelServices?: "mcp" | "actions" | "all";
}

export interface WindowsServicePlanDto {
  schemaVersion: number;
  ownerSid: string;
  ownerUsername: string;
  workspaces: WindowsWorkspaceAutostartDto[];
  gatewayWorkspaceIds: string[];
}

export interface BuildIdentityDto {
  packageVersion: string;
  gitSha: string;
  gitDirty: boolean;
  buildWorkspace: string;
}

export interface WindowsServiceRuntimeStateDto {
  schemaVersion: number;
  pid: number;
  startedAtUnix: number;
  executablePath: string;
  buildIdentity: BuildIdentityDto;
}

export interface WindowsScmServiceStatusDto {
  supported: boolean;
  serviceName: string;
  installed: boolean;
  state: string;
  autoStart: boolean;
  processId?: number;
  configDir: string;
  planPath: string;
  plan: WindowsServicePlanDto;
  buildState: "not_installed" | "stopped" | "current" | "different" | "unknown";
  currentBuild: BuildIdentityDto;
  runtime?: WindowsServiceRuntimeStateDto;
}

export async function getWindowsServiceStatus(): Promise<WindowsScmServiceStatusDto> {
  return invokeRead<WindowsScmServiceStatusDto>("get_windows_service_status");
}

export async function installWindowsService(): Promise<WindowsScmServiceStatusDto> {
  return invoke<WindowsScmServiceStatusDto>("install_windows_service");
}

export async function uninstallWindowsService(): Promise<WindowsScmServiceStatusDto> {
  return invoke<WindowsScmServiceStatusDto>("uninstall_windows_service");
}

export async function startWindowsService(): Promise<WindowsScmServiceStatusDto> {
  return invoke<WindowsScmServiceStatusDto>("start_windows_service");
}

export async function stopWindowsService(): Promise<WindowsScmServiceStatusDto> {
  return invoke<WindowsScmServiceStatusDto>("stop_windows_service");
}

export async function restartWindowsService(): Promise<WindowsScmServiceStatusDto> {
  return invoke<WindowsScmServiceStatusDto>("restart_windows_service");
}

export async function syncWindowsServicePlan(): Promise<WindowsScmServiceStatusDto> {
  return invoke<WindowsScmServiceStatusDto>("sync_windows_service_plan");
}
