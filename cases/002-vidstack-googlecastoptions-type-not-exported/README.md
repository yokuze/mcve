# Bug Repro: `vidstack/vue` types fail `vue-tsc` with TS4023 on `GoogleCastOptions`

## Problem

In
[`vidstack@1.12.13`](https://github.com/vidstack/player/tree/feb725702a23cba424d75c736aa2d74a601a92f0),
the `MediaPlayerProps.googleCast` property is typed as `GoogleCastOptions`, but
`GoogleCastOptions` is not re-exported from the `vidstack` package entry. When a Vue +
TypeScript project compiles `src/App.vue` with `composite: true`,
`"types": ["vidstack/vue"]`, and `vueCompilerOptions.fallthroughAttributes: true`,
`vue-tsc` fails with:

```text
error TS4023: Exported variable '__VLS_export' has or is using name 'GoogleCastOptions'
from external module ".../vidstack/types/vidstack-tX8MEPiY" but cannot be named.
```

Upstream issue: [vidstack/player#1757](https://github.com/vidstack/player/issues/1757).

## Root cause

`GoogleCastOptions` is declared in
[`packages/vidstack/src/providers/google-cast/types.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/providers/google-cast/types.ts#L1)
and imported from that internal path by
[`packages/vidstack/src/core/api/player-props.ts:5`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/core/api/player-props.ts#L5),
where it is used as the type of the public `MediaPlayerProps.googleCast` property at
[`player-props.ts:157`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/core/api/player-props.ts#L157).

The public barrel at
[`packages/vidstack/src/exports/providers.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L31-L34)
re-exports the Google Cast loader, provider, and events but omits the `types` file:

```ts
// Google Cast
export type { GoogleCastLoader } from '../providers/google-cast/loader';
export type { GoogleCastProvider } from '../providers/google-cast/provider';
export type * from '../providers/google-cast/events';
// missing: export type * from '../providers/google-cast/types';
```

HLS and DASH both re-export their `types` files at
[`providers.ts:22-24`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L22-L24)
and
[`providers.ts:27-29`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L27-L29).

When `composite: true` forces TypeScript to emit isolated declarations for every module,
it must inline a name for `MediaPlayerProps.googleCast`. It cannot, because
`GoogleCastOptions` has no public import path, so TypeScript raises TS4023.

## Expected behavior

`packages/vidstack/src/exports/providers.ts` should re-export the Google Cast types the
same way it does for HLS and DASH:

```ts
export type * from '../providers/google-cast/types';
```

After that change, `vue-tsc --noEmit` should complete with zero errors against this
project's `src/App.vue`.

## How to run

From this directory:

```text
npm install
bash ./run.sh
```

`run.sh` runs `vue-tsc --noEmit` and prints `BUG CONFIRMED: ...` on the last line when
it observes the expected TS4023 error, or `BUG NOT REPRODUCED: ...` otherwise.

## Relevant source files

All at tag `v1.12.13-next` (commit
[`feb7257`](https://github.com/vidstack/player/tree/feb725702a23cba424d75c736aa2d74a601a92f0))
of [vidstack/player](https://github.com/vidstack/player):

| File | Lines | What it contains |
| --- | --- | --- |
| [`packages/vidstack/src/providers/google-cast/types.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/providers/google-cast/types.ts#L1) | [1](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/providers/google-cast/types.ts#L1) | Declares `interface GoogleCastOptions extends Partial<cast.framework.CastOptions>` |
| [`packages/vidstack/src/core/api/player-props.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/core/api/player-props.ts#L5) | [5](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/core/api/player-props.ts#L5) | `import type { GoogleCastOptions } from '../../providers/google-cast/types'` — reaches into a non-exported path |
| [`packages/vidstack/src/core/api/player-props.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/core/api/player-props.ts#L157) | [157](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/core/api/player-props.ts#L157) | `googleCast: GoogleCastOptions` on the public `MediaPlayerProps` interface |
| [`packages/vidstack/src/exports/providers.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L31-L34) | [31–34](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L31-L34) | Google Cast barrel — loader, provider, events only; no `types` re-export |
| [`packages/vidstack/src/exports/providers.ts`](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L22-L29) | [22–29](https://github.com/vidstack/player/blob/feb725702a23cba424d75c736aa2d74a601a92f0/packages/vidstack/src/exports/providers.ts#L22-L29) | HLS and DASH barrels — both include `export type * from '../providers/<provider>/types'`, the pattern Google Cast is missing |
