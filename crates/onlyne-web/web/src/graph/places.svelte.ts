import { load, save } from '../lib/persist';
import type { GraphPosition } from './geometry';

const PLACES_KEY = 'onlyne.graph.places';
const remembered = load<Record<string, GraphPosition>>(PLACES_KEY, {});

class PlacesStore {
  positions = $state.raw<Record<string, GraphPosition>>(remembered);

  place(role: string, position: GraphPosition): void {
    this.positions = {
      ...this.positions,
      [role]: { x: position.x, y: position.y },
    };
    save(PLACES_KEY, this.positions);
  }

  forget(role: string): void {
    if (!(role in this.positions)) return;
    const next = { ...this.positions };
    delete next[role];
    this.positions = next;
    save(PLACES_KEY, this.positions);
  }

  clear(): void {
    if (Object.keys(this.positions).length === 0) return;
    this.positions = {};
    save(PLACES_KEY, this.positions);
  }

  missingOf(roles: readonly string[]): string[] {
    return roles.filter((role) => !(role in this.positions));
  }
}

export const places = new PlacesStore();
