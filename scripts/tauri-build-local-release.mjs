#!/usr/bin/env node

import { spawn } from 'node:child_process'
import { closeSync, existsSync, openSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(here, '..')
const localConfig = join(repoRoot, 'src-tauri', 'tauri.local.conf.json')
// A running BAT launched from src-tauri/target/release locks files the build must overwrite.
const targetDir = resolve(repoRoot, process.env.CARGO_TARGET_DIR || join('src-tauri', 'target', 'local'))

const args = [join(here, 'tauri-build-mode.mjs'), '--mode', 'all-in-one', '--config', localConfig]
// MSI rejects the non-numeric pre-release in 0.0.1-dev.
if (process.platform === 'win32') args.push('--bundles', 'nsis')
args.push(...process.argv.slice(2))

function newBundles(since) {
  const bundleDir = join(targetDir, 'release', 'bundle')
  if (!existsSync(bundleDir)) return []
  return readdirSync(bundleDir, { withFileTypes: true })
    .filter(entry => entry.isDirectory())
    .flatMap(entry => readdirSync(join(bundleDir, entry.name)).map(name => join(bundleDir, entry.name, name)))
    .filter(path => statSync(path).mtimeMs >= since)
}

// Windows refuses write access to a running executable, so EBUSY marks a live BAT in this target dir.
function runningExecutables() {
  if (process.platform !== 'win32') return []
  const release = join(targetDir, 'release')
  return [join(release, 'BetterAgentTerminal.exe'), join(release, 'node-runtime', 'windows-x86_64', 'node.exe')]
    .filter((path) => {
      if (!existsSync(path)) return false
      try {
        closeSync(openSync(path, 'r+'))
        return false
      } catch (err) {
        return err.code === 'EBUSY'
      }
    })
}

const running = runningExecutables()
if (running.length > 0) {
  console.error('[tauri-build-local-release] these files are in use and the build would fail to overwrite them:')
  for (const path of running) console.error(`  ${path}`)
  console.error('Close the BetterAgentTerminal started from this folder (or run the installed copy instead),')
  console.error('or set CARGO_TARGET_DIR to another directory.')
  process.exit(1)
}

const startedAt = Date.now()
console.log(`[tauri-build-local-release] CARGO_TARGET_DIR=${targetDir}`)
const child = spawn(process.execPath, args, {
  cwd: repoRoot,
  stdio: 'inherit',
  env: { ...process.env, CARGO_TARGET_DIR: targetDir },
})
child.on('exit', (code, signal) => {
  if (signal || code) {
    console.error(`[tauri-build-local-release] build failed (${signal || `exit ${code}`})`)
    process.exit(code || 1)
  }
  const bundles = newBundles(startedAt)
  console.log('[tauri-build-local-release] bundles:')
  for (const path of bundles) console.log(`  ${path}`)
  if (bundles.length === 0) console.log('  (none produced under release/bundle)')
})
child.on('error', (err) => {
  console.error(`[tauri-build-local-release] failed to spawn build: ${err.message}`)
  process.exit(1)
})
