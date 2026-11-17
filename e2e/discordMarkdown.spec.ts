import { expect, test } from '@playwright/test'
import { emojiUrl, formatMessageTime, formatTimestamp, isEmojiOnly, parseBlocks, parseInline } from '../src/lib/discordMarkdown.ts'

// These run in Node without a browser, since the parser builds a plain tree.

test('inline formatting nests the way Discord parses it', () => {
  expect(parseInline('**bold *both*** __under__ ~~gone~~ `a *b*`')).toEqual([
    { type: 'bold', children: [{ type: 'text', text: 'bold ' }, { type: 'italic', children: [{ type: 'text', text: 'both' }] }] },
    { type: 'text', text: ' ' },
    { type: 'underline', children: [{ type: 'text', text: 'under' }] },
    { type: 'text', text: ' ' },
    { type: 'strike', children: [{ type: 'text', text: 'gone' }] },
    { type: 'text', text: ' ' },
    { type: 'code', text: 'a *b*' },
  ])
  expect(parseInline('_it_ and snake_case_name')).toEqual([{ type: 'italic', children: [{ type: 'text', text: 'it' }] }, { type: 'text', text: ' and snake_case_name' }])
  expect(parseInline('\\*not italic\\*')).toEqual([{ type: 'text', text: '*not italic*' }])
  expect(parseInline('||secret||')).toEqual([{ type: 'spoiler', children: [{ type: 'text', text: 'secret' }] }])
  expect(parseInline('a\nb')).toEqual([{ type: 'text', text: 'a' }, { type: 'br' }, { type: 'text', text: 'b' }])
})

test('links only allow http and https, and titles take no links', () => {
  expect(parseInline('[Install **now**](https://cf/files/7)')).toEqual([
    { type: 'link', url: 'https://cf/files/7', children: [{ type: 'text', text: 'Install ' }, { type: 'bold', children: [{ type: 'text', text: 'now' }] }] },
  ])
  expect(parseInline('[x](javascript:alert(1))')).toEqual([{ type: 'text', text: '[x](javascript:alert(1))' }])
  expect(parseInline('see https://example.com/a_b, or <https://example.com>.')).toEqual([
    { type: 'text', text: 'see ' },
    { type: 'link', url: 'https://example.com/a_b', children: [{ type: 'text', text: 'https://example.com/a_b' }] },
    { type: 'text', text: ', or ' },
    { type: 'link', url: 'https://example.com', children: [{ type: 'text', text: 'https://example.com' }] },
    { type: 'text', text: '.' },
  ])
  expect(parseInline('[Pack](https://cf) https://cf', { links: false })).toEqual([{ type: 'text', text: '[Pack](https://cf) https://cf' }])
})

test('markup in the source stays text', () => {
  expect(parseInline('<img src=x onerror=alert(1)> <b>hi</b>')).toEqual([{ type: 'text', text: '<img src=x onerror=alert(1)> <b>hi</b>' }])
})

test('emoji, mentions and timestamps', () => {
  expect(parseInline('<:raven:123456> <a:flap:789> <@1> <@!2> <@&3> <#4> @everyone @here <t:1700000000:R> <t:1700000000>')).toEqual([
    { type: 'emoji', name: 'raven', id: '123456', animated: false },
    { type: 'text', text: ' ' },
    { type: 'emoji', name: 'flap', id: '789', animated: true },
    { type: 'text', text: ' ' },
    { type: 'mention', kind: 'user', id: '1' },
    { type: 'text', text: ' ' },
    { type: 'mention', kind: 'user', id: '2' },
    { type: 'text', text: ' ' },
    { type: 'mention', kind: 'role', id: '3' },
    { type: 'text', text: ' ' },
    { type: 'mention', kind: 'channel', id: '4' },
    { type: 'text', text: ' ' },
    { type: 'mention', kind: 'everyone' },
    { type: 'text', text: ' ' },
    { type: 'mention', kind: 'here' },
    { type: 'text', text: ' ' },
    { type: 'timestamp', unix: 1700000000, style: 'R' },
    { type: 'text', text: ' ' },
    { type: 'timestamp', unix: 1700000000, style: 'f' },
  ])
  expect(emojiUrl({ id: '123', animated: false })).toBe('https://cdn.discordapp.com/emojis/123.webp?size=48')
  expect(emojiUrl({ id: '123', animated: true })).toBe('https://cdn.discordapp.com/emojis/123.gif?size=48')
})

test('blocks: headings, subtext, quotes, lists, code and blank lines', () => {
  const text = (value: string) => [{ type: 'text', text: value }]
  expect(parseBlocks('# Big\n## Mid\n### Small\n#### Not a heading\n-# quiet')).toEqual([
    { type: 'heading', level: 1, children: text('Big') },
    { type: 'heading', level: 2, children: text('Mid') },
    { type: 'heading', level: 3, children: text('Small') },
    { type: 'paragraph', children: text('#### Not a heading') },
    { type: 'subtext', children: text('quiet') },
  ])
  expect(parseBlocks('Pack 1.0\n\n- Foo\n  - Nested\n- Bar\n\n3. three\n4. four')).toEqual([
    { type: 'paragraph', children: text('Pack 1.0') },
    { type: 'blank' },
    {
      type: 'list',
      ordered: false,
      start: 1,
      items: [
        [{ type: 'paragraph', children: text('Foo') }, { type: 'list', ordered: false, start: 1, items: [[{ type: 'paragraph', children: text('Nested') }]] }],
        [{ type: 'paragraph', children: text('Bar') }],
      ],
    },
    { type: 'blank' },
    { type: 'list', ordered: true, start: 3, items: [[{ type: 'paragraph', children: text('three') }], [{ type: 'paragraph', children: text('four') }]] },
  ])
  expect(parseBlocks('> one\n> two\nafter')).toEqual([{ type: 'quote', children: [{ type: 'paragraph', children: [...text('one'), { type: 'br' }, ...text('two')] }] }, { type: 'paragraph', children: text('after') }])
  expect(parseBlocks('before\n>>> all\nof this')).toEqual([{ type: 'paragraph', children: text('before') }, { type: 'quote', children: [{ type: 'paragraph', children: [...text('all'), { type: 'br' }, ...text('of this')] }] }])
  expect(parseBlocks('```js\nconst a = **1**\n```\nafter')).toEqual([{ type: 'codeblock', language: 'js', text: 'const a = **1**' }, { type: 'paragraph', children: text('after') }])
})

test('emoji-only content is shown large', () => {
  expect(isEmojiOnly(parseBlocks('<:raven:1> 🎉 <a:flap:2>'))).toBe(true)
  expect(isEmojiOnly(parseBlocks('<:raven:1> hi'))).toBe(false)
  expect(isEmojiOnly(parseBlocks('plain'))).toBe(false)
})

test('timestamps format like Discord', () => {
  const at = 1700000000
  const now = new Date(at * 1000 + 1 * 3600 * 1000)
  const options = { now, locale: 'en-US', timeZone: 'UTC' }
  // Newer ICU versions put a narrow no-break space before AM and PM.
  const spaced = (text: string) => text.replace(/\s/g, ' ')
  expect(formatTimestamp(at, 'R', options)).toBe('1 hour ago')
  expect(formatTimestamp(at, 'D', options)).toBe('November 14, 2023')
  expect(spaced(formatTimestamp(at, 't', options))).toBe('10:13 PM')
  expect(spaced(formatMessageTime(new Date(at * 1000), options))).toBe('Today at 10:13 PM')
  expect(spaced(formatMessageTime(new Date(at * 1000), { ...options, now: new Date(at * 1000 + 24 * 3600 * 1000) }))).toBe('Yesterday at 10:13 PM')
})
