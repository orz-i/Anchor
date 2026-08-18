import { invokeAdmin } from "$lib/api/invoke";

export interface SoftwareStatus {
  kind: string;
  name: string;
  installed: boolean;
  path: string;
  managed: boolean;
}

export interface DownloadConfig {
  githubMirror: string;
  proxyMode: string;
  proxyUrl: string;
}

export async function listSoftware(): Promise<SoftwareStatus[]> {
  return invokeAdmin("list_software");
}

export async function installSoftware(kind: string): Promise<SoftwareStatus> {
  return invokeAdmin("install_software", { kind });
}

export async function uninstallSoftware(kind: string): Promise<SoftwareStatus> {
  return invokeAdmin("uninstall_software", { kind });
}

export async function getDownloadConfig(): Promise<DownloadConfig> {
  return invokeAdmin("get_download_config");
}

export async function setDownloadConfig(config: DownloadConfig): Promise<void> {
  return invokeAdmin("set_download_config", { config });
}
