import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const script = fileURLToPath(new URL('./check-code-structure.mjs', import.meta.url))

async function fixture(t, files, baseline = {}) {
  const root = await mkdtemp(path.join(tmpdir(), 'openllm-structure-'))
  t.after(() => rm(root, { recursive: true, force: true }))
  for (const directory of ['src', 'tests', 'web/src', 'scripts']) {
    await mkdir(path.join(root, directory), { recursive: true })
  }
  for (const [name, text] of Object.entries({
    ...files,
    'scripts/code-size-baseline.json': JSON.stringify(baseline),
  })) {
    await mkdir(path.dirname(path.join(root, name)), { recursive: true })
    await writeFile(path.join(root, name), text)
  }
  return spawnSync(process.execPath, [script, '--root', root], {
    encoding: 'utf8',
  })
}

test('counts LF and CRLF lines equally at each limit', async t => {
  const result = await fixture(t, {
    'src/domain.rs': 'line\r\n'.repeat(1000),
    'web/src/domain.ts': 'line\n'.repeat(600),
    'tests/unit.rs': 'line\n'.repeat(999) + 'last',
  })
  assert.equal(result.status, 0, result.stderr)
})

test('rejects oversized new Rust and frontend files', async t => {
  const result = await fixture(t, {
    'src/domain.rs': 'line\n'.repeat(1001),
    'web/src/domain.tsx': 'line\r\n'.repeat(601),
  })
  assert.equal(result.status, 1)
  assert.match(result.stderr, /src\/domain.rs: 1001/)
  assert.match(result.stderr, /web\/src\/domain.tsx: 601/)
})

test('requires baselines to ratchet down and disappear when compliant', async t => {
  const result = await fixture(t, {
    'src/shrinking.rs': 'line\n'.repeat(1001),
    'src/compliant.rs': 'line\n'.repeat(1000),
  }, {
    'src/shrinking.rs': { maxLines: 1100, reason: 'Separate query and rendering.' },
    'src/compliant.rs': { maxLines: 1100, reason: 'Separate query and rendering.' },
  })
  assert.equal(result.status, 1)
  assert.match(result.stderr, /reduce its baseline from 1100 to 1001/)
  assert.match(result.stderr, /remove its baseline entry/)
})

test('allows only documented existing exceptions and ignores build artifacts', async t => {
  const result = await fixture(t, {
    'src/legacy.rs': 'line\n'.repeat(1001),
    'web/src/node_modules/dependency.ts': 'line\n'.repeat(2000),
    'src/generated.json': 'line\n'.repeat(2000),
  }, {
    'src/legacy.rs': { maxLines: 1001, reason: 'Separate query and rendering.' },
  })
  assert.equal(result.status, 0, result.stderr)
})

test('rejects stale and undocumented baseline entries', async t => {
  const result = await fixture(t, { 'src/legacy.rs': 'line\n'.repeat(1001) }, {
    'src/legacy.rs': { maxLines: 1001 },
    'src/missing.rs': { maxLines: 1100, reason: 'Missing source.' },
  })
  assert.equal(result.status, 1)
  assert.match(result.stderr, /an exception requires/)
  assert.match(result.stderr, /remove the stale baseline entry/)
})
