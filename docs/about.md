# About and open-source software

Open the app menu and choose **About**, or search for
**About**, **version**, **commit** or **licenses** in command search.

The **About** tab shows the running frontend's version and Git commit, including a modified
source marker for development builds. It uses the same component and bundled
content for local and remote projects. Build details are bundled with the app;
license notices load from a versioned static asset when their tab opens. Escape or Close dismisses it and restores focus.

The About view is centered in a compact dialog. Select the **Open-source software**
tab to load the license inventory into its own scroll area, then expand a library
or bundled asset to read its license text and copyright notices.
The inventory follows `Cargo.lock`, including direct, transitive, vendored,
platform-specific and build/test dependencies. Individual binaries use a subset.
It includes the original Monaspace font and Lucide icon notices. Where a crate
archive omits its notice, a version-pinned upstream copy is bundled when available;
entries without a full text say so and link to the upstream project.

Once the PWA has cached a build, the offline connection screen links to
**About**. Its standalone page uses the cached build's
version, commit, inventory and styles without requiring sign-in or an API request.
External project links still require network access.

## Building notices

Frontend builds require Python 3. The app and website share
`tools/open_source.py`; Cargo metadata and the resolved local crate distributions
supply the inventory. Notice generation never downloads upstream license texts.
Keep the exact-version entries in `third-party/notices/index.json` current when
updating dependencies that omit notices from their published archives.

Git source builds derive their commit automatically. Docker's build context
excludes `.git`; provide the revision when building an image:

```sh
docker build --build-arg OPENWEBIDE_BUILD_COMMIT="$(git rev-parse HEAD)" -t openwebide .
```

CI and release image builds supply the revision automatically. Source archives
and image builds without a supplied revision show that the commit is unknown.

The About dialog also includes a scrollable **Release notes** page. Notes for the
installed build are bundled with the app and remain readable offline. A **What’s
new** notice appears after login when the account has not acknowledged the current
version. Dismiss it or choose Release notes to save that acknowledgement in the
account’s database settings, shared across devices. The pane links to the release
on GitHub for downloads and the published notes.
