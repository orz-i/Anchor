import { spawnSync } from 'node:child_process';

const mode = process.argv[2];
const passthrough = process.argv.slice(3);
const scripts = {
  dev: 'legacy:desktop',
  build: 'legacy:desktop:build',
  manifest: 'legacy:desktop:manifest',
  tauri: 'legacy:tauri',
};

const target = scripts[mode];
if (!target) {
  console.error(`[desktop] Unknown compatibility mode: ${mode ?? '<missing>'}`);
  process.exit(2);
}

console.error(
  `[desktop] DEPRECATED: the Tauri desktop shell is no longer a default Anchor build or release target. ` +
    `Use \`pnpm ${target}\` only for legacy compatibility validation.`,
);

const pnpm = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm';
const args = [target];
if (passthrough.length > 0) args.push('--', ...passthrough);
const result = spawnSync(pnpm, args, { stdio: 'inherit' });
if (result.error) {
  console.error(`[desktop] Failed to invoke ${pnpm}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
