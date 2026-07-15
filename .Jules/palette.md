## 2024-05-15 - Equitable Focus States for Container Components
**Learning:** Container components (like Cards) that contain interactive elements often only provide visual feedback (elevations, shadows) on `:hover`. This leaves keyboard users without the same affordances when tabbing through internal links.
**Action:** Always pair `:hover` with `:focus-within` on container components that hold links, ensuring equitable visual feedback while respecting `@media (prefers-reduced-motion: reduce)` for both states.

## 2024-10-24 - Consistent Focus Visible for Astro/Starlight Components
**Learning:** Custom Astro/Starlight framework components (like `starlight-tabs`) and nested markdown content frequently provide rich `:hover` effects but can neglect explicit `:focus-visible` styling, relying on default browser outlines which may have poor contrast against theme colors.
**Action:** Explicitly map `:focus-visible` styles to match existing `:hover` states on complex framework components and markdown links in documentation sites, ensuring a consistent and accessible keyboard navigation experience.
