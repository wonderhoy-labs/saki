# Design

Wonderhoy Labs apps are Material 3 throughout, in the colours of the character each one is named for. Someone who
has used one should feel at home in the next, on a phone or in a browser.

## Colour

- The seed is the character's image colour, in `package.json` under `wonderhoy.color`.
- `pnpm theme` runs Material's colour utilities with the **content** scheme. In the light theme it keeps the
  image colour itself as the primary container, and picks a neighbouring hue for tertiary. In the dark theme the
  primary and tertiary roles use Material's usual tones (80, 20, 30, 90) in the same hues, so a pale image colour
  doesn't glare or swallow the buttons on it. It writes:
  - `web/src/ui/theme.css`: every mdui colour variable, light and dark, plus `--wonderhoy-brand`
  - `web/src/brand.ts`: the name, tagline, character, unit, colour and repository
  - `android/app/src/main/kotlin/com/wonderhoylabs/<id>/ui/Theme.kt`: the `Brand` object, both colour schemes and
    `<Name>Theme`
  - `android/app/src/main/res/values{,-night}/theme_colors.xml`: the window background before Compose draws, and
    the brand colour for XML that needs it
- Use the scheme's roles, never literal colours: `primary` for the one most important action, `primaryContainer`
  for the card that shows the current state, `surfaceContainer*` for the rest, `error*` only for errors. A success
  or "connected" state uses `primaryContainer` or `tertiaryContainer`, not a new green.
- No dynamic (wallpaper) colour on Android. The brand has to show.

## Frame

Every app has the same frame around its own content.

|                           | Web                                                                                     | Android                                                                        |
| ------------------------- | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Top                       | `AppBar` from `web/src/ui/Brand.tsx`: logo, name, the page's own actions, a GitHub link | `BrandHeader` from `ui/Brand.kt`: logo, name in `primary`, a one-line subtitle |
| Background                | `.app` from `wonderhoy.css`: a faint wash of the primary and tertiary containers        | `MaterialTheme.colorScheme.background`                                         |
| Bottom of the main screen | `Footer`: "A Wonderhoy Labs project, named for … of …" with a dot in the brand colour   | `BrandFooter`, the same line                                                   |
| Edges                     | `--page-gutter`: 24px, 32px from 840px wide                                             | `safeDrawingPadding()` and 24dp sides                                          |

A screen that fills the whole window on purpose, like a remote desktop or a full-screen log, can leave the frame
out, as long as getting back to the main screen shows it again.

## Components

- **Web**: mdui only. Buttons are `mdui-button` (`filled` for the main action, `tonal` for secondary ones,
  `text` for links), cards are `mdui-card` (`filled` or `elevated`), inputs are `mdui-text-field`, progress is
  `mdui-linear-progress` or `mdui-circular-progress`, choices are `mdui-chip`, confirmations are `mdui-dialog`.
- **Android**: Material 3 Compose. `Button` for the main action, `FilledTonalButton` or `OutlinedButton` for
  others, `Card` with container colours for grouped content, `SegmentedButton` for a choice between modes,
  `AlertDialog` for confirmations.
- Icons come from Material Symbols only: `@mdui/icons` on the web, Compose's Material icons on Android.
- Corners: 12px (web) and 12dp (Android) for cards, 16 for the logo, full for buttons and chips, as Material sets
  them.

## Type

- Web: Roboto Flex, 14px body, Material's sizes for headings (`45px/52px` display, `32px/40px` headline).
- Android: `MaterialTheme.typography` as it comes, with `headlineMedium` and `SemiBold` for the app name.
- Sentence case everywhere: "Share my network", not "Share My Network".

## Words

Short, plain, second person. Say what will happen ("Your phone's traffic goes through this computer"), not how
the code does it. Errors say what went wrong and what to do next.

## Artwork

The logo, launcher icon and favicon are the maintainer's own art, kept in `docs/logo.webp` and copied where each
platform needs it. Agents never make or edit artwork. If a project has none yet, the frame shows the name alone.
