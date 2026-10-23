/** The board rectangle is one decision shared by the layout and the node. */
export const NODE_W = 232;
export const NODE_H = 116;

/** Handles sit halfway down each board; keeping this here avoids two offsets. */
export const HANDLE_Y = NODE_H / 2;

export interface GraphPosition {
  x: number;
  y: number;
}
