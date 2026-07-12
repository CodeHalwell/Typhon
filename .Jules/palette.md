## 2024-05-15 - Equitable Focus States for Container Components
**Learning:** Container components (like Cards) that contain interactive elements often only provide visual feedback (elevations, shadows) on `:hover`. This leaves keyboard users without the same affordances when tabbing through internal links.
**Action:** Always pair `:hover` with `:focus-within` on container components that hold links, ensuring equitable visual feedback while respecting `@media (prefers-reduced-motion: reduce)` for both states.
## 2024-07-12 - Ensure Equitable Keyboard Focus for Links and Tabs
**Learning:** Container components and simple interaction elements like `.tab` or markdown links in Starlight often receive distinct `:hover` styles, but sometimes `:focus-visible` styles are forgotten, rendering keyboard navigation hard to track. Also, remember to disable these dynamic visual feedback transitions if the user prefers reduced motion (`@media (prefers-reduced-motion: reduce)`).
**Action:** Always map the `:hover` style visual changes to `:focus-visible` elements for equitable experience. Explicitly disable CSS transitions for these affected elements within the reduced-motion media query.
