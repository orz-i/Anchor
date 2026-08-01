import { message } from "@tauri-apps/plugin-dialog";

export async function reloadServiceAfterConfigSave<T>(
  serviceRunning: boolean,
  serviceLabel: string,
  reload: () => Promise<T>,
): Promise<T | null> {
  if (!serviceRunning) return null;
  const result = await reload();
  await message(`配置已生效，${serviceLabel} listener 已受控重载。现有隧道和公网链接保持不变。`, {
    title: "配置已热重载",
    kind: "info",
  });
  return result;
}
