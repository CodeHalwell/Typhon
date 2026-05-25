## 2024-05-25 - [Add focus outline and smooth scrolling]
**Learning:** Typhon docs site using Starlight didn't have an explicit focus ring for interactive elements like links and buttons when using keyboard navigation, decreasing accessibility.
**Action:** Added a 2px solid accent colored focus ring using `:focus-visible` to `custom.css` to enhance keyboard accessibility. Also added `scroll-behavior: smooth` inside a `@media (prefers-reduced-motion: no-preference)` query for anchor links.
