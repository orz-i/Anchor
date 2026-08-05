import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync
} from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const releaseRoot = join(repoRoot, 'src-tauri', 'target', 'release');
const bundleRoot = join(releaseRoot, 'bundle');
const manifestPath = join(bundleRoot, 'anchor-build-manifest.json');

function git(args) {
  return execFileSync('git', ['-C', repoRoot, ...args], {
    encoding: 'utf8',
    windowsHide: true
  }).trim();
}

function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function trackedSourceDigest() {
  const hash = createHash('sha256');
  const files = [
    ...git(['ls-files', '-z']).split('\0').filter(Boolean),
    ...git(['ls-files', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean)
  ]
    .filter((path, index, values) => values.indexOf(path) === index)
    .sort();
  for (const path of files) {
    const absolute = join(repoRoot, path);
    hash.update(path.replaceAll('\\', '/'));
    hash.update('\0');
    if (existsSync(absolute)) hash.update(readFileSync(absolute));
    else hash.update('<missing>');
    hash.update('\0');
  }
  return { algorithm: 'sha256', digest: hash.digest('hex'), source_file_count: files.length };
}

function catalogVersion() {
  const registry = readFileSync(join(repoRoot, 'src-tauri', 'src', 'tools', 'registry.rs'), 'utf8');
  const match = registry.match(/CATALOG_VERSION:\s*u32\s*=\s*(\d+)/);
  if (!match) throw new Error('Unable to read CATALOG_VERSION');
  return Number(match[1]);
}

function walkArtifacts(root) {
  if (!existsSync(root)) return [];
  const artifacts = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const absolute = join(directory, entry.name);
      if (entry.isDirectory()) visit(absolute);
      else if (entry.isFile() && absolute !== manifestPath && /\.(exe|msi)$/i.test(entry.name)) {
        const stats = statSync(absolute);
        artifacts.push({
          path: relative(repoRoot, absolute).replaceAll('\\', '/'),
          size_bytes: stats.size,
          sha256: sha256File(absolute)
        });
      }
    }
  };
  visit(root);
  return artifacts.sort((left, right) => left.path.localeCompare(right.path));
}

const packageJson = JSON.parse(readFileSync(join(repoRoot, 'package.json'), 'utf8'));
const changedFiles = git(['status', '--porcelain=v1', '--untracked-files=all'])
  .split(/\r?\n/)
  .filter(Boolean);
const artifacts = [
  ...(existsSync(join(releaseRoot, 'anchor-desktop.exe'))
    ? [join(releaseRoot, 'anchor-desktop.exe')]
    : [])
].map((absolute) => {
  const stats = statSync(absolute);
  return {
    path: relative(repoRoot, absolute).replaceAll('\\', '/'),
    size_bytes: stats.size,
    sha256: sha256File(absolute)
  };
});
artifacts.push(...walkArtifacts(bundleRoot));

const manifest = {
  format: 'anchor.desktop-build-manifest',
  schema_version: 1,
  created_at: new Date().toISOString(),
  package_version: packageJson.version,
  catalog_version: catalogVersion(),
  git: {
    head: git(['rev-parse', 'HEAD']),
    dirty: changedFiles.length > 0,
    changed_files: changedFiles
  },
  source: trackedSourceDigest(),
  artifacts
};

if (artifacts.length === 0) throw new Error('No desktop build artifacts were found');
mkdirSync(bundleRoot, { recursive: true });
writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
console.log(`[desktop:build] Wrote ${relative(repoRoot, manifestPath)} for ${artifacts.length} artifacts.`);
