import { invokeAdmin } from "@/lib/api/invoke";

export type TunnelService = "mcp" | "actions";

export interface TunnelStatus {
  state: string;
  publicUrl: string;
  tunnelPid: number | null;
}

export async function startTunnel(id: string, service: TunnelService): Promise<TunnelStatus> {
  return invokeAdmin<TunnelStatus>("start_tunnel", { id, service });
}

export async function stopTunnel(id: string, service: TunnelService): Promise<TunnelStatus> {
  return invokeAdmin<TunnelStatus>("stop_tunnel", { id, service });
}

export interface TunnelTestResult {
  success: boolean;
  publicUrl: string;
  keptRunning: boolean;
  message: string;
}

export async function testTunnel(id: string, service: TunnelService): Promise<TunnelTestResult> {
  return invokeAdmin<TunnelTestResult>("test_tunnel", { id, service });
}

export async function restartTunnel(id: string, service: TunnelService): Promise<TunnelStatus> {
  return invokeAdmin<TunnelStatus>("restart_tunnel", { id, service });
}
