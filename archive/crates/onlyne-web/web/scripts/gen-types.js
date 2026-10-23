// Converts the schemars JSON schemas the Rust generator wrote into
// TypeScript modules under src/gen/. Run by `npm run build`; the generated
// files carry the banner so nobody edits them by hand.
import { compile, compileFromFile } from 'json-schema-to-typescript';
import { readdir, mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join, basename } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const schemaDir = join(here, '..', 'schema');
const outDir = join(here, '..', 'src', 'gen');

const BANNER = '/* eslint-disable */\n// Generated from onlyne-proto\'s schemars schema (web/schema). Do not edit.\n';

const names = {
  'view.schema.json': 'View',
  'snapshot.schema.json': 'Snapshot',
  'board.schema.json': 'Board',
  'board-card.schema.json': 'BoardCard',
  'web-op.schema.json': 'WebOp',
};

await mkdir(outDir, { recursive: true });
const files = (await readdir(schemaDir)).filter((name) => name.endsWith('.schema.json'));
for (const file of files) {
  const type = names[file] ?? basename(file, '.schema.json').replace(/(^|-)(\w)/g, (_, __, c) => c.toUpperCase());
  const ts = await compileFromFile(join(schemaDir, file), {
    bannerComment: '',
    style: { singleQuote: true, semi: true },
    declareExternallyReferenced: true,
  });
  await writeFile(join(outDir, `${type}.ts`), BANNER + ts + '\n');
  console.log(`wrote src/gen/${type}.ts`);
}
