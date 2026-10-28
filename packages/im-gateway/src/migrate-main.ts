#!/usr/bin/env bun
import { OnlyneError, type NodeId } from '@onlyne/lib';
import { applyMigration, inspectStore, planMigration, rollbackMigration } from './migrate.js';
import { t } from './gateway-text.js';

/**
 * Offline owner migration CLI. It never discovers a store: the operator names the file, the old
 * identity, the new identity, the channel platform and an explicit pairing of the new identity.
 * `apply` also needs `--yes`; `plan` prints the same report without writing anything.
 */

function argument(argv: string[], name: string): string | undefined {
  const index = argv.indexOf(`--${name}`);
  return index < 0 ? undefined : argv[index + 1];
}

function required(argv: string[], name: string): string {
  const value = argument(argv, name);
  if (value === undefined || value.length === 0) throw new OnlyneError('invalid_request', t('migrate.useApply'));
  return value;
}

const argv = process.argv.slice(2);
const [command, store] = argv;
if (command === undefined || store === undefined) {
  console.error(t('migrate.usage'));
  process.exit(2);
}

const print = (value: unknown): void => { console.log(JSON.stringify(value, null, 2)); };

try {
  if (command === 'inspect') {
    print(await inspectStore(store));
  } else if (command === 'plan' || command === 'apply') {
    const input = {
      store_path: store,
      from: required(argv, 'from') as NodeId,
      to: required(argv, 'to') as NodeId,
      platform: required(argv, 'platform'),
      pairing: { node_id: required(argv, 'pair') as NodeId },
      ...(argument(argv, 'snapshot') === undefined ? {} : { snapshot_path: argument(argv, 'snapshot')! }),
      ...(argument(argv, 'report') === undefined ? {} : { report_path: argument(argv, 'report')! }),
    };
    if (command === 'plan') {
      print((await planMigration(input)).report);
    } else {
      if (!argv.includes('--yes')) {
        console.error('apply rewrites the store. Re-run with --yes once `plan` shows the expected resources.');
        process.exit(2);
      }
      print(await applyMigration(input));
    }
  } else if (command === 'rollback') {
    const snapshot = required(argv, 'snapshot');
    print(await rollbackMigration({ store_path: store, snapshot_path: snapshot }));
  } else {
    console.error(t('migrate.usage'));
    process.exit(2);
  }
} catch (error) {
  console.error(error instanceof OnlyneError ? error.message : String(error));
  process.exit(1);
}
