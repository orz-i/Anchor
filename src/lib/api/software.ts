import { invokeAdmin } from "@/lib/api/invoke";
import { invokePrivilegedAdmin } from "@/lib/api/admin-security";

export interface SoftwareStatus {
  kind: string;
  name: string;
  installed: boolean;
  path: string;
  managed: boolean;
  targetVersion: string;
}

export interface DownloadConfig {
  githubMirror: string;
  proxyMode: string;
  proxyUrl: string;
}

export async function listSoftware(): Promise<SoftwareStatus[]> {
  return invokeAdmin("list_software");
}

export async function installSoftware(kind: string, targetVersion: string): Promise<SoftwareStatus> {
  return invokePrivilegedAdmin("install_software", { kind }, { kind, version: targetVersion });
}

export async function uninstallSoftware(kind: string): Promise<SoftwareStatus> {
  return invokePrivilegedAdmin("uninstall_software", { kind }, { kind });
}

export async function getDownloadConfig(): Promise<DownloadConfig> {
  return invokeAdmin("get_download_config");
}

export async function setDownloadConfig(config: DownloadConfig): Promise<void> {
  return invokeAdmin("set_download_config", { config });
}
