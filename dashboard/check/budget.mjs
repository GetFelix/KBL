// The performance budget, in one place so docs, the node test and the browser check agree.
export const ITEMS = 10_000;
export const FOLD_BUDGET_MS = { node: 500, browser: 1500 };
export const PAINT_BUDGET_MS = { firstUseful: 5000, fullLoad: 15000 };
