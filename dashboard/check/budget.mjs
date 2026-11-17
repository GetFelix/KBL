// The performance budget, in one place so docs, the node test and the browser check agree.
export const ITEMS = 10_000;
export const FOLD_BUDGET_MS = { node: 2000, browser: 1000 };
export const PAINT_BUDGET_MS = { firstUseful: 3000, fullLoad: 6000 };
