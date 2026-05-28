---
title: Dynamic Imports for Heavy Components
impact: CRITICAL
impactDescription: directly affects TTI and LCP
tags: bundle, dynamic-import, code-splitting, react-lazy
---

## Dynamic Imports for Heavy Components

Use `React.lazy()` (Vite) or `next/dynamic` (Next) to lazy-load large components not needed on initial render. Phase.rs uses Vite, so prefer `React.lazy()`.

**Incorrect (Monaco bundles with main chunk ~300KB):**

```tsx
import { MonacoEditor } from './monaco-editor'

function CodePanel({ code }: { code: string }) {
  return <MonacoEditor value={code} />
}
```

**Correct (Monaco loads on demand):**

```tsx
import { lazy, Suspense } from 'react'

const MonacoEditor = lazy(() =>
  import('./monaco-editor').then(m => ({ default: m.MonacoEditor }))
)

function CodePanel({ code }: { code: string }) {
  return (
    <Suspense fallback={null}>
      <MonacoEditor value={code} />
    </Suspense>
  )
}
```

Vite's build splits each `import()` call into a separate chunk automatically.
