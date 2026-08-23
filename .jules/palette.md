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
## 2026-06-03 - [Ensure markdown links do not rely purely on color]
**Learning:** Discovered that inline links within `.sl-markdown-content` paragraphs only relied on a different text color (`var(--sl-color-text-accent)`) to differentiate themselves from regular text, which violates WCAG 1.4.1 (Use of Color). This makes it harder for users with color vision deficiencies to spot interactive links.
**Action:** Added a subtle `text-decoration: underline` to markdown links in `custom.css` with a semi-transparent `text-decoration-color` that turns solid on hover. This ensures links are visually distinct through multiple channels (color + underline) while maintaining a clean aesthetic.
## 2024-06-05 - [Improve inline code visual breaking and mobile layout]
**Learning:** Found that long inline code snippets (`:not(pre) > code`) could either break mobile responsive layouts horizontally if they didn't wrap, or look visually broken when they did wrap, because the border radius and padding wouldn't apply to the line breaks.
**Action:** Added `overflow-wrap: break-word` to ensure long strings (like paths or URLs in code tags) break to prevent horizontal scrolling on mobile. Added `box-decoration-break: clone` (and `-webkit-` prefix) so that padding and border radius are cleanly applied to both the end of the first line and the start of the next line when inline code wraps.
## 2026-06-06 - [Link hover transitions and abbreviation semantics]
**Learning:** Adding a transition to `text-decoration-color` on markdown links makes the hover interaction feel significantly smoother and more deliberate than an abrupt color snap. Also, standard `<abbr>` elements lacked a visual indicator that they can be hovered for a title expansion, hiding valuable context.
**Action:** Added `transition: text-decoration-color 150ms ease;` to markdown links. Added `text-decoration: underline dotted;` and `cursor: help;` to `abbr[title]` to clearly signal interactivity.
## 2026-06-06 - [Themed text selection for visual polish]
**Learning:** Default browser text selection colors (usually stark blue) often clash with custom themes, making the site feel less cohesive.
**Action:** Implemented `::selection` to match the site's `var(--sl-color-accent)` at 25% opacity, preserving contrast while harmonizing the selection experience with the overall design.
## 2024-06-13 - [Focus indicators on scrollable code blocks]
**Learning:** Discovered that scrollable regions in Astro/Starlight documentation, such as code blocks, utilize `tabindex="0"` to allow keyboard users to scroll through the content. However, these elements lacked an explicit focus indicator, making it confusing for keyboard users to know when they had focused on them.
**Action:** Added `[tabindex="0"]:focus-visible` to the existing focus ring declarations in `docs-site/src/styles/custom.css` to ensure keyboard navigation accessibility for these regions.
## 2026-06-09 - [Target highlighting for spatial orientation]
**Learning:** Found that jumping to an anchor link (like clicking a Table of Contents entry) abruptly changes the scroll position without indicating *which* heading was targeted. This loss of spatial orientation makes it harder for users to immediately find where they are supposed to start reading, especially when multiple headings look similar or the target heading is near the bottom of the page.
**Action:** Added a `:target` CSS animation that briefly flashes the background and outline of the targeted heading using a faded accent color. Also added a `@media (prefers-reduced-motion: reduce)` fallback that uses a static colored left border instead of a fading animation.
## 2026-06-16 - [Visually elevate markdown blockquotes for better readability]
**Learning:** Found that default Starlight markdown blockquotes (`>`) only utilize a thin 1px border and no background color, causing them to blend into the main text body and losing their impact as distinct callouts or key takeaways.
**Action:** Added targeted CSS to `custom.css` for `.sl-markdown-content blockquote` to introduce a subtle accent background color, a thicker 4px left border, and rounded right corners. This significantly elevates blockquotes in the visual hierarchy, improving content scannability.
## 2026-06-18 - [Add hover state to unselected tabs and focus-within to tables]
**Learning:** Discovered that unselected Starlight tabs (`starlight-tabs`) lacked any hover feedback, leaving users without visual confirmation that the tabs are interactive before clicking. Additionally, data tables containing links did not highlight the entire row when the links were focused via keyboard, breaking the visual connection for keyboard users that mouse users get from `:hover`.
**Action:** Added a `transition: color 150ms ease, border-color 150ms ease;` and a `:hover` state modifying the text and border colors for unselected tabs (`[role="tab"]:not([aria-selected="true"])`) in `custom.css`. Also added a `:focus-within` selector mirroring the `:hover` style for `table tbody tr` to improve keyboard navigation inside tables.
## 2026-06-23 - [Focus indicators on expressive code blocks]
**Learning:** Found that Starlight 'Expressive Code' blocks specifically use the `.expressive-code pre` selector for the scrollable container. While `[tabindex="0"]` focus styling covered some generic scrollable elements, the expressive code blocks specifically required  to ensure a visible focus outline when navigating via keyboard, confirming that custom components sometimes bypass generic accessibility selectors.
**Action:** Appended `.expressive-code pre:focus-visible` to the global focus ring CSS rules in `custom.css`.
## 2026-06-23 - [Focus indicators on expressive code blocks]
**Learning:** Found that Starlight 'Expressive Code' blocks specifically use the \`.expressive-code pre\` selector for the scrollable container. While \`[tabindex="0"]\` focus styling covered some generic scrollable elements, the expressive code blocks specifically required \`.expressive-code pre:focus-visible\` to ensure a visible focus outline when navigating via keyboard, confirming that custom components sometimes bypass generic accessibility selectors.
**Action:** Appended \`.expressive-code pre:focus-visible\` to the global focus ring CSS rules in \`custom.css\`.
## 2024-06-26 - Markdown Abbreviations and Code Blocks
**Learning:** When using `<abbr>` tags for accessibility tooltips in `.mdx` content files, standard regex replacements will erroneously apply the HTML tags inside code blocks (```). Markdown engines render HTML in code blocks literally, breaking the presentation (e.g., ASCII art).
**Action:** When applying string or regex replacements in Markdown files to add HTML semantics, use a parsing script or stateful replacement to explicitly skip lines inside code blocks.
## 2026-06-25 - [Modern thin scrollbars for technical documentation]
**Learning:** Default OS scrollbars (especially on Windows/Linux) are bulky and visually clash with custom, modern documentation themes. They are particularly detrimental when dealing with horizontally scrollable technical content, like `.expressive-code pre` blocks, as thick default scrollbars can overlap or reduce the visibility of code content.
**Action:** Added global `scrollbar-width: thin` and `::-webkit-scrollbar` styling in `custom.css` with a matching theme color. Included a specific fix for `.expressive-code pre::-webkit-scrollbar-corner` to prevent unsightly corner boxes on code blocks with both horizontal and vertical overflow.

## 2024-05-15 - Equitable Focus States for Container Components
**Learning:** Container components (like Cards) that contain interactive elements often only provide visual feedback (elevations, shadows) on `:hover`. This leaves keyboard users without the same affordances when tabbing through internal links.
**Action:** Always pair `:hover` with `:focus-within` on container components that hold links, ensuring equitable visual feedback while respecting `@media (prefers-reduced-motion: reduce)` for both states.

## 2024-05-16 - Equitable Focus States and Reduced Motion
**Learning:** We need to pair existing `:hover` states with `:focus-visible` to ensure equitable visual feedback for keyboard users on interactive elements (like markdown links and starlight tabs). Additionally, when respecting `@media (prefers-reduced-motion: reduce)`, we must explicitly disable all associated CSS transitions using `transition: none;` on the affected elements.
**Action:** Pair `:hover` with `:focus-visible` on interactive elements, and explicitly disable transitions under `@media (prefers-reduced-motion: reduce)` rules for any components with CSS transitions.
## 2024-05-30 - Smooth transitions for custom components
**Learning:** Found that custom Starlight components like LinkCard lacked base transition properties, causing abrupt animation snaps on hover.
**Action:** Added missing transition properties to custom components and their internal elements to ensure smooth UX during hover states.
## 2026-06-27 - [Disable transitions alongside animations for reduced motion]
**Learning:** In CSS `@media (prefers-reduced-motion: reduce)` blocks, explicitly declaring `animation: none` on elements like `:target` does not inherently disable CSS transitions that might be applied to the same element.
**Action:** Always declare `transition: none` alongside `animation: none` when respecting reduced motion preferences to fully disable all unintended animated states.
## 2024-08-23 - [Remove tabindex="0" from abbr tags]
**Learning:** Adding `tabindex="0"` to non-interactive `<abbr>` elements makes them focusable for screen readers but fails WCAG 1.4.13 (Content on Hover or Focus) for sighted keyboard users. Standard browser tooltips (`title` attribute) do not display on keyboard focus, making this pattern inaccessible.
**Action:** Removed `tabindex="0"` from `<abbr>` tags. If tooltips are needed for keyboard users, a custom tooltip component must be used instead of relying on the native `title` attribute on `<abbr>`.
