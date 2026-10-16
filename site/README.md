# protocol-droid documentation site

Astro + Starlight site for protocol-droid. Deployed to
[greysquirr3l.github.io/protocol-droid](https://greysquirr3l.github.io/protocol-droid).

## Develop

```
pnpm install
pnpm dev          # http://localhost:4321
```

## Build

```
pnpm build        # outputs to ./dist
pnpm preview      # serve dist locally
```

## Deploy

Manual: `pnpm build`, then `rsync` `./dist/` into a working clone of
`greysquirr3l/greysquirr3l.github.io` and push.
