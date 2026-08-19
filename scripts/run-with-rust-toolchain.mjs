import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const [requestedProgram, ...requestedArgs] = process.argv.slice(2);

if (!requestedProgram) {
  console.error('Usage: node scripts/run-with-rust-toolchain.mjs <cargo|program> [...args]');
  process.exit(2);
}

function rustupWhich(tool) {
  const result = spawnSync('rustup', ['which', tool], {
    cwd: repoRoot,
    encoding: 'utf8',
    env: process.env,
    windowsHide: true
  });
  const resolved = result.stdout?.trim();
  if (result.status !== 0 || !resolved || !existsSync(resolved)) {
    const detail = result.stderr?.trim() || `rustup which ${tool} returned no usable path`;
    throw new Error(detail);
  }
  return resolved;
}

const env = { ...process.env };
const rustTools = {};

if (process.platform === 'win32') {
  for (const tool of ['cargo', 'rustc', 'rustdoc']) {
    rustTools[tool] = rustupWhich(tool);
    env[tool.toUpperCase()] = rustTools[tool];
  }
  const toolchainBin = dirname(rustTools.cargo);
  const pathKey = Object.keys(env).find((key) => key.toLowerCase() === 'path') || 'PATH';
  env[pathKey] = `${toolchainBin};${env[pathKey] || ''}`;
  for (const key of Object.keys(env)) {
    if (key !== pathKey && key.toLowerCase() === 'path') delete env[key];
  }
}

let program = requestedProgram;
let args = requestedArgs;

if (rustTools[requestedProgram]) {
  program = rustTools[requestedProgram];
}

const child = spawn(program, args, {
  cwd: repoRoot,
  env,
  stdio: 'inherit',
  windowsHide: true
});

const signalHandlers = new Map();
for (const signal of ['SIGINT', 'SIGTERM']) {
  const handler = () => {
    if (!child.killed) child.kill(signal);
  };
  signalHandlers.set(signal, handler);
  process.on(signal, handler);
}

function removeSignalHandlers() {
  for (const [signal, handler] of signalHandlers) process.off(signal, handler);
}

child.on('error', (error) => {
  removeSignalHandlers();
  console.error(`Failed to start ${requestedProgram}: ${error.message}`);
  process.exitCode = 1;
});

child.on('exit', (code, signal) => {
  removeSignalHandlers();
  if (signal) {
    console.error(`${requestedProgram} terminated by ${signal}`);
    process.exitCode = 1;
  } else {
    process.exitCode = code ?? 1;
  }
});
