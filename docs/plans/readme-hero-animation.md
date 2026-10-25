# README hero animation

Status: planned 2026-10-25, implemented on `feat/macos-background-input`.

## Goal

Replace the static hero with an animated SVG in the spirit of Super Mario Run: a runner moves along a track and jumps obstacles. The obstacles are the issues we are working on for the next release, so the hero tells the same story as the README.

## Reading of the request

- The obstacles are the five open issues that already have a "will ship in the next release" comment: #25 streaming, #58 copy text, #63 stuck, #64 typing (Windows 11), #66 /rename.
- They are still open, so the animation says "tackled", not "fixed".
- The claude-seo hero was used only to confirm the technique (CSS keyframes inside an SVG). Nothing was copied, and its license is unknown.

## Design

- Canvas 960 x 320, shown at 720 px wide. Palette taken from the existing hero PNG: near-black sky, mint `#34e0b0`, amber `#f0a53a`.
- Runner: a terminal window with a `>` prompt and a blinking amber cursor, plus a two-frame leg cycle.
- Track: grid ground. Ridges and dots move at half speed for depth.
- Loop: 15 s. The track moves 160 px/s, so one loop covers 2400 px. The runner stays at x = 150.
- Five jumps, each 1.3 s in the air with a 120 px apex. Each jump is centred on the moment the runner is over its block: 1.5, 4.5, 7.5, 10.5 and 13.5 s.
- Clearance: the runner and an 84 px block overlap for 0.4 s either side of the hit. Over that window the jump is at least 74 px high, and the block is 64 px tall.
- When the runner is over a block, the amber block turns mint with a check mark, and the counter moves from `TACKLED 0/5` to `5/5`. The loop then restarts.
- Seam: each block has a copy one loop ahead, so it enters from the right and the wrap is invisible. Every passed block is off screen before the wrap.
- No script, no external fonts, no embedded images. `prefers-reduced-motion` stops the animation and shows the start frame, which is also the frame non-animating viewers show.

## Verification

- `xmllint --noout` on the output.
- The generator asserts the geometry: jumps stay inside the loop, clearance holds, and passed blocks are off screen at the wrap.
- Still frames at t = 0, 1.5, 8, 13.5 and 14.9 s, rendered with headless Chrome from copies with paused animations. Checked: runner height at each jump, flip timing, and the wrap.
- GitHub's own rendering can only be checked after a push.

## README changes

- Hero: the animated SVG replaces the static PNG. The PNG stays in `docs/images/`.
- New "Unreleased" section: desktop tools (preview), history and recordings in the data folder, and the context-window cache.
- New "Desktop Tools (macOS)" section under Harness and Tooling.
- Storage table: history, recordings, and the model-limits cache.
- "Speed and Memory" table with the measured numbers from `cli-speed-and-footprint.md`. The Cursor row stays (startup only, not logged in). Caveats stay next to the table.
- Internal design docs list: links to the three plan docs.

## Decisions

- The heading is "Unreleased" until we decide whether these changes ship in 0.5.0 or later.
- The generator stays out of the repo, in a scratch folder, because the project is Rust-native (AGENTS.md). The SVG is self-contained and does not need the generator to run.
- MCP HTTP lives on another branch, so the README does not mention it.

## Round 2: monsters (2026-10-25)

Request: make the obstacles monsters, not blocks.

- Each obstacle is a pixel monster: a 12 x 11 pixel grid at 5 px, so 60 x 55 px on screen, with horns, eyes, teeth and feet. Its issue number and name sit in the ground band under it.
- When the runner tackles one, the monster squashes to 45% of its height, turns mint, and gets a check mark. The counter logic is unchanged.
- Open monsters breathe with a short scale wobble, staggered by issue, so the track never looks frozen. Reduced motion stops the wobble and shows the start frame.
- Clearance is recomputed for the new width. The runner and a 60 px monster overlap for 0.325 s either side of the hit, and over that window the jump is at least 90 px high. The generator asserts this.
