## 2024-05-25 - [Add focus outline and smooth scrolling]
**Learning:** Typhon docs site using Starlight didn't have an explicit focus ring for interactive elements like links and buttons when using keyboard navigation, decreasing accessibility.
**Action:** Added a 2px solid accent colored focus ring using `:focus-visible` to `custom.css` to enhance keyboard accessibility. Also added `scroll-behavior: smooth` inside a `@media (prefers-reduced-motion: no-preference)` query for anchor links.
## 2024-05-29 - Missing Focus Visible on Starlight Cards & Animation Respect
**Learning:** Found that custom Starlight `LinkCard` and `Card` components in this Astro setup had `:hover` effects (`transform: translateY(-2px)`) but entirely missed explicit `:focus-visible` or `:focus-within` styles. Furthermore, hover animations weren't wrapped in `@media (prefers-reduced-motion: reduce)`.
**Action:** Applied global focus-visible and focus-within styles in `custom.css` to cover standard links, buttons, and custom cards (`.sl-link-card`). Wrapped card hover transforms in reduced-motion queries to respect user OS preferences.
