import { createReadStream } from 'node:fs';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs/promises';
import path from 'node:path';

const ARCHITECTURES = ['linux-x86_64', 'linux-aarch64'];
const ARCHIVE = 'galaxy-wormhole.tar.gz';
const VERSION = 'galaxy-wormhole.version';

/** Inspect prebuilt artifacts only. Packaging never compiles a Wormhole helper. */
export async function inspectWormholeHelpers(directory) {
  const helpers = [];
  for (const architecture of ARCHITECTURES) {
    const archivePath = path.join(directory, architecture, ARCHIVE);
    const versionPath = path.join(directory, architecture, VERSION);
    try {
      const [archive, marker] = await Promise.all([fs.stat(archivePath), fs.stat(versionPath)]);
      if (!archive.isFile() || archive.size < 18) throw new Error('archive is empty or not a gzip file');
      if (!marker.isFile() || marker.size > 128) throw new Error('version marker is not a bounded text file');
      const version = (await fs.readFile(versionPath, 'utf8')).trim();
      if (!/^[0-9a-f]{64}$/i.test(version)) throw new Error('version marker must contain the helper binary SHA-256');
      const handle = await fs.open(archivePath, 'r');
      try {
        const header = Buffer.alloc(3);
        const { bytesRead } = await handle.read(header, 0, header.length, 0);
        if (bytesRead !== 3 || header[0] !== 0x1f || header[1] !== 0x8b || header[2] !== 8) {
          throw new Error('archive is not gzip-compressed');
        }
      } finally { await handle.close(); }
      const hash = createHash('sha256');
      let archiveBytes = 0;
      // Bound the read to the inspected size. Appending to an override while
      // packaging cannot make validation stream an indefinitely growing file.
      for await (const chunk of createReadStream(archivePath, { end: archive.size - 1 })) {
        archiveBytes += chunk.length;
        hash.update(chunk);
      }
      if (archiveBytes !== archive.size) throw new Error('archive changed while being inspected');
      helpers.push({ architecture, archivePath, versionPath, version, archiveBytes, archiveSha256: hash.digest('hex') });
    } catch (error) {
      throw new Error(`Prebuilt Wormhole helper ${architecture} is missing or invalid at ${directory}: ${error.message}. Restore the committed resources/wormhole-helpers artifacts or set GALAXY_WORMHOLE_HELPERS_DIR to a complete prebuilt helper directory. Refresh helpers separately with script/build_wormhole_helpers when needed; install/deploy will not compile them.`, { cause: error });
    }
  }
  return helpers;
}

export async function copyWormholeHelpers(helpers, directory) {
  await fs.rm(directory, { recursive: true, force: true });
  for (const helper of helpers) {
    const destination = path.join(directory, helper.architecture);
    await fs.mkdir(destination, { recursive: true });
    // Copy files rather than source symlinks: the app must be self-contained.
    await fs.copyFile(helper.archivePath, path.join(destination, ARCHIVE));
    await fs.copyFile(helper.versionPath, path.join(destination, VERSION));
  }
  await verifyWormholeHelpers(helpers, directory);
}

export async function verifyWormholeHelpers(expected, directory) {
  const actual = await inspectWormholeHelpers(directory);
  for (const helper of expected) {
    const bundled = actual.find((entry) => entry.architecture === helper.architecture);
    if (!bundled || bundled.version !== helper.version || bundled.archiveBytes !== helper.archiveBytes || bundled.archiveSha256 !== helper.archiveSha256) {
      throw new Error(`Bundled Wormhole helper ${helper.architecture} does not match its prebuilt source`);
    }
  }
}
