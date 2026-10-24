import { describe, expect, it } from 'vitest'
import { featureOff, fill, fmtVal, isMonoKey, pickRows, toCsv } from './resource'

describe('fill', () => {
  it('substitutes row fields and encodes them', () => { expect(fill('/v1/x/{name}/{id}', { name: 'a b', id: 'k' })).toBe('/v1/x/a%20b/k') })
  it('prefers the explicit id for {id}', () => { expect(fill('/v1/x/{id}', { id: 'row' }, 'explicit')).toBe('/v1/x/explicit') })
  it('uses id as fallback for unknown fields', () => { expect(fill('/v1/x/{code}', {}, 'k')).toBe('/v1/x/k') })
})

describe('pickRows', () => {
  it('handles arrays, keyed payloads and first-array fallback', () => {
    expect(pickRows([{ id: 1 }])).toEqual([{ id: 1 }])
    expect(pickRows({ keys: [{ id: 'a' }], count: 1 }, 'keys')).toEqual([{ id: 'a' }])
    expect(pickRows({ n: 1, items: [{ id: 2 }] })).toEqual([{ id: 2 }])
  })
  it('turns lists of names into one-column rows', () => { expect(pickRows(['w1', 'w2'], undefined, 'name')).toEqual([{ name: 'w1' }, { name: 'w2' }]) })
  it('is empty for garbage', () => { expect(pickRows(null)).toEqual([]); expect(pickRows({ a: 1 })).toEqual([]) })
})

describe('fmtVal', () => {
  it('formats empties, booleans, timestamps and objects', () => {
    expect(fmtVal('x', null)).toBe('—'); expect(fmtVal('x', '')).toBe('—')
    expect(fmtVal('x', true)).toBe('yes')
    expect(fmtVal('created_at', 1700000000)).toBe('2023-11-14 22:13:20 UTC')
    expect(fmtVal('created_at', 1700000000000)).toBe('2023-11-14 22:13:20 UTC')
    expect(fmtVal('count', 1700000000)).toBe('1700000000')
    expect(fmtVal('x', { a: 1 })).toBe('{"a":1}')
  })
})

describe('isMonoKey', () => { it('matches ids, hashes, codes', () => { expect(isMonoKey('account_id')).toBe(true); expect(isMonoKey('sha256')).toBe(true); expect(isMonoKey('title')).toBe(false); expect(isMonoKey('identity')).toBe(false) }) })

describe('featureOff', () => {
  it('recognises disabled-feature responses only', () => {
    expect(featureOff({ status: 404, message: 'not found: edge is not enabled' })).toBe('edge is not enabled')
    expect(featureOff({ status: 404, message: 'not found: world' })).toBeNull()
    expect(featureOff({ status: 500, message: 'not enabled' })).toBeNull()
    expect(featureOff(null)).toBeNull()
  })
})

describe('toCsv', () => {
  it('quotes commas, quotes and newlines; serialises objects', () => {
    expect(toCsv(['a', 'b', 'c'], [{ a: 'x,y', b: 'say "hi"', c: { k: 1 } }, { a: null, b: 'line\nbreak' }])).toBe('a,b,c\n"x,y","say ""hi""","{""k"":1}"\n,"line\nbreak",')
  })
})
