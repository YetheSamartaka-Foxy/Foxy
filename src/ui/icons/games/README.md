# Game logos and icons

Artwork embedded by `src/ui/game_logos.rs` and shown next to a game space in the
repository browser and the game space picker. These files identify which game a
space manages; they are not Foxy branding and must never be merged into Foxy's
own logo, icon, installer art, or store listings.

## Third-party trademark logos (use as shipped)

| File | Source | Owner |
| --- | --- | --- |
| `arma3_black.png`, `arma3_white.png` | `https://arma3.com/assets/downloads/press_assets/Logo.zip` (`Arma 3/Arma 3_logo_black.png`, `Arma 3_logo_white.png`) | Bohemia Interactive a.s. |
| `reforger_black.png`, `reforger_white.png` | `https://data.bistudio.com/press/arma/ArmaReforgerPressKit.zip` (`Logos/ARMA Reforger_logo black.png`, `... white.png`) | Bohemia Interactive a.s. |

The only change from the press kit files is a proportional downscale to 160 px
height after cropping transparent margins. Do not recolor, tint, outline,
rotate, stretch, crop the artwork itself, or place anything inside the clear
zone. The Arma 3 logo manual (`Arma3_logoManual_01_2023.pdf` in the same zip)
requires a minimum digital height of 60 px and a blank safe zone of one fifth of
the logo height on every side; `game_logos.rs` keeps the safe zone everywhere
and the 60 px height in the game space picker rows, and applies the same rules
to the Reforger logo. The repository sidebar header shows the logo as a smaller
badge beside the space name (`GAME_LOGO_BADGE_HEIGHT`). Pick the black or white variant by surface
darkness, never by palette color.

Bohemia Interactive's Game Content Usage Rules allow their trademarks and logos
"only as fair use" and require that nothing created with them "appear to be an
official product of Bohemia Interactive". Foxy is not affiliated with or
authorized by Bohemia Interactive a.s. Bohemia Interactive, ARMA, and all
associated logos and designs are trademarks or registered trademarks of Bohemia
Interactive a.s.

## Neutral icon (ours to style)

| File | Source | License |
| --- | --- | --- |
| `generic_white.png` | Lucide `gamepad-2` (`https://github.com/lucide-icons/lucide/blob/main/icons/gamepad-2.svg`), rasterized at 160 px with a white stroke | ISC, Copyright (c) Lucide Icons and Contributors |

Used for every game without its own cleared logo, including Total War: WARHAMMER
III and generic spaces. It is tinted with the palette text color at render time.

Do not use the Steam logo for this purpose. Valve's brand guidelines grant no
right to use Steam marks without a separate license, forbid using them as a
primary feature of non-Valve materials or combined with other elements, and the
logo indicates "a game is available and runs on Steam", which a generic space
does not assert. Steam Workshop features are referenced by name in text only.
