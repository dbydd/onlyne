import type { ComputeBrokerService } from './service.ts';
import type { LeaseRecord } from '@onlyne/compute-core';

export class ComputeTuiRenderer {
  constructor(private readonly service: ComputeBrokerService) {}

  async formatStatusLines(): Promise<string[]> {
    const all = await this.service.ledger.all();
    const lines: string[] = [];

    const running = all.filter(r => r.state === 'running');
    const stopped = all.filter(r => r.state === 'stopped');
    const destroyed = all.filter(r => r.state === 'destroyed');

    lines.push(
      `[Compute Broker] Active: ${running.length} running, ${stopped.length} stopped, ${destroyed.length} destroyed`
    );

    if (all.length === 0) {
      lines.push('No active compute sandboxes.');
      return lines;
    }

    lines.push(
      `${'NAME'.padEnd(20)} ${'STATE'.padEnd(12)} ${'PROVIDER'.padEnd(14)} ${'EXPIRES / RETENTION'.padEnd(20)} ENDPOINTS`
    );

    const now = this.service.now();
    for (const r of all.slice(0, 8)) {
      const remainingSec = Math.max(0, Math.round((r.expires_at - now) / 1000));
      const retentionSec = Math.max(0, Math.round((r.retention_until - now) / 1000));
      const timing = r.state === 'running' ? `${remainingSec}s left` : `${retentionSec}s keep`;
      
      const endpoints = r.descriptor?.connection?.endpoints
        ? Object.entries(r.descriptor.connection.endpoints).map(([k, v]) => `${k}=${v}`).join(', ')
        : '—';

      lines.push(
        `${r.name.slice(0, 18).padEnd(20)} ${r.state.padEnd(12)} ${r.provider.slice(0, 12).padEnd(14)} ${timing.padEnd(20)} ${endpoints}`
      );
    }

    if (all.length > 8) {
      lines.push(`… and ${all.length - 8} more leases.`);
    }

    return lines;
  }
}
