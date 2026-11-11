# Brand

Wonderhoy Labs takes its name from Otori Emu's "Wonderhoy!", and every project is named for a character from
Project SEKAI COLORFUL STAGE! feat. Hatsune Miku.

## Names

A project's name is part of its character's name: the given name (Saki, Airi), the family name (Akiyama,
Otori, Shinonome), or the start of either (Shira from Shiraishi, Asahi from Asahina). Write it capitalised in
prose and UI ("Saki"), and lowercase only where it's an identifier (`saki`, `com.wonderhoylabs.saki`).

## Projects and colours

The colour is the character's image colour as [Sekaipedia](https://www.sekaipedia.org) gives it, in capitals.

| Project   | Character       | Unit                 | Image colour |
| --------- | --------------- | -------------------- | ------------ |
| Asahi     | Asahina Mafuyu  | 25-ji, Nightcord de. | `#8888CC`    |
| Airi      | Momoi Airi      | MORE MORE JUMP!      | `#FFAACC`    |
| Otori     | Otori Emu       | Wonderlands×Showtime | `#FF66BB`    |
| Shinonome | Shinonome Ena   | 25-ji, Nightcord de. | `#CCAA88`    |
| Shira     | Shiraishi An    | Vivid BAD SQUAD      | `#00BBDD`    |
| Akiyama   | Akiyama Mizuki  | 25-ji, Nightcord de. | `#DDAACC`    |
| Saki      | Tenma Saki      | Leo/need             | `#FFDD44`    |
| Minori    | Hanasato Minori | MORE MORE JUMP!      | `#FFCCAA`    |
| Template  | Hatsune Miku    | VIRTUAL SINGER       | `#33CCBB`    |

These names are taken. For a new project, pick another character, check the image colour on their Sekaipedia
page, and add a row here in the template.

## Where the brand shows

- **README badge**: `img.shields.io/badge/<Character>-<Unit>-<colour>`, linking to the credits. `pnpm unify`
  checks the exact markup.
- **Colours**: everything in the app comes from the image colour (see [design.md](design.md)).
- **Logo**: the maintainer's art for the project. Agents never make one.

## Credits

The README's "License and credits" section ends with this paragraph, filled in:

> <Name> is named for <Character> of <Unit>, from Project SEKAI COLORFUL STAGE! feat. Hatsune Miku. Wonderhoy Labs
> is a fan project, not affiliated with SEGA, Colorful Palette or Crypton Future Media, who own Project SEKAI and
> its characters. The Apache License doesn't cover the <Name> name or logo.
