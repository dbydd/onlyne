import { OnlyneError } from '@onlyne/lib';

/**
 * A parsed five-field cron expression. Every field becomes a sorted list of the values it
 * admits, so matching is a membership test and never a re-parse. Time is UTC: a mounted task
 * must fire at the same instant after a restart, in every process, on every host.
 */
export interface CronSchedule {
  readonly minutes: readonly number[];
  readonly hours: readonly number[];
  readonly days: readonly number[];
  readonly months: readonly number[];
  readonly weekdays: readonly number[];
  /** Which of the day fields the expression restricts; both restricted match either (Vixie). */
  readonly domRestricted: boolean;
  readonly dowRestricted: boolean;
}

const FIELDS = [
  { name: 'minute', min: 0, max: 59 },
  { name: 'hour', min: 0, max: 23 },
  { name: 'day-of-month', min: 1, max: 31 },
  { name: 'month', min: 1, max: 12 },
  // 7 is Sunday: accepted as an input alias only, and normalized before the schedule is stored.
  { name: 'day-of-week', min: 0, max: 7 },
] as const;

/** `7` is Sunday in the day-of-week field; the schedule normalizes it to `0`. */
const WEEKDAY_ALIAS: Record<string, number> = { '7': 0 };

const RANGE = /^(\d+)-(\d+)$/;
const NUMBER = /^\d+$/;

function parseField(text: string, spec: (typeof FIELDS)[number]): number[] {
  if (text.length === 0) throw cronError(`The ${spec.name} field is empty.`);
  const values = new Set<number>();
  for (const part of text.split(',')) {
    if (part.length === 0) throw cronError(`The ${spec.name} field holds an empty list item.`);
    const [rangeText = '', stepText, extra] = part.split('/');
    if (extra !== undefined) throw cronError(`The ${spec.name} field step has more than one slash.`);
    const step = stepText === undefined ? 1 : Number(stepText);
    if (!Number.isInteger(step) || step <= 0) throw cronError(`The ${spec.name} field step is a positive integer.`);
    let from: number;
    let to: number;
    if (rangeText === '*') {
      from = spec.min;
      to = spec.max;
    } else if (NUMBER.test(rangeText)) {
      from = Number(rangeText);
      // A bare number with a step names the range from that number to the field maximum.
      to = stepText === undefined ? from : spec.max;
    } else {
      const range = RANGE.exec(rangeText);
      if (range === null) throw cronError(`The ${spec.name} field value ${JSON.stringify(rangeText)} is not a number, range or '*'.`);
      from = Number(range[1]);
      to = Number(range[2]);
    }
    const low = from;
    const high = to;
    if (low < spec.min || low > spec.max || high < spec.min || high > spec.max) {
      throw cronError(`The ${spec.name} field allows ${spec.min}..${spec.max}, not ${from}..${to}.`);
    }
    if (low > high) throw cronError(`The ${spec.name} field range ${from}-${to} runs backwards.`);
    for (let value = low; value <= high; value += step) values.add(WEEKDAY_ALIAS[String(value)] ?? value);
  }
  return [...values].sort((a, b) => a - b);
}

function cronError(message: string): OnlyneError {
  return new OnlyneError('invalid_request', message);
}

/** Parses one five-field expression; throwing `invalid_request` names the offending field. */
export function parseCron(expression: string): CronSchedule {
  if (typeof expression !== 'string') throw cronError('A cron expression is a string.');
  const parts = expression.trim().split(/\s+/);
  if (parts.length !== FIELDS.length) {
    throw cronError(`A cron expression has ${FIELDS.length} fields (minute hour day month weekday), not ${parts.length}.`);
  }
  const [minutes, hours, days, months, weekdays] = parts.map((part, index) => parseField(part, FIELDS[index]!));
  return {
    minutes: minutes!, hours: hours!, days: days!, months: months!, weekdays: weekdays!,
    domRestricted: parts[2] !== '*',
    dowRestricted: parts[4] !== '*',
  };
}

const MINUTE_MS = 60_000;
const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
/** Five years covers every Gregorian recurrence, including 29 February on a weekday. */
const HORIZON_DAYS = 366 * 5;

/**
 * The first instant strictly after `fromMs` that the expression admits, to the minute, in UTC.
 * `undefined` means no occurrence inside the five-year horizon (for example `0 0 31 2 *`).
 */
export function nextCronTime(schedule: CronSchedule, fromMs: number): number | undefined {
  const dayStart = Math.floor(fromMs / DAY_MS) * DAY_MS;
  for (let day = 0; day < HORIZON_DAYS; day++) {
    const base = dayStart + day * DAY_MS;
    const date = new Date(base);
    const month = date.getUTCMonth() + 1;
    if (!schedule.months.includes(month)) continue;
    const dom = date.getUTCDate();
    const dow = date.getUTCDay();
    const domMatch = schedule.days.includes(dom);
    const dowMatch = schedule.weekdays.includes(dow);
    // Vixie cron: a restricted day-of-month and a restricted day-of-week combine with OR.
    const dayMatch = schedule.domRestricted && schedule.dowRestricted
      ? domMatch || dowMatch
      : domMatch && dowMatch;
    if (!dayMatch) continue;
    for (const hour of schedule.hours) {
      for (const minute of schedule.minutes) {
        const at = base + hour * HOUR_MS + minute * MINUTE_MS;
        if (at > fromMs) return at;
      }
    }
  }
  return undefined;
}
