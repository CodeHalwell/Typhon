## 2025-07-04 - Equitable visual feedback for container components

**Learning:** Container components (like `<Card>` and `<LinkCard>`) that feature rich interactive hover states (e.g. box-shadows, borders, elevations) often lack corresponding focus feedback for keyboard users when internal links are tabbed through. This creates an inequitable interaction experience.
**Action:** When adding hover states that modify container dimensions or aesthetics, ensure there are corresponding `:focus-within` styles so keyboard navigation triggers the same visual feedback. Additionally, these transitions must be properly accounted for in `@media (prefers-reduced-motion: reduce)` queries.
