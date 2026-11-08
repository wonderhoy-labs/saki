# README

Every README follows one layout, so a reader always knows where to look. `pnpm unify` checks the opening, the
badges, the section order and the credits.

## Opening

```html
<p align="center">
  <img src="docs/logo.webp" alt="" width="180">
</p>

<h1 align="center"><Name></h1>

<p align="center"><tagline from package.json></p>

<p align="center">
  <a href="https://github.com/wonderhoy-labs/<id>/actions/workflows/ci.yml"><img src="https://github.com/wonderhoy-labs/<id>/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache 2.0"></a>
  <a href="#license-and-credits"><img src="https://img.shields.io/badge/<Character>-<Unit>-<colour>" alt="Named for <Character> of <Unit>"></a>
</p>
```

In the badge, `-` and `_` are doubled and the rest is URL-encoded: `Tenma%20Saki-Leo%2Fneed-FFDD44`. A web app
adds a fourth badge linking to its site. After the badges: two or three paragraphs on what it does and who it's
for, one screenshot of the real app, and an early-software note that links to "What has been tested".

## Sections

In this order. Sections marked optional can be left out; a project can add its own between them.

| Section                     | What goes in it                                                                                  |
| --------------------------- | ------------------------------------------------------------------------------------------------ |
| What you need               | Devices, OS versions, accounts, cables                                                           |
| Install _(optional)_        | Release downloads first, then building from source                                               |
| Use it                      | The common tasks, each as a short `###` walkthrough                                              |
| Use it in your app          | The library: how to add it, a short working example, and its main entry points                   |
| What has been tested        | Exactly what ran, on which hardware and OS versions, and what never did. Speed numbers here too. |
| How it works                | The design in plain words, with `###` parts                                                      |
| Security and privacy        | What it can reach, what leaves the device, and what an attacker on each link could do            |
| Options _(optional)_        | Every command-line flag or setting                                                               |
| Troubleshooting             | Symptom, then fix                                                                                |
| Known issues                | What's broken or missing right now                                                               |
| Development                 | Layout table, how to build, test and format, how to release                                      |
| Under the hood _(optional)_ | Deeper notes: protocols, measurements, platform workarounds                                      |
| License and credits         | Always last                                                                                      |

## License and credits

```markdown
## License and credits

<Name> is a [Wonderhoy Labs](https://github.com/wonderhoy-labs) project, released under the
[Apache License 2.0](LICENSE). <Any trademark notes: "It isn't affiliated with Google. Android and Pixel are
trademarks of Google LLC.">

It stands on the work of these projects:
- [Project](link) (license), for what

<Name> is named for <Character> of <Unit>, from Project SEKAI COLORFUL STAGE! feat. Hatsune Miku. Wonderhoy Labs
is a fan project, not affiliated with SEGA, Colorful Palette or Crypton Future Media, who own Project SEKAI and
its characters. The Apache License doesn't cover the <Name> name or logo.
```

## Style

- Plain words, short sentences, second person. Explain what happens before how.
- Commands in fenced `bash` blocks, one command per block where people will copy them.
- Tables for anything with more than two attributes per row.
- Only real screenshots of the real app. No mock-ups, renders or generated images.
