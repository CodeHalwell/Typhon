## 2024-05-25 - [Add focus outline and smooth scrolling]
**Learning:** Typhon docs site using Starlight didn't have an explicit focus ring for interactive elements like links and buttons when using keyboard navigation, decreasing accessibility.
**Action:** Added a 2px solid accent colored focus ring using `:focus-visible` to `custom.css` to enhance keyboard accessibility. Also added `scroll-behavior: smooth` inside a `@media (prefers-reduced-motion: no-preference)` query for anchor links.
## 2024-05-29 - Missing Focus Visible on Starlight Cards & Animation Respect
**Learning:** Found that custom Starlight `LinkCard` and `Card` components in this Astro setup had `:hover` effects (`transform: translateY(-2px)`) but entirely missed explicit `:focus-visible` or `:focus-within` styles. Furthermore, hover animations weren't wrapped in `@media (prefers-reduced-motion: reduce)`.
**Action:** Applied global focus-visible and focus-within styles in `custom.css` to cover standard links, buttons, and custom cards (`.sl-link-card`). Wrapped card hover transforms in reduced-motion queries to respect user OS preferences.
## 2026-05-30 - Improve Data Table Legibility and Form Element Keyboard Navigation
**Learning:** Found that long markdown data tables across the docs site were missing visual guidance (like row hover effects), making it difficult for the eye to track horizontal data accurately. Also discovered input/textarea elements missed the custom focus ring applied to other interactive elements.
**Action:** Added a subtle background color transition on `table tbody tr:hover` to improve reading UX for wide technical tables, and included `input` and `textarea` in the explicit `:focus-visible` ring rule to complete keyboard accessibility for form fields.
## 2026-06-01 - [Improve keyboard shortcut `<kbd>` styling]
**Learning:** Found that inline keyboard shortcuts (`<kbd>` elements) inside Starlight markdown content blended too much with regular text and lacked the standard "key" appearance, making instructions like `Cmd + C` harder to read quickly.
**Action:** Added targeted CSS to `custom.css` for `.sl-markdown-content kbd` to provide a subtle background, border, border-bottom, and shadow. This gives the keys a 3D, pressable appearance, making keyboard shortcuts stand out intuitively.
## 2026-06-02 - [High Contrast Mode Support for Boundaries and Gradients]
**Learning:** Found that elements relying purely on background colors (like tags and inline code) lose their boundaries in Windows High Contrast Mode (Forced Colors Mode), making them indistinguishable from plain text. Also discovered that gradient text using `-webkit-text-fill-color: transparent` becomes completely invisible when the OS strips the background gradient.
**Action:** Added `border: 1px solid transparent` to `.tag` and `:not(pre) > code` so the OS can render a border color in High Contrast Mode. Used `@media (forced-colors: active)` to reset `-webkit-text-fill-color: currentcolor` on `.hero h1` to ensure hero text remains visible.
## 2026-06-02 - [Card hover state interaction enhancement]
**Learning:** Adding a subtle, contextual drop shadow on hover to cards makes the UI feel more tactile, responding better to user intent.
**Action:** Added `box-shadow: 0 4px 12px color-mix(in srgb, var(--sl-color-accent) 20%, transparent);` to `.card:hover` to complement the existing `transform: translateY(-2px)` animation.
