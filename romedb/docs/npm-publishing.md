# Publishing to npm

Two packages are on npm in the `romedb` organization:

- `@romedb/node` holds the Linux x64 native addon and the Web-API entries
  `@romedb/node/client` and `@romedb/node/values`. It installs on every platform:
  the client entries run anywhere (Node, Workers, browsers), and the native entries
  fail with `RDB_NATIVE_LOAD` outside Linux x64.
- `@romedb/client` holds the code of those Web-API entries, in
  `romedb/bindings/client`. It has no native addon and no dependencies.

From 0.2.15, `@romedb/node` depends on `@romedb/client` at exactly its own version, and
its `client` and `values` entries pass that package through. So there is one set of
value classes, and the client must reach npm before `@romedb/node`.

In this repository, `romedb/bindings/node/pnpm-workspace.yaml` points that dependency at
the folder next to it. This is because a release is built and tested before its client
is on npm. The release checks install the client from the bundle in the same way.
`pnpm pack` leaves the version in `package.json` unchanged.

## How a release reaches npm

1. The release flow builds and qualifies `packages/romedb-node-VERSION.tgz` and
   `packages/romedb-client-VERSION.tgz`, and uploads both to the GitHub release, each
   with its `.sha256` file.
2. Publishing the GitHub release runs
   [`npm-publish.yml`](../../.github/workflows/npm-publish.yml). The workflow
   downloads both assets, checks their checksums, names, versions, repository and
   client dependency, and runs `npm publish` on each file, `@romedb/client` first.
   So each npm package and its release asset are the same file.
3. npm accepts the workflow through
   [trusted publishing](https://docs.npmjs.com/trusted-publishers): GitHub gives
   the workflow an identity (OIDC) that npm checks. There is no npm token in
   GitHub. Because the repository and the package are public, npm also records
   provenance: the workflow run and the commit that published the file. The file
   itself is built by the release flow, not by that workflow.

A version that is already on npm is skipped. So after a failure between the two
packages, a re-run publishes only the one that is missing. To retry, re-run the
workflow, or run it by hand with the release version as `version`.

## Configuration

These values must match exactly; npm compares them case by case.

| Where | Setting |
| --- | --- |
| npm, `@romedb/node` and `@romedb/client` → Settings → Trusted publishing | GitHub Actions; organization `romedb`, repository `romedb`, workflow `npm-publish.yml`, environment `Publish to npm`, allowed action `npm publish` |
| npm, `@romedb/node` and `@romedb/client` → Settings → Publishing access | Require two-factor authentication and disallow tokens |
| GitHub, `romedb/romedb` → Settings → Environments | `Publish to npm`, deployments only from `main` and tags `romedb-v*` |
| `romedb/bindings/node/package.json` and `romedb/bindings/client/package.json` | not `private`; `repository.url` is `git+https://github.com/romedb/romedb.git` |

## The first version

npm adds a trusted publisher only to a package that already exists, so the first
version of each package is published by hand from a maintainer's npm CLI.

### @romedb/client

0.2.14 is the first version, published by hand from a maintainer's npm CLI. Its code
is byte-identical to the client files in the 0.2.14 release asset of `@romedb/node`. The
npm file has SHA-256 `51b4c0390b2a521310b6611d948c37d1e0711fe52eb7e2e9e46fd92b54de1f0f`
and no provenance. The 0.2.14 GitHub release has no `romedb-client` asset. From 0.2.15
the workflow publishes each release.

To add the trusted publisher to a new package:

```sh
npm trust github @romedb/client --repository romedb/romedb --file npm-publish.yml --environment "Publish to npm" --allow-publish
```

### @romedb/node

0.2.12 was published by hand from a maintainer's npm CLI. It is the 0.2.12 release
asset (`romedb-node-0.2.12.tgz`, SHA-256 `5912edf3f8c00f5e18d8b73af2c3be2b4c4ac65ceb9bbb94f9b9709ccb8d1d59`) with two files changed for
npm: `package.json` (no `private`; adds `description`, `homepage`, `repository`
and `bugs`) and `README.md` (the npm install line). The other 20 files, including
`romedb.node`, are byte-identical to the asset. The npm file has SHA-256
`ae4bdab85ad65ac13791b515b7bd09661a522d6dc3988acfc2ad1ed4b795e1da` and no
provenance. Before publishing, it passed the release consumer check from a clean
install on Node 22.0.0 and 24.19.0. From 0.2.13 the workflow publishes each
release.
