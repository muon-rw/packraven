// Builds the jar that slim server packs run to download their mods. We commit the jar so building
// packraven doesn't need a JDK, and ship the source inside it so a test can catch a stale jar.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const dir = 'src-tauri/src/scripts/downloader';
const classes = mkdtempSync(join(tmpdir(), 'packraven-downloader-'));
try {
  execFileSync('javac', ['--release', '8', '-Xlint:-options', '-d', classes, `${dir}/PackravenDownloader.java`], { stdio: 'inherit' });
  execFileSync('jar', ['--create', '--file', `${dir}/packraven-downloader.jar`, '--main-class', 'PackravenDownloader', '-C', classes, '.', '-C', dir, 'PackravenDownloader.java'], { stdio: 'inherit' });
} finally {
  rmSync(classes, { recursive: true, force: true });
}
