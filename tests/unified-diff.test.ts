import * as assert from 'assert'
import { diffWords, getFileChangeMarkers, parseUnifiedDiff, type DiffRow } from '../renderer/src/utils/unified-diff'

function changedText(row: DiffRow | undefined): string[] {
  assert.ok(row && 'segments' in row && row.segments, 'expected word segments')
  return row.segments.filter(s => s.changed).map(s => s.text)
}

const diff = [
  'diff --git a/version.h b/version.h',
  'index 1111111..2222222 100644',
  '--- a/version.h',
  '+++ b/version.h',
  '@@ -79,4 +79,4 @@ // Driver',
  ' // Driver version',
  '-#define DRIVER_VERSION "4.0.9901.0003"\r',
  '+#define DRIVER_VERSION "4.5.7901.7867"\r',
  ' ',
  '-old only',
  '\\ No newline at end of file',
  '+completely different words here',
  '@@ -120,2 +120,3 @@',
  ' keep',
  '+added',
  ' tail',
  '',
].join('\n')

const rows = parseUnifiedDiff(diff)
assert.equal(rows.some(r => r.text.startsWith('diff --git') || r.text.startsWith('+++')), false, 'file headers are dropped')
assert.deepEqual(rows.map(r => r.kind), ['hunk', 'context', 'del', 'add', 'context', 'del', 'meta', 'add', 'hunk', 'context', 'add', 'context'])

const [, context, del, add, blank] = rows
assert.deepEqual(context, { kind: 'context', text: '// Driver version', oldLine: 79, newLine: 79 })
assert.equal(del.kind === 'del' && del.oldLine, 80)
assert.equal(add.kind === 'add' && add.newLine, 80)
assert.equal(add.text, '#define DRIVER_VERSION "4.5.7901.7867"', 'trailing CR is stripped')
assert.deepEqual(changedText(del), ['0', '9901', '0003'])
assert.deepEqual(changedText(add), ['5', '7901', '7867'])
assert.deepEqual(blank, { kind: 'context', text: '', oldLine: 81, newLine: 81 })

const unrelatedAdd = rows[7]
assert.equal(unrelatedAdd.kind === 'add' && unrelatedAdd.segments, undefined, 'dissimilar lines get no word highlight')

const secondHunkAdd = rows[10]
assert.equal(secondHunkAdd.kind === 'add' && secondHunkAdd.newLine, 121)
assert.deepEqual(rows[11], { kind: 'context', text: 'tail', oldLine: 121, newLine: 122 })

assert.equal(diffWords('', 'x'), null)
assert.deepEqual(parseUnifiedDiff('fatal: bad revision'), [{ kind: 'meta', text: 'fatal: bad revision' }])
assert.deepEqual(parseUnifiedDiff(''), [])

const fileDiff = [
  '@@ -1,5 +1,5 @@',
  ' keep',
  '-old',
  '+changed',
  '+inserted',
  ' tail',
  '-gone',
  ' end',
].join('\n')
const fileContent = 'keep\r\nchanged\r\ninserted\r\ntail\r\nend\r\n'
assert.deepEqual([...getFileChangeMarkers(fileContent, fileDiff)], [
  [2, 'modified'],
  [3, 'add'],
  [5, 'deleted'],
])
assert.deepEqual([...getFileChangeMarkers(fileContent.replace('changed', 'newer'), fileDiff)], [], 'stale file content is not marked')
assert.deepEqual([...getFileChangeMarkers('first\nsecond', '@@ -0,0 +1,2 @@\n+first\n+second')], [
  [1, 'add'],
  [2, 'add'],
])
assert.deepEqual([...getFileChangeMarkers('', '@@ -1 +0,0 @@\n-gone')], [[1, 'deleted']])

console.log('unified-diff tests passed')
