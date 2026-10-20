import { readdir, readFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const arguments_ = process.argv.slice(2)
if (arguments_.length && (arguments_.length !== 2 || arguments_[0] !== '--root')) {
  throw new Error('Usage: node scripts/check-code-structure.mjs [--root <directory>]')
}
const root = arguments_.length
  ? path.resolve(arguments_[1])
  : fileURLToPath(new URL('../', import.meta.url))
const baseline = JSON.parse(
  await readFile(path.join(root, 'scripts/code-size-baseline.json'), 'utf8'),
)
const limits = new Map([
  ['.rs', 1000],
  ['.ts', 600],
  ['.tsx', 600],
  ['.js', 600],
  ['.jsx', 600],
  ['.mjs', 600],
  ['.cjs', 600],
  ['.css', 600],
])
const skipped = new Set(['node_modules', 'target', 'dist', '.git', '.pi'])
const failures = []
const sizes = new Map()

async function scan(directory) {
  for (const entry of await readdir(path.join(root, directory), { withFileTypes: true })) {
    const relative = path.posix.join(directory, entry.name)
    if (entry.isDirectory() && !skipped.has(entry.name)) {
      await scan(relative)
    } else if (entry.isFile() && limits.has(path.extname(entry.name))) {
      const text = await readFile(path.join(root, relative), 'utf8')
      const normalized = text.replace(/\r\n/g, '\n')
      const count = normalized === '' ? 0 : normalized.split('\n').length - Number(normalized.endsWith('\n'))
      const limit = limits.get(path.extname(entry.name))
      const exception = baseline[relative]
      sizes.set(relative, { count, limit })
      if (count > (exception?.maxLines ?? limit)) {
        failures.push(`${relative}: ${count} lines exceeds ${exception?.maxLines ?? limit}; split by responsibility.`)
      }
    }
  }
}

await Promise.all(['src', 'tests', 'web/src', 'scripts'].map(scan))
for (const [file, exception] of Object.entries(baseline)) {
  const current = sizes.get(file)
  if (!current) {
    failures.push(`${file}: remove the stale baseline entry.`)
    continue
  }
  if (!Number.isInteger(exception.maxLines) || !exception.reason?.trim()) {
    failures.push(`${file}: an exception requires an integer maxLines and a reason.`)
  } else if (current.count <= current.limit) {
    failures.push(`${file}: now within the limit; remove its baseline entry.`)
  } else if (current.count < exception.maxLines) {
    failures.push(`${file}: reduce its baseline from ${exception.maxLines} to ${current.count}.`)
  }
}

if (failures.length) {
  console.error(`Code structure check failed:\n${failures.map(message => `- ${message}`).join('\n')}`)
  process.exitCode = 1
} else {
  console.log(`Code structure check passed: ${sizes.size} files, ${Object.keys(baseline).length} existing exceptions.`)
}
