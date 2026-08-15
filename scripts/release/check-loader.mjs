#!/usr/bin/env node
/**
 * Unit-style check of the bundle's binary resolution (the loader contract):
 *
 *   1. With no platform package installed, the loader falls back to the
 *      bundle's own `bin/dsh-tui` (the source-build placeholder).
 *   2. With a fake per-platform package on disk, the loader picks its
 *      binary (the prebuild path wins over the local bin).
 *
 * The fake package lives in `bundle/node_modules/` and is removed after the
 * run. Resolution is relative to the bundle, exactly like a real install.
 */

import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { resolveBinary } from '../../bundle/lib/index.js'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const bundleDir = path.join(root, 'bundle')
const platformPackage = `@rbelem/dsh-tui-${process.platform}-${process.arch}`
const fakeDir = path.join(bundleDir, 'node_modules', platformPackage)

let failures = 0
const check = (label, ok) => {
  console.log(`${ok ? 'ok' : 'FAIL'} — ${label}`)
  if (!ok) failures += 1
}

// 1. No platform package: the fallback resolves to the bundle's own bin.
const fallback = resolveBinary({})
check(
  'fallback resolves to bundle/bin/dsh-tui',
  path.resolve(fallback) === path.resolve(bundleDir, 'bin', 'dsh-tui'),
)

// 2. With a fake platform package installed, the prebuild path wins.
fs.mkdirSync(path.join(fakeDir, 'bin'), { recursive: true })
fs.writeFileSync(path.join(fakeDir, 'package.json'), JSON.stringify({ name: platformPackage, version: '0.1.0' }))
fs.writeFileSync(path.join(fakeDir, 'bin', 'dsh-tui'), '#!/bin/sh\necho fake prebuild\n')
fs.chmodSync(path.join(fakeDir, 'bin', 'dsh-tui'), 0o755)

const prebuild = resolveBinary({})
check(
  'prebuild path wins over the local bin',
  path.resolve(prebuild) === path.resolve(fakeDir, 'bin', 'dsh-tui'),
)
check('prebuild binary is executable', fs.accessSync(prebuild, fs.constants.X_OK) === undefined)

// A config `binary` override beats both.
const override = resolveBinary({ binary: '/tmp/some-binary' })
check('config binary override wins', override === '/tmp/some-binary')

fs.rmSync(path.join(bundleDir, 'node_modules'), { recursive: true, force: true })

if (failures > 0) {
  console.error(`check-loader: ${failures} failure(s)`)
  process.exit(1)
}
console.log('check-loader: ok')
