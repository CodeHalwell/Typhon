
## 2024-07-05 - Ensure Container Components Support Focus Equity
**Learning:** Container components (like `<Card>` and `<LinkCard>`) often define rich `:hover` effects (elevations, box-shadows, and border colors) but neglect to apply them when focused via keyboard navigation, leading to inequitable visual feedback for keyboard users.
**Action:** When inspecting or styling interactive containers, always ensure they define `:focus-within` styles that correspond to their `:hover` effects, and ensure these transitions also respect `@media (prefers-reduced-motion: reduce)` rules.
