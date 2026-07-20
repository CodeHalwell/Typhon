## 2024-05-15 - Equitable Focus States for Container Components
**Learning:** Container components (like Cards) that contain interactive elements often only provide visual feedback (elevations, shadows) on `:hover`. This leaves keyboard users without the same affordances when tabbing through internal links.
**Action:** Always pair `:hover` with `:focus-within` on container components that hold links, ensuring equitable visual feedback while respecting `@media (prefers-reduced-motion: reduce)` for both states.

## 2024-05-16 - Equitable Focus States and Reduced Motion
**Learning:** We need to pair existing `:hover` states with `:focus-visible` to ensure equitable visual feedback for keyboard users on interactive elements (like markdown links and starlight tabs). Additionally, when respecting `@media (prefers-reduced-motion: reduce)`, we must explicitly disable all associated CSS transitions using `transition: none;` on the affected elements.
**Action:** Pair `:hover` with `:focus-visible` on interactive elements, and explicitly disable transitions under `@media (prefers-reduced-motion: reduce)` rules for any components with CSS transitions.
