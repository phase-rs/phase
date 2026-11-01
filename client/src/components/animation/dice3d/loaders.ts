// Dynamic-import seams for the code-split WebGL renderers. Kept in one module so
// the overlay's lazy wiring and its prefetch share a single import site per chunk.
export const loadDice3DModule = () => import("./Dice3D.tsx");
export const loadCoin3DModule = () => import("./Coin3D.tsx");
