# Interface design

The interface is a focused desktop utility, not a configuration dashboard.
Its default screen answers three questions in order:

1. Which model and connection are active?
2. Which saved connection should be used next?
3. How can a new connection be added?

Provider internals, proxy lifecycle details, recovery warnings, and the direct
configuration fallback live on a separate Advanced Settings page. This keeps
the normal switching flow short without hiding operational controls.

## Information architecture

- **Model Switching** is the default page. It contains the current state, the
  official Codex account, saved API connections, and the one primary
  Add Connection action.
- **Advanced Settings** contains switching mode selection, local-proxy state,
  fallback direct configuration, and recovery actions.
- **Add or Edit Connection** is a modal sheet with two explicit steps:
  connection details and model selection, followed by the save or
  save-and-switch action.
- Restart notices use a dedicated confirmation dialog only when the selected
  transition cannot take effect on the next turn.

The connection editor keeps a manual model-ID path next to discovery rather
than showing it as an advanced global preference. It is a fallback for the
same user goal, not a different product mode.

## Visual system

- Use the native Apple system-font stack and compact 13–14 px desktop type.
- Use semantic surface, separator, label, accent, success, warning, and danger
  tokens. Every token has a dark appearance counterpart.
- Reserve blue for the selected navigation item, current focus, and the single
  primary action in a region. Destructive actions remain text-first until
  confirmation.
- Use a translucent sidebar, solid grouped surfaces, quiet separators, and
  restrained shadows. Material effects support hierarchy and do not decorate
  every container.
- Keep controls at least 36 px high in the normal layout and never below the
  WCAG 2.2 minimum target size.

## Interaction rules

- The app opens on Model Switching and loads the current state automatically.
- A saved connection exposes one model picker and one primary Use This Model
  action. Edit and Remove are lower-emphasis actions.
- The official-account row is visually equal to a saved connection but clearly
  labels that login remains managed by Codex.
- Add Connection opens with keyboard focus in the first field. Escape closes
  the sheet, and focus returns to the button that opened it.
- The editor reveals one task at a time. Model selection stays unavailable
  until a connection has been checked, and saving stays unavailable until the
  required models are selected.
- Status changes use a persistent, polite live region. Destructive, restart,
  and manual-recovery states require explicit confirmation.
- The layout collapses to a top navigation bar below 680 px; cards and action
  groups become single-column without changing their reading or tab order.
- Motion is brief and functional. `prefers-reduced-motion` removes nonessential
  animation.

## Accessibility

- Every icon-only control has an accessible name.
- The active navigation page uses `aria-current`; page visibility and expanded
  state stay synchronized.
- Dialogs use native modal behavior, labelled headings, logical tab order, and
  visible two-pixel focus treatment.
- Informational color is always paired with text or an icon; color alone never
  communicates current, warning, or failure state.
- Text and controls use semantic dark-mode colors instead of automatic
  inversion.

## Research basis

The design translates platform conventions rather than copying a specific
application:

- Apple Human Interface Guidelines:
  [Layout](https://developer.apple.com/design/human-interface-guidelines/layout),
  [Typography](https://developer.apple.com/design/human-interface-guidelines/typography),
  [Sidebars](https://developer.apple.com/design/human-interface-guidelines/sidebars),
  [Buttons](https://developer.apple.com/design/human-interface-guidelines/buttons),
  [Color](https://developer.apple.com/design/human-interface-guidelines/color),
  [Search fields](https://developer.apple.com/design/human-interface-guidelines/search-fields),
  and
  [Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility).
- W3C WCAG 2.2 guidance for
  [minimum target size](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html)
  and
  [focus appearance](https://www.w3.org/WAI/WCAG22/Understanding/focus-appearance).
- The community pattern
  [Progressive Disclosure](https://ui-patterns.com/patterns/ProgressiveDisclosure)
  informs the split between the normal switching task and advanced
  operational settings.
- [Raycast Settings](https://manual.raycast.com/settings) and
  [Windows app settings guidance](https://learn.microsoft.com/en-us/windows/apps/design/app-settings/guidelines-for-app-settings)
  provide contemporary desktop references for keyboard-first settings,
  predictable grouping, and persistent navigation.
