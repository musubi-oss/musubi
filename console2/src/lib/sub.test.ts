import { describe, expect, it } from 'vitest'
import { listOf } from './sub'

describe('listOf', () => {
  it('returns a bare array as-is', () => { expect(listOf([{ a: 1 }])).toEqual([{ a: 1 }]) })
  it('picks the named sub-field', () => { expect(listOf({ total: 2, events: [{ n: 1 }], other: [9] }, 'events')).toEqual([{ n: 1 }]) })
  it('falls back to the first array-valued field', () => { expect(listOf({ count: 1, rows: [{ x: 1 }] })).toEqual([{ x: 1 }]) })
  it('is empty for scalars, null and non-array fields', () => { expect(listOf(null)).toEqual([]); expect(listOf('x')).toEqual([]); expect(listOf({ a: 1 })).toEqual([]); expect(listOf({ events: 3 }, 'events')).toEqual([]) })
})
