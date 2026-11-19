import { expect, test } from '@playwright/test'
import { isUpTo12111, mavenSnippet, rawFileId, rawProjectId } from '../src/lib/maven.ts'

test('recognizes obfuscated Minecraft versions through 1.21.11', () => {
  expect(isUpTo12111('1.16.5')).toBe(true)
  expect(isUpTo12111('1.20.1')).toBe(true)
  expect(isUpTo12111('1.21')).toBe(true)
  expect(isUpTo12111('1.21.1')).toBe(true)
  expect(isUpTo12111('1.21.11')).toBe(true)
  expect(isUpTo12111('1.21.12')).toBe(false)
  expect(isUpTo12111('1.22')).toBe(false)
  expect(isUpTo12111('26.1')).toBe(false)
  expect(isUpTo12111('26.1.1')).toBe(false)
  expect(isUpTo12111(null)).toBe(false)
})

test('formats CurseMaven and Modrinth Maven snippets per loader and Minecraft version', () => {
  const cf = { key: 'cf:238222', file: 'cf:5846810', slug: 'jei' }
  const mr = { key: 'mr:AANobbMI', file: 'mr:uMOpc5uV', slug: 'sodium' }

  expect(mavenSnippet({ ...cf, minecraft: '1.21.1', loader: 'neoforge-21.1.72' })).toBe('implementation("curse.maven:jei-238222:5846810")')
  expect(mavenSnippet({ ...cf, minecraft: '1.21.11', loader: 'fabric-0.16.5' })).toBe('modImplementation("curse.maven:jei-238222:5846810")')
  expect(mavenSnippet({ ...cf, minecraft: '26.1', loader: 'fabric-0.18.0' })).toBe('implementation("curse.maven:jei-238222:5846810")')
  expect(mavenSnippet({ ...cf, minecraft: '1.20.1', loader: 'forge-47.3.0' })).toBe('implementation(fg.deobf("curse.maven:jei-238222:5846810"))')
  expect(mavenSnippet({ ...cf, minecraft: '26.1', loader: 'forge-60.0.0' })).toBe('implementation("curse.maven:jei-238222:5846810")')

  expect(mavenSnippet({ ...mr, minecraft: '1.21.1', loader: 'fabric-0.16.5' })).toBe('modImplementation("maven.modrinth:sodium:uMOpc5uV")')
  expect(mavenSnippet({ ...mr, minecraft: '1.21.1', loader: 'neoforge-21.1.72' })).toBe('implementation("maven.modrinth:sodium:uMOpc5uV")')
  expect(mavenSnippet({ ...mr, minecraft: '1.20.1', loader: 'forge-47.3.0' })).toBe('implementation(fg.deobf("maven.modrinth:sodium:uMOpc5uV"))')
})

test('extracts raw project and file IDs', () => {
  expect(rawProjectId('cf:238222')).toBe('238222')
  expect(rawProjectId('mr:AANobbMI')).toBe('AANobbMI')
  expect(rawFileId('cf:5846810')).toBe('5846810')
  expect(rawFileId('mr:uMOpc5uV')).toBe('uMOpc5uV')
  expect(rawFileId(null)).toBeNull()
})
