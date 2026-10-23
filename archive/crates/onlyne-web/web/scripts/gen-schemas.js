// Runs the Rust generator that renders onlyne-proto's schemars schemas into
// web/schema/ — the repo's own generator pattern (onlyne-proto's gen-schema),
// because the web's request and response types are generated from the proto's
// schema, never hand-written. Requires a built bundle (the crate's build.rs
// gates the whole crate), so this is a maintainer step, not a build step.
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const crate = join(here, '..');
const result = spawnSync('cargo', ['run', '--bin', 'gen-schema'], {
  cwd: crate,
  stdio: 'inherit',
});
if (result.status !== 0) {
  console.error('gen-schema failed: the crate needs its built bundle first (npm run build)');
  process.exit(result.status ?? 1);
}
