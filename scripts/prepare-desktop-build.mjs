import {
  existsSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync
} from 'node:fs';
import { dirname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), '..'));
const targetDir = join(repoRoot, 'src-tauri', 'target');
const markerPath = join(targetDir, '.anchor-workspace-root');

function comparablePath(value) {
  const normalized = normalize(value)
    .replace(/^\\\\\?\\/, '')
    .replace(/[\\/]+$/, '');

  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

function readPreviousRoot() {
  try {
    return comparablePath(readFileSync(markerPath, 'utf8').trim());
  } catch (error) {
    if (error?.code === 'ENOENT') return null;
    throw error;
  }
}

const currentRoot = comparablePath(repoRoot);
const previousRoot = readPreviousRoot();

// Tauri permission artifacts and Rust build outputs can contain absolute paths.
// A target directory copied or moved with the repository is therefore unsafe to reuse.
if (existsSync(targetDir) && previousRoot !== currentRoot) {
  const reason = previousRoot === null ? 'untracked' : 'relocated';
  console.log(`[desktop:build] Cleaning ${reason} Cargo target cache.`);
  rmSync(targetDir, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
}

mkdirSync(targetDir, { recursive: true });
writeFileSync(markerPath, `${repoRoot}\n`, 'utf8');
