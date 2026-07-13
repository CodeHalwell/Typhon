## 2024-05-15 - Equitable Focus States for Container Components
**Learning:** Container components (like Cards) that contain interactive elements often only provide visual feedback (elevations, shadows) on `:hover`. This leaves keyboard users without the same affordances when tabbing through internal links.
**Action:** Always pair `:hover` with `:focus-within` on container components that hold links, ensuring equitable visual feedback while respecting `@media (prefers-reduced-motion: reduce)` for both states.

## 2024-10-24 - Equitable Focus and Reduced Motion for Interactive Text Elements
**Learning:** While container components often lack equitable focus states, inline interactive elements (like markdown links and tab navigation headers) frequently have styled `:hover` states (like color changes or underlines) but also miss `:focus-visible` parity. Additionally, smooth CSS `transition` properties on these elements can cause issues for users who prefer reduced motion.
**Action:** Consistently pair `:hover` with `:focus-visible` across all interactive element styling, and explicitly disable `transition` properties within a `@media (prefers-reduced-motion: reduce)` block to ensure a fully accessible and equitable experience.
