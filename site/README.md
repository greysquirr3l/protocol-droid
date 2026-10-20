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

```
pnpm run deploy
```

The deploy script lives in [scripts/deploy-docs.sh](./scripts/deploy-docs.sh).
It reads its config from the `deploy-docs` field of `package.json` (so the
same script works across projects), builds the site, rsyncs `dist/` into the
configured subdirectory of the working gh-pages clone, commits, pushes, and
then polls the live URL until it serves the content just pushed — not merely
until it answers `200`, since a CDN still carrying the previous deploy answers
`200` immediately and would report success on a stale site.

Useful flags:

| flag             | purpose                                             |
| ---------------- | --------------------------------------------------- |
| `--rebuild`      | force a fresh build even if `dist/` is current      |
| `--no-push`      | preview: stage but neither commit nor push, so a later real run still deploys them |
| `--skip-build`   | deploy the existing `dist/` without rebuilding      |
| `--no-poll`      | skip the live-URL check after pushing               |
| `--message MSG`  | override the auto-generated commit message          |
| `--subdir NAME`  | override the target subdir in the pages repo        |
| `--pages-repo PATH` | override the path to the gh-pages clone          |

### First-time setup

The script expects a working clone of the gh-pages repo at the path
configured in `package.json` (`../greysquirr3l.github.io` by default). Clone
it if you haven't:

```
git clone git@github.com:greysquirr3l/greysquirr3l.github.io.git ../greysquirr3l.github.io
```

The script also drops a `.nojekyll` file into the pages repo on the first
run. This is required: GitHub Pages runs Jekyll by default, and Jekyll
silently skips directories that start with `_` — which is exactly where
Astro emits its bundled JS/CSS (the `_astro/` directory). Without
`.nojekyll` the page renders but the assets are missing.

### Pages-source conflict

GitHub Pages can only have one source per URL. If `greysquirr3l/protocol-droid`
has Pages enabled (with `build_type: workflow` and no deploy workflow
defined), its Pages config will shadow the user-pages repo and the deployed
site will 404 with "Site not found" — even though the files are in the repo
and the build is "built".

The deploy script detects this state on the fourth 404 attempt and tells
you to run:

```
gh api -X DELETE repos/greysquirr3l/protocol-droid/pages
```

This is reversible from the repo's Settings → Pages page.
