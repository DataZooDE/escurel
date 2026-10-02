# The VS Code floor for a secondary-sidebar view container is 1.104

**Symptom.** The M4 spec puts the Runner view in the secondary sidebar. The extension declared
`engines.vscode: ^1.93.0`, and nothing said whether a `viewsContainers.secondarySidebar`
contribution was accepted that far back. Worse, `@types/vscode` was `^1.93.0` but resolved to
1.138.0 in the lockfile, so the typecheck would have accepted any API up to 1.138 under a claim of
supporting 1.93.

**Fix.** `engines.vscode` is `^1.104.0` and `@types/vscode` is pinned to exactly `1.104.0`, so the
typecheck now fails on an API newer than the floor. The whole integration suite was run on VS Code
1.104.0 (17 + 6 passing, no unhandled rejections).

**How the floor was found.** Stream each release's tarball and look for the extension point's own
description, `views containers to Secondary Side Bar`, in `resources/app/out/nls.messages.json`:

```sh
curl -sL "https://update.code.visualstudio.com/$V/linux-x64/stable" \
  | tar -xz --wildcards -O '*/resources/app/out/nls.messages.json' \
  | LC_ALL=C grep -ac 'iews containers to Secondary Side Bar'
```

1.103.2 has none, 1.104.3 has it.

**Recognise it next time.** Only PRESENCE is trustworthy. The string was absent from 1.120.2 and
1.130.1 although 1.110 and 1.139 have it (the file moves or changes shape between builds), and a
grep for `case"secondarySidebar"` in the minified `workbench.desktop.main.js` gave the same
non-monotonic answers. Bisect on a present/absent boundary and then confirm by running the suite on
the candidate version, which is what `ESCUREL_VSCODE_VERSION=1.104.0` is for.
