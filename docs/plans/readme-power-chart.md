# README RAM chart

Status: planned 2026-10-25, implemented on `feat/macos-background-input`.

## Goal

A game-style RAM comparison for the README's "Speed and Memory" section, so the memory result reads at a glance. Request: for RAM usage, show how powerful we are against the other CLIs.

## Reading of the request

- RAM only. CPU stays in the table under the chart.
- Cursor CLI stays as a row, marked "not sampled (startup only)". We keep the Cursor row, and its footprint was never measured, so it gets no bar.
- The multipliers come from one footprint sample per CLI. The footer says so, and the table keeps the same numbers.

## Data

From `cli-speed-and-footprint.md`, macOS footprint: nca 6.4 MB, Grok CLI 59 MB, Claude Code 162 MB, Cursor CLI not sampled.

## Design

- 960 x 420 SVG in the hero's palette: near-black background, mint for nca, slate for the others.
- One shared scale: 520 px = 162 MB, so nca's bar is about 20 px long. That contrast is the point.
- Callout beside nca: "25× less RAM than Claude Code · 9× less than Grok CLI". Both multipliers are rounded down.
- Bars fill once on load, staggered. The full bars are the base state, which is also the reduced-motion state and what non-animating viewers show.

## Verification

- `xmllint --noout` on the output.
- Headless Chrome render. Bar lengths checked against the scale: 6.4 MB = 20.5 px, 59 MB = 189.4 px, 162 MB = 520 px.

## README

- The chart goes above the table in "Speed and Memory". Its alt text carries the four numbers.
