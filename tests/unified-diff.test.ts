import * as assert from 'assert'
import { buildInlineFileRows, changeBlockEnd, changeBlockStarts, diffWords, parseUnifiedDiff, type DiffRow } from '../renderer/src/utils/unified-diff'

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

type Line = [kind: string, oldLine: number | undefined, newLine: number | undefined, text: string]

function inlineLines(content: string, fileDiff: string): Line[] | null {
  const inline = buildInlineFileRows(content, fileDiff)
  return inline && inline.map(row => [row.kind, 'oldLine' in row ? row.oldLine : undefined, 'newLine' in row ? row.newLine : undefined, row.text])
}

const fileDiff = [
  'diff --git a/sample.txt b/sample.txt',
  'index 1111111..2222222 100644',
  '--- a/sample.txt',
  '+++ b/sample.txt',
  '@@ -1,4 +1,5 @@',
  '-first\r',
  ' keep1\r',
  '-value = 1\r',
  '+value = 2\r',
  '+inserted\r',
  '+inserted2\r',
  ' keep2\r',
  '@@ -8,2 +9 @@',
  ' tail\r',
  '-last',
  '\\ No newline at end of file',
  '',
].join('\n')
const fileContent = 'keep1\r\nvalue = 2\r\ninserted\r\ninserted2\r\nkeep2\r\nkeep3\r\nkeep4\r\nkeep5\r\ntail\r\n'
assert.deepEqual(inlineLines(fileContent, fileDiff), [
  ['del', 1, undefined, 'first'],
  ['context', 2, 1, 'keep1'],
  ['del', 3, undefined, 'value = 1'],
  ['add', undefined, 2, 'value = 2'],
  ['add', undefined, 3, 'inserted'],
  ['add', undefined, 4, 'inserted2'],
  ['context', 4, 5, 'keep2'],
  ['context', 5, 6, 'keep3'],
  ['context', 6, 7, 'keep4'],
  ['context', 7, 8, 'keep5'],
  ['context', 8, 9, 'tail'],
  ['del', 9, undefined, 'last'],
], 'multi-hunk diff with deletions at both file ends')
const inlineRows = buildInlineFileRows(fileContent, fileDiff)
assert.deepEqual(changedText(inlineRows?.[2]), ['1'])
assert.deepEqual(changedText(inlineRows?.[3]), ['2'])
assert.equal(inlineRows?.[4].kind === 'add' && inlineRows[4].segments, undefined, 'unpaired additions get no word highlight')
assert.deepEqual(inlineLines(fileContent.replace(/\r\n$/, ''), fileDiff), inlineLines(fileContent, fileDiff), 'a missing final newline does not change the lines')

assert.deepEqual(inlineLines('a\nB\nc\nd\n', '@@ -2 +2 @@\n-b\n+B'), [
  ['context', 1, 1, 'a'],
  ['del', 2, undefined, 'b'],
  ['add', undefined, 2, 'B'],
  ['context', 3, 3, 'c'],
  ['context', 4, 4, 'd'],
], 'unchanged lines after the last hunk keep counting old lines')
assert.deepEqual(inlineLines('a\nx\nb\n', '@@ -1,0 +2 @@\n+x'), [
  ['context', 1, 1, 'a'],
  ['add', undefined, 2, 'x'],
  ['context', 2, 3, 'b'],
], 'zero-context insertion')
assert.deepEqual(inlineLines('a\nb\n', '@@ -2 +1,0 @@\n-x'), [
  ['context', 1, 1, 'a'],
  ['del', 2, undefined, 'x'],
  ['context', 3, 2, 'b'],
], 'zero-context deletion')

assert.deepEqual(inlineLines('first\nsecond', '@@ -0,0 +1,2 @@\n+first\n+second'), [
  ['add', undefined, 1, 'first'],
  ['add', undefined, 2, 'second'],
], 'new file')
assert.deepEqual(inlineLines('', '@@ -1,2 +0,0 @@\n-gone\n-too'), [
  ['del', 1, undefined, 'gone'],
  ['del', 2, undefined, 'too'],
], 'whole file deleted')

assert.deepEqual(inlineLines('a\r\n\r\nb\r\n', ''), [
  ['context', 1, 1, 'a'],
  ['context', 2, 2, ''],
  ['context', 3, 3, 'b'],
], 'empty diff shows the file unchanged')
assert.deepEqual(inlineLines('', ''), [])
assert.deepEqual(inlineLines('a\n', 'diff --git a/x b/x\nold mode 100644\nnew mode 100755'), [['context', 1, 1, 'a']], 'diff without hunks')

assert.equal(buildInlineFileRows(fileContent.replace('inserted\r', 'other\r'), fileDiff), null, 'stale added line')
assert.equal(buildInlineFileRows(fileContent.replace('keep2', 'keep9'), fileDiff), null, 'stale context line')
assert.equal(buildInlineFileRows(fileContent.replace('keep4\r\n', ''), fileDiff), null, 'hunk shifted by stale content')
assert.equal(buildInlineFileRows('a\n', '@@ -5 +5 @@\n-x\n+y'), null, 'hunk beyond the end of the file')
assert.equal(buildInlineFileRows('a\nb\nc\n', '@@ -1,2 +1,2 @@\n a\n b\n@@ -2 +2 @@\n b'), null, 'overlapping hunks')
assert.equal(buildInlineFileRows('a\nb\n', '@@ -1 +1 @@\n a\n@@ -2 +3 @@\n b'), null, 'hunk gap that differs between old and new sides')
assert.equal(buildInlineFileRows('a\n', '@@ -1 +1 @@\n a\n+b'), null, 'added line past the end of the file')

function kindRows(kinds: string): DiffRow[] {
  const byCode: Record<string, DiffRow['kind']> = { c: 'context', a: 'add', d: 'del' }
  return [...kinds].map(code => ({ kind: byCode[code], text: code }) as DiffRow)
}

function blocks(kinds: string): [number, number][] {
  const blockRows = kindRows(kinds)
  return changeBlockStarts(blockRows).map(start => [start, changeBlockEnd(blockRows, start)])
}

assert.deepEqual(changeBlockStarts([]), [], 'no rows')
assert.deepEqual(blocks('ccc'), [], 'no changes')
assert.deepEqual(blocks('cac'), [[1, 2]], 'single block')
assert.deepEqual(blocks('cdac'), [[1, 3]], 'del followed by add is one block')
assert.deepEqual(blocks('cadc'), [[1, 3]], 'add followed by del is one block')
assert.deepEqual(blocks('acd'), [[0, 1], [2, 3]], 'blocks at the start and end of the file')
assert.deepEqual(blocks('dacccaacd'), [[0, 2], [5, 7], [8, 9]], 'context separates blocks')
assert.deepEqual(blocks('aaa'), [[0, 3]], 'whole file changed')
assert.deepEqual(changeBlockStarts(inlineRows ?? []), [0, 2, 11], 'blocks of a built inline file')

console.log('unified-diff tests passed')
