import { invokeAdmin, invokeRead } from "@/lib/api/invoke";
import type { TunnelProfile } from "@/lib/types";

export interface TunnelStatus {
  state: string;
  publicUrl: string;
  tunnelPid: number | null;
}

export async function listTunnels(): Promise<TunnelProfile[]> {
  return invokeRead<TunnelProfile[]>("list_tunnels");
}

export async function createTunnel(workspaceId: string, name?: string): Promise<TunnelProfile> {
  return invokeAdmin<TunnelProfile>("create_tunnel", { workspaceId, name: name?.trim() || null });
}

export async function updateTunnel(tunnel: TunnelProfile): Promise<TunnelProfile> {
  return invokeAdmin<TunnelProfile>("update_tunnel", { tunnel });
}

export async function deleteTunnel(id: string): Promise<void> {
  return invokeAdmin("delete_tunnel", { id });
}

export async function getTunnelStatus(id: string): Promise<TunnelStatus> {
  return invokeRead<TunnelStatus>("get_tunnel_status", { id });
}

export async function startTunnel(id: string): Promise<TunnelStatus> {
  return invokeAdmin<TunnelStatus>("start_tunnel", { id });
}

export async function stopTunnel(id: string): Promise<TunnelStatus> {
  return invokeAdmin<TunnelStatus>("stop_tunnel", { id });
}

export interface TunnelTestResult {
  success: boolean;
  publicUrl: string;
  keptRunning: boolean;
  message: string;
}

export async function testTunnel(id: string): Promise<TunnelTestResult> {
  return invokeAdmin<TunnelTestResult>("test_tunnel", { id });
}

export type TunnelSecretKey = "frp_token" | "cloudflare_token";

export async function getTunnelSecret(id: string, key: TunnelSecretKey): Promise<string | null> {
  return invokeRead<string | null>("get_tunnel_secret", { id, key });
}

export async function setTunnelSecret(id: string, key: TunnelSecretKey, value: string): Promise<void> {
  return invokeAdmin("set_tunnel_secret", { id, key, value });
}
